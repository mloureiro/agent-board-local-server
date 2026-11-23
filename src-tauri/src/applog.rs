use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::localtime::timestamp;
use crate::supervisor::process::{rotate_if_larger, LOG_LIMIT};

#[derive(Debug, Clone)]
pub struct LogFile {
    path: PathBuf,
    limit: u64,
}

impl LogFile {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            limit: LOG_LIMIT,
        }
    }

    /// A full disk or a missing folder must not take the app down through its own log.
    pub fn write(&self, line: &str) {
        let _ = self.append(line);
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent().filter(|dir| !dir.exists()) {
            fs::create_dir_all(dir)?;
        }
        rotate_if_larger(&self.path, self.limit)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{} {line}", timestamp())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looks_like_a_timestamp(text: &str) -> bool {
        let shape = "dddd-dd-dd dd:dd:dd";
        text.len() == shape.len()
            && text
                .chars()
                .zip(shape.chars())
                .all(|(got, want)| match want {
                    'd' => got.is_ascii_digit(),
                    other => got == other,
                })
    }

    #[test]
    fn a_line_is_appended_after_a_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let log = LogFile::new(dir.path().join("logs/app.log"));

        log.write("first");
        log.write("second");

        let text = fs::read_to_string(log.path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(looks_like_a_timestamp(&lines[0][..19]), "{}", lines[0]);
        assert!(lines[0].ends_with(" first"));
        assert!(lines[1].ends_with(" second"));
    }

    #[test]
    fn a_file_over_the_limit_is_rotated_and_the_new_line_lands_in_the_fresh_file() {
        let dir = tempfile::tempdir().unwrap();
        let log = LogFile {
            path: dir.path().join("app.log"),
            limit: 10,
        };
        log.write("a line longer than the limit");

        log.write("fresh");

        let current = fs::read_to_string(log.path()).unwrap();
        let previous = fs::read_to_string(dir.path().join("app.log.1")).unwrap();
        assert!(current.ends_with(" fresh\n") && current.lines().count() == 1);
        assert!(previous.contains("a line longer than the limit"));
    }

    #[test]
    fn a_path_that_cannot_be_opened_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "").unwrap();

        LogFile::new(blocker.join("app.log")).write("lost");
    }
}
