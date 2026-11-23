use std::path::Path;

use serde::Deserialize;

const SHORT_COMMIT: usize = 7;
const DATE_LENGTH: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreBuild {
    pub commit: String,
    pub built_at: String,
}

impl CoreBuild {
    pub fn read(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    pub fn short_commit(&self) -> &str {
        self.commit.get(..SHORT_COMMIT).unwrap_or(&self.commit)
    }

    pub fn built_date(&self) -> &str {
        self.built_at.get(..DATE_LENGTH).unwrap_or(&self.built_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(contents: Option<&str>) -> Option<CoreBuild> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("core-build.json");
        if let Some(contents) = contents {
            std::fs::write(&path, contents).unwrap();
        }
        CoreBuild::read(&path)
    }

    #[test]
    fn a_manifest_gives_its_commit_and_build_time_and_ignores_the_other_fields() {
        let build = read(Some(
            r#"{"commit":"0123456789abcdef","builtAt":"2026-11-20T08:15:00.000Z","bun":"1.2.18","recipe":"r","files":{}}"#,
        ))
        .unwrap();

        assert_eq!(build.short_commit(), "0123456");
        assert_eq!(build.built_date(), "2026-11-20");
    }

    #[test]
    fn a_manifest_from_before_the_build_time_was_recorded_is_unreadable() {
        assert_eq!(
            read(Some(
                r#"{"commit":"0123456789abcdef","bun":"1","recipe":"r","files":{}}"#
            )),
            None
        );
    }

    #[test]
    fn a_missing_file_is_unreadable() {
        assert_eq!(read(None), None);
    }
}
