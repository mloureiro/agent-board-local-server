use std::future::Future;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::worker::enrol::EnrolError;

const OSASCRIPT: &str = "/usr/bin/osascript";
const DIALOG_LIMIT: Duration = Duration::from_secs(15 * 60);
const TRY_AGAIN: &str = "Try again";

/// Hidden answers; the two values leave the dialogs on stdout only.
const CREDENTIALS_SCRIPT: &str = r#"on run
	set tokenAnswer to text returned of (display dialog "Machine token\n\nOpen the board, set up encryption, create a machine token in Settings, then paste it here." default answer "" with hidden answer with title "Connect worker" buttons {"Cancel", "Next"} default button "Next" cancel button "Cancel")
	set keyAnswer to text returned of (display dialog "Data key\n\nPaste the data key from the same Settings page." default answer "" with hidden answer with title "Connect worker" buttons {"Cancel", "Connect"} default button "Connect" cancel button "Cancel")
	return tokenAnswer & linefeed & keyAnswer
end run
"#;

/// The text shown is an argument of the script, never part of its source.
const RESULT_SCRIPT: &str = r#"on run argv
	set kind to item 1 of argv
	set detail to item 2 of argv
	if kind is "connected" then
		display dialog detail with title "Connect worker" buttons {"OK"} default button "OK" with icon note
		return "OK"
	end if
	set answer to button returned of (display alert "The worker was not connected" message detail as critical buttons {"Close", "Try again"} default button "Try again")
	return answer
end run
"#;

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    TryAgain,
    Done,
}

pub trait Dialogs: Send + Sync {
    fn ask(&self) -> impl Future<Output = Option<(String, String)>> + Send;
    fn report(&self, outcome: &Result<(), String>) -> impl Future<Output = Verdict> + Send;
}

pub trait Enroller: Send + Sync {
    fn enrol(
        &self,
        token: &str,
        data_key: &str,
    ) -> impl Future<Output = Result<(), EnrolError>> + Send;
}

/// Cancelling either dialog ends the flow with nothing stored.
pub async fn connect_worker<D: Dialogs, E: Enroller>(dialogs: &D, enroller: &E) {
    loop {
        let Some((token, data_key)) = dialogs.ask().await else {
            return;
        };
        let outcome = enroller
            .enrol(&token, &data_key)
            .await
            .map_err(|error| error.to_string());
        if dialogs.report(&outcome).await == Verdict::Done {
            return;
        }
    }
}

/// `osascript` prints the returned text followed by one line break; the break between the two answers is what
/// tells a missing second answer from an empty one.
pub fn parse_answers(stdout: &str) -> Option<(String, String)> {
    let body = stdout
        .strip_suffix("\r\n")
        .or_else(|| stdout.strip_suffix('\n'))
        .unwrap_or(stdout);
    let (token, data_key) = body.split_once('\n')?;
    Some((token.trim_end_matches('\r').to_owned(), data_key.to_owned()))
}

pub struct Osascript;

impl Dialogs for Osascript {
    async fn ask(&self) -> Option<(String, String)> {
        parse_answers(&run_script(CREDENTIALS_SCRIPT, &[]).await?)
    }

    async fn report(&self, outcome: &Result<(), String>) -> Verdict {
        let (kind, detail) = match outcome {
            Ok(()) => (
                "connected",
                "The worker is connected and runs your cards on this machine.",
            ),
            Err(message) => ("failed", message.as_str()),
        };
        match run_script(RESULT_SCRIPT, &[kind, detail]).await {
            Some(button) if button.trim() == TRY_AGAIN => Verdict::TryAgain,
            _ => Verdict::Done,
        }
    }
}

