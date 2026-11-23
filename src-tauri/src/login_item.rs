use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::APP_IDENTIFIER;

const PLUGIN_PLIST: &str = "Agent Board Server.plist";

pub fn launch_agents_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/LaunchAgents"))
}

pub fn plist_file(dir: &Path) -> PathBuf {
    dir.join(format!("{APP_IDENTIFIER}.plist"))
}

pub fn plugin_plist_file(dir: &Path) -> PathBuf {
    dir.join(PLUGIN_PLIST)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `KeepAlive` on an unsuccessful exit relaunches a crash or a force quit; Quit, a log-out SIGTERM and a second
/// launch that finds the lock all exit 0 and stay down.
pub fn plist_text(executable: &Path) -> String {
    let executable = escape(&executable.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{APP_IDENTIFIER}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{executable}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
</dict>
</plist>
"#
    )
}

fn remove_if_present(file: &Path) -> io::Result<()> {
    match fs::remove_file(file) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// No `launchctl` call: a loaded job is the running app, and the file takes effect at the next login.
pub fn enable(dir: &Path, executable: &Path) -> io::Result<()> {
    let text = plist_text(executable);
    let file = plist_file(dir);
    if fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
        fs::create_dir_all(dir)?;
        fs::write(&file, text)?;
    }
    remove_if_present(&plugin_plist_file(dir))
}

pub fn disable(dir: &Path) -> io::Result<()> {
    let own = remove_if_present(&plist_file(dir));
    let plugin = remove_if_present(&plugin_plist_file(dir));
    own.and(plugin)
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_plist_escapes_the_executable_and_relaunches_only_after_an_unsuccessful_exit() {
        let text = plist_text(Path::new(
            "/Apps/Board & <Co>/Agent Board.app/Contents/MacOS/app",
        ));

        assert!(text.contains(
            "<string>/Apps/Board &amp; &lt;Co&gt;/Agent Board.app/Contents/MacOS/app</string>"
        ));
        assert!(text.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(text.contains(&format!(
            "<key>Label</key>\n\t<string>{APP_IDENTIFIER}</string>"
        )));
    }

    #[test]
    fn enable_writes_the_agent_removes_the_plugins_file_and_leaves_an_identical_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = plugin_plist_file(dir.path());
        fs::write(&plugin, "old").unwrap();
        let exe = Path::new("/Apps/Agent Board.app/Contents/MacOS/app");

        enable(dir.path(), exe).unwrap();

        let own = plist_file(dir.path());
        assert_eq!(fs::read_to_string(&own).unwrap(), plist_text(exe));
        assert!(!plugin.exists());
        let first = fs::metadata(&own).unwrap().modified().unwrap();
        sleep(Duration::from_millis(20));
        enable(dir.path(), exe).unwrap();
        assert_eq!(fs::metadata(&own).unwrap().modified().unwrap(), first);
    }

    #[test]
    fn enable_replaces_a_file_for_another_executable() {
        let dir = tempfile::tempdir().unwrap();
        enable(dir.path(), Path::new("/old/app")).unwrap();
        enable(dir.path(), Path::new("/new/app")).unwrap();

        assert!(fs::read_to_string(plist_file(dir.path()))
            .unwrap()
            .contains("/new/app"));
    }

    #[test]
    fn disable_removes_both_files_and_succeeds_when_neither_exists() {
        let dir = tempfile::tempdir().unwrap();
        disable(dir.path()).unwrap();
        enable(dir.path(), Path::new("/app")).unwrap();
        fs::write(plugin_plist_file(dir.path()), "old").unwrap();

        disable(dir.path()).unwrap();

        assert!(!plist_file(dir.path()).exists());
        assert!(!plugin_plist_file(dir.path()).exists());
    }
}
