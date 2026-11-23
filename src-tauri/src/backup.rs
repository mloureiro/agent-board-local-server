use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::core_build::CoreBuild;
use crate::paths::{core_commit_file, pglite_dir};

pub const KEEP_MANUAL: usize = 5;
const PARTIAL: &str = ".partial";
const MANUAL_PREFIX: &str = "manual-";

#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Nothing,
    Record(String),
    BackUp(String),
    Unreadable,
}

pub fn plan(recorded: Option<&str>, bundled: Option<&str>, database_exists: bool) -> Plan {
    match (bundled, database_exists) {
        (Some(bundled), _) if recorded == Some(bundled) => Plan::Nothing,
        (Some(bundled), false) => Plan::Record(bundled.into()),
        (Some(bundled), true) => Plan::BackUp(bundled.into()),
        (None, true) => Plan::Unreadable,
        (None, false) => Plan::Nothing,
    }
}

pub fn manual_name(stamp: &str) -> String {
    format!("{MANUAL_PREFIX}{stamp}")
}

pub fn pre_upgrade_name(short_commit: &str, stamp: &str) -> String {
    format!("pre-upgrade-{short_commit}-{stamp}")
}

pub fn read_recorded(data_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(core_commit_file(data_dir)).ok()?;
    let commit = text.trim();
    (!commit.is_empty()).then(|| commit.to_owned())
}