/// The script goes in on stdin so that neither it nor the answers appear in a process listing.
async fn run_script(script: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(OSASCRIPT)
        .arg("-")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| eprintln!("cannot run osascript: {error}"))
        .ok()?;
    let mut stdin = child.stdin.take()?;
    stdin.write_all(script.as_bytes()).await.ok()?;
    drop(stdin);
    let output = timeout(DIALOG_LIMIT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn two_answers_come_back_in_order_without_the_final_line_break() {
        assert_eq!(
            parse_answers("abm_one\nabk1_two\n"),
            Some(("abm_one".into(), "abk1_two".into()))
        );
    }

    #[test]
    fn a_missing_second_answer_is_not_an_answer_but_an_empty_one_is() {
        assert_eq!(parse_answers("abm_one\n"), None);
        assert_eq!(parse_answers(""), None);
        assert_eq!(
            parse_answers("abm_one\n\n"),
            Some(("abm_one".into(), String::new()))
        );
    }

    #[test]
    fn whitespace_around_the_answers_is_left_for_enrol_to_trim() {
        assert_eq!(
            parse_answers(" abm_one \r\n abk1_two  \r\n"),
            Some((" abm_one ".into(), " abk1_two  ".into()))
        );
    }

    #[test]
    fn an_extra_line_stays_inside_the_second_answer_where_enrol_refuses_it() {
        assert_eq!(
            parse_answers("abm_one\nabk1_two\nthird\n"),
            Some(("abm_one".into(), "abk1_two\nthird".into()))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn both_dialog_scripts_compile() {
        let dir = tempfile::tempdir().unwrap();
        for (name, script) in [
            ("credentials", CREDENTIALS_SCRIPT),
            ("result", RESULT_SCRIPT),
        ] {
            let source = dir.path().join(format!("{name}.applescript"));
            std::fs::write(&source, script).unwrap();
            let compiled = std::process::Command::new("/usr/bin/osacompile")
                .arg("-o")
                .arg(dir.path().join(format!("{name}.scpt")))
                .arg(&source)
                .output()
                .unwrap();
            assert!(
                compiled.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&compiled.stderr)
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_message_that_looks_like_script_source_reaches_the_script_as_plain_text() {
        let echo = "on run argv\n\treturn item 2 of argv\nend run\n";
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("injected");
        let hostile = [
            format!(r#"x" & (do shell script "touch {}") & ""#, marker.display()),
            r#"back\slash \n \" quote"#.to_owned(),
            "two\nlines".to_owned(),
        ];

        for message in &hostile {
            let shown = run_script(echo, &["failed", message]).await.unwrap();
            assert_eq!(shown.trim_end_matches('\n'), message);
        }
        assert!(!marker.exists());
    }

    struct Scripted {
        answers: Mutex<Vec<Option<(String, String)>>>,
        verdicts: Mutex<Vec<Verdict>>,
        reported: Mutex<Vec<Result<(), String>>>,
    }

    impl Dialogs for Scripted {
        async fn ask(&self) -> Option<(String, String)> {
            self.answers.lock().unwrap().remove(0)
        }

        async fn report(&self, outcome: &Result<(), String>) -> Verdict {
            self.reported.lock().unwrap().push(outcome.clone());
            self.verdicts.lock().unwrap().remove(0)
        }
    }

    struct Refuses(Mutex<Vec<String>>);

    impl Enroller for Refuses {
        async fn enrol(&self, token: &str, _data_key: &str) -> Result<(), EnrolError> {
            self.0.lock().unwrap().push(token.to_owned());
            if token == "good" {
                Ok(())
            } else {
                Err(EnrolError::new("server: the token was refused"))
            }
        }
    }

    fn scripted(answers: Vec<Option<(String, String)>>, verdicts: Vec<Verdict>) -> Scripted {
        Scripted {
            answers: Mutex::new(answers),
            verdicts: Mutex::new(verdicts),
            reported: Mutex::default(),
        }
    }

    fn pair(token: &str) -> Option<(String, String)> {
        Some((token.into(), "key".into()))
    }

    #[tokio::test]
    async fn cancelling_enrols_nothing_and_shows_nothing() {
        let dialogs = scripted(vec![None], vec![]);
        let enroller = Refuses(Mutex::default());

        connect_worker(&dialogs, &enroller).await;

        assert!(enroller.0.lock().unwrap().is_empty());
        assert!(dialogs.reported.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refusal_is_shown_and_try_again_asks_for_both_values_again() {
        let dialogs = scripted(
            vec![pair("wrong"), pair("good")],
            vec![Verdict::TryAgain, Verdict::Done],
        );
        let enroller = Refuses(Mutex::default());

        connect_worker(&dialogs, &enroller).await;

        assert_eq!(*enroller.0.lock().unwrap(), vec!["wrong", "good"]);
        assert_eq!(
            *dialogs.reported.lock().unwrap(),
            vec![Err("server: the token was refused".to_owned()), Ok(())]
        );
    }

    #[tokio::test]
    async fn closing_the_failure_alert_ends_the_flow() {
        let dialogs = scripted(vec![pair("wrong")], vec![Verdict::Done]);
        let enroller = Refuses(Mutex::default());

        connect_worker(&dialogs, &enroller).await;

        assert_eq!(enroller.0.lock().unwrap().len(), 1);
    }
}
