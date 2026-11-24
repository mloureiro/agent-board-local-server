use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::localtime::timestamp;
use crate::supervisor::process::{rotate_if_larger, LOG_LIMIT};

#[derive(Debug, Clone)]
pub struct LogFile {
    path: PathBuf,
    limit: u64,
    writing: Arc<Mutex<()>>,
}

impl LogFile {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            limit: LOG_LIMIT,
            writing: Arc::default(),
        }
    }

    /// A full disk or an unwritable folder must not take the app down through its own log.
    pub fn write(&self, line: &str) {
        let _ = self.append(line);
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        let _writing = self
            .writing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
            writing: Arc::default(),
        };
        log.write("a line longer than the limit");

        log.write("fresh");

        let current = fs::read_to_string(log.path()).unwrap();
        let previous = fs::read_to_string(dir.path().join("app.log.1")).unwrap();
        assert!(current.ends_with(" fresh\n") && current.lines().count() == 1);
        assert!(previous.contains("a line longer than the limit"));
    }

    #[test]
    fn clones_writing_at_once_lose_no_line_to_a_double_rotation() {
        const WRITERS: usize = 8;
        const LINES: usize = 60;
        let dir = tempfile::tempdir().unwrap();
        let log = LogFile {
            path: dir.path().join("app.log"),
            limit: 20_000,
            writing: Arc::default(),
        };

        let writers: Vec<_> = (0..WRITERS)
            .map(|_| {
                let log = log.clone();
                std::thread::spawn(move || {
                    for _ in 0..LINES {
                        log.write(&"x".repeat(60));
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }

        let kept = fs::read_to_string(log.path()).unwrap().lines().count()
            + fs::read_to_string(dir.path().join("app.log.1"))
                .unwrap()
                .lines()
                .count();
        assert_eq!(kept, WRITERS * LINES);
    }

    #[test]
    fn a_path_that_cannot_be_opened_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "").unwrap();

        LogFile::new(blocker.join("app.log")).write("lost");
    }
}