pub fn record(data_dir: &Path, commit: &str) -> io::Result<()> {
    fs::write(core_commit_file(data_dir), format!("{commit}\n"))
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub fn back_up(data_dir: &Path, backups_dir: &Path, name: &str) -> io::Result<PathBuf> {
    let finished = backups_dir.join(name);
    let partial = backups_dir.join(format!("{name}{PARTIAL}"));
    if partial.exists() {
        fs::remove_dir_all(&partial)?;
    }
    let copied = copy_tree(&pglite_dir(data_dir), &pglite_dir(&partial))
        .and_then(|()| fs::rename(&partial, &finished));
    if copied.is_err() {
        let _ = fs::remove_dir_all(&partial);
    }
    copied.map(|()| finished)
}

pub fn prune_manual(backups_dir: &Path, keep: usize) -> io::Result<()> {
    let mut manual = Vec::new();
    for entry in fs::read_dir(backups_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(PARTIAL) {
            fs::remove_dir_all(entry.path())?;
        } else if name.starts_with(MANUAL_PREFIX) {
            manual.push(entry.path());
        }
    }
    manual.sort();
    let surplus = manual.len().saturating_sub(keep);
    for old in manual.into_iter().take(surplus) {
        fs::remove_dir_all(old)?;
    }
    Ok(())
}

pub fn manual(data_dir: &Path, backups_dir: &Path, stamp: &str) -> Result<PathBuf, String> {
    let written = back_up(data_dir, backups_dir, &manual_name(stamp))
        .map_err(|error| format!("cannot copy the database: {error}"))?;
    let _ = prune_manual(backups_dir, KEEP_MANUAL);
    Ok(written)
}

pub fn pre_upgrade(
    data_dir: &Path,
    backups_dir: &Path,
    bundled: Option<&CoreBuild>,
    stamp: &str,
) -> Result<Option<PathBuf>, String> {
    let commit = bundled.map(|build| build.commit.as_str());
    let recorded = read_recorded(data_dir);
    let database_exists = pglite_dir(data_dir).is_dir();
    let cannot_record = |error: io::Error| {
        format!(
            "cannot record the core commit in {}: {error}",
            data_dir.display()
        )
    };
    match plan(recorded.as_deref(), commit, database_exists) {
        Plan::Nothing => Ok(None),
        Plan::Record(commit) => record(data_dir, &commit)
            .map(|()| None)
            .map_err(cannot_record),
        Plan::BackUp(commit) => {
            let short = bundled.map_or("", CoreBuild::short_commit);
            let written = back_up(data_dir, backups_dir, &pre_upgrade_name(short, stamp))
                .map_err(|error| {
                    format!(
                        "the backup before the upgrade failed ({error}); the database is left as it is"
                    )
                })?;
            record(data_dir, &commit).map_err(cannot_record)?;
            Ok(Some(written))
        }
        Plan::Unreadable => Err(
            "the bundled core commit cannot be read, so the database cannot be backed up before the upgrade"
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn build(commit: &str) -> CoreBuild {
        CoreBuild {
            commit: commit.into(),
            built_at: "2026-11-20T08:15:00.000Z".into(),
        }
    }

    fn data_dir_with_database() -> tempfile::TempDir {
        let data = tempfile::tempdir().unwrap();
        let nested = pglite_dir(data.path()).join("base/1");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("page"), "rows").unwrap();
        fs::write(pglite_dir(data.path()).join("PG_VERSION"), "16").unwrap();
        data
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut found: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        found.sort();
        found
    }

    fn unwritable(dir: &Path) {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
    }

    fn writable(dir: &Path) {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn the_decision_follows_the_recorded_commit_the_bundled_one_and_the_database() {
        let new = || "bbbbbbb".to_owned();
        let cases = [
            (Some("bbbbbbb"), Some("bbbbbbb"), true, Plan::Nothing),
            (Some("bbbbbbb"), Some("bbbbbbb"), false, Plan::Nothing),
            (Some("aaaaaaa"), Some("bbbbbbb"), false, Plan::Record(new())),
            (None, Some("bbbbbbb"), false, Plan::Record(new())),
            (Some("aaaaaaa"), Some("bbbbbbb"), true, Plan::BackUp(new())),
            (None, Some("bbbbbbb"), true, Plan::BackUp(new())),
            (Some("aaaaaaa"), None, true, Plan::Unreadable),
            (None, None, true, Plan::Unreadable),
            (None, None, false, Plan::Nothing),
        ];
        for (recorded, bundled, database, expected) in cases {
            assert_eq!(
                plan(recorded, bundled, database),
                expected,
                "{recorded:?} {bundled:?} {database}"
            );
        }
    }

    #[test]
    fn a_backup_is_a_whole_copy_of_the_database_and_leaves_no_partial_directory() {
        let data = data_dir_with_database();
        let backups = tempfile::tempdir().unwrap();

        let written = back_up(data.path(), backups.path(), "manual-20261123-141502").unwrap();

        assert_eq!(written, backups.path().join("manual-20261123-141502"));
        assert_eq!(names(backups.path()), ["manual-20261123-141502"]);
        assert_eq!(
            fs::read_to_string(written.join("pglite/base/1/page")).unwrap(),
            "rows"
        );
        assert_eq!(
            fs::read_to_string(written.join("pglite/PG_VERSION")).unwrap(),
            "16"
        );
    }

    #[test]
    fn a_copy_into_a_folder_that_cannot_be_written_fails_and_leaves_no_finished_directory() {
        let data = data_dir_with_database();
        let backups = tempfile::tempdir().unwrap();
        unwritable(backups.path());

        let result = back_up(data.path(), backups.path(), "manual-1");
        writable(backups.path());

        assert!(result.is_err());
        assert!(names(backups.path())
            .iter()
            .all(|name| name.ends_with(PARTIAL)));
    }

    #[test]
    fn a_copy_that_breaks_half_way_removes_its_partial_directory() {
        let data = data_dir_with_database();
        let backups = tempfile::tempdir().unwrap();
        let unreadable = pglite_dir(data.path()).join("base/1");
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

        let result = back_up(data.path(), backups.path(), "manual-1");
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err());
        assert!(names(backups.path()).is_empty());
    }

    #[test]
    fn retention_keeps_the_newest_five_manual_backups_all_pre_upgrade_ones_and_drops_partials() {
        let backups = tempfile::tempdir().unwrap();
        for day in 1..=7 {
            fs::create_dir(backups.path().join(format!("manual-2026110{day}-100000"))).unwrap();
        }
        for name in [
            "pre-upgrade-aaaaaaa-20261001-100000",
            "pre-upgrade-bbbbbbb-20261101-100000",
            "manual-20261108-100000.partial",
        ] {
            fs::create_dir(backups.path().join(name)).unwrap();
        }

        prune_manual(backups.path(), KEEP_MANUAL).unwrap();

        assert_eq!(
            names(backups.path()),
            [
                "manual-20261103-100000",
                "manual-20261104-100000",
                "manual-20261105-100000",
                "manual-20261106-100000",
                "manual-20261107-100000",
                "pre-upgrade-aaaaaaa-20261001-100000",
                "pre-upgrade-bbbbbbb-20261101-100000",
            ]
        );
    }

    #[test]
    fn a_manual_backup_is_followed_by_retention() {
        let data = data_dir_with_database();
        let backups = tempfile::tempdir().unwrap();
        for day in 1..=5 {
            fs::create_dir(backups.path().join(format!("manual-2026110{day}-100000"))).unwrap();
        }

        manual(data.path(), backups.path(), "20261123-141502").unwrap();

        let kept = names(backups.path());
        assert_eq!(kept.len(), 5);
        assert!(!kept.contains(&"manual-20261101-100000".to_owned()));
        assert!(kept.contains(&"manual-20261123-141502".to_owned()));
    }

    #[test]
    fn the_commit_is_recorded_after_a_pre_upgrade_copy_and_not_after_a_failed_one() {
        let data = data_dir_with_database();
        record(data.path(), "aaaaaaa0").unwrap();
        let backups = tempfile::tempdir().unwrap();
        let bundled = build("bbbbbbb1234");

        unwritable(backups.path());
        let failed = pre_upgrade(
            data.path(),
            backups.path(),
            Some(&bundled),
            "20261123-090000",
        );
        writable(backups.path());

        assert!(failed.is_err());
        assert_eq!(read_recorded(data.path()).as_deref(), Some("aaaaaaa0"));

        let written = pre_upgrade(
            data.path(),
            backups.path(),
            Some(&bundled),
            "20261123-090001",
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            written,
            backups.path().join("pre-upgrade-bbbbbbb-20261123-090001")
        );
        assert_eq!(read_recorded(data.path()).as_deref(), Some("bbbbbbb1234"));
    }

    #[test]
    fn a_database_with_an_unreadable_bundled_commit_is_not_touched_and_a_new_one_is_recorded() {
        let backups = tempfile::tempdir().unwrap();
        let existing = data_dir_with_database();
        let fresh = tempfile::tempdir().unwrap();

        assert!(pre_upgrade(existing.path(), backups.path(), None, "x").is_err());
        assert!(names(backups.path()).is_empty());

        assert_eq!(
            pre_upgrade(fresh.path(), backups.path(), Some(&build("bbbbbbb")), "x"),
            Ok(None)
        );
        assert_eq!(read_recorded(fresh.path()).as_deref(), Some("bbbbbbb"));
    }
}
