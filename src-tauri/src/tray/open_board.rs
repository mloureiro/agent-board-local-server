use std::future::Future;

pub const CLIENT_BUNDLE_ID: &str = "com.mloureiro.agent-board-client";
const OPEN: &str = "/usr/bin/open";

/// The one origin the app hands out; `localhost` would be a different origin with its own browser keys.
pub fn board_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

#[derive(Debug, PartialEq, Eq)]
pub enum Opened {
    DesktopClient,
    DefaultBrowser,
}

/// `open -b` exits 1 when no application has that bundle identifier (checked on macOS 26).
pub fn client_took_the_url(exit_code: Option<i32>) -> bool {
    exit_code == Some(0)
}

pub async fn open_with<F, Fut>(url: &str, run_open: F) -> Opened
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = Option<i32>>,
{
    let to_client = vec!["-b".into(), CLIENT_BUNDLE_ID.into(), url.to_owned()];
    if client_took_the_url(run_open(to_client).await) {
        return Opened::DesktopClient;
    }
    run_open(vec![url.to_owned()]).await;
    Opened::DefaultBrowser
}

pub async fn open_board(port: u16) -> Opened {
    open_with(&board_url(port), |args| async move {
        tokio::process::Command::new(OPEN)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .ok()
            .and_then(|status| status.code())
    })
    .await
}

pub async fn open_text(file: &std::path::Path) {
    open_path(&["-t"], file).await;
}

pub async fn open_folder(folder: &std::path::Path) {
    open_path(&[], folder).await;
}

async fn open_path(flags: &[&str], path: &std::path::Path) {
    let _ = tokio::process::Command::new(OPEN)
        .args(flags)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    async fn opened_with(client_exit: Option<i32>) -> (Opened, Vec<Vec<String>>) {
        let calls = Mutex::new(Vec::new());
        let opened = open_with("http://127.0.0.1:4785/", |args| {
            let first = calls.lock().unwrap().is_empty();
            calls.lock().unwrap().push(args);
            async move { first.then_some(client_exit).flatten() }
        })
        .await;
        (opened, calls.into_inner().unwrap())
    }

    #[tokio::test]
    async fn an_installed_client_gets_the_url_and_the_browser_is_left_alone() {
        let (opened, calls) = opened_with(Some(0)).await;

        assert_eq!(opened, Opened::DesktopClient);
        assert_eq!(
            calls,
            vec![vec![
                "-b".to_owned(),
                CLIENT_BUNDLE_ID.to_owned(),
                "http://127.0.0.1:4785/".to_owned()
            ]]
        );
    }

    #[tokio::test]
    async fn without_the_client_the_same_url_goes_to_the_default_browser() {
        for client_exit in [Some(1), None] {
            let (opened, calls) = opened_with(client_exit).await;

            assert_eq!(opened, Opened::DefaultBrowser);
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[1], vec!["http://127.0.0.1:4785/".to_owned()]);
        }
    }
}
