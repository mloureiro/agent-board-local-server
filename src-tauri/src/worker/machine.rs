use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub fn read(config: &Path) -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(&fs::read(config).ok()?).ok()?;
    json.get("machine")?.as_str().map(str::to_owned)
}

pub fn set(config: &Path, machine: &str) -> io::Result<()> {
    let mut json: serde_json::Value = serde_json::from_slice(&fs::read(config)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let object = json.as_object_mut().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "config.json is not an object")
    })?;
    object.insert("machine".into(), machine.into());
    let mut text = serde_json::to_string_pretty(&json).expect("a JSON value serialises");
    text.push('\n');
    let mut temporary = config.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, text)?;
    fs::set_permissions(
        &temporary,
        fs::Permissions::from_mode(fs::metadata(config)?.permissions().mode() & 0o777),
    )?;
    fs::rename(&temporary, config)
}

fn backup_of(config: &Path) -> PathBuf {
    let mut backup = config.as_os_str().to_owned();
    backup.push(".pre-enrol");
    PathBuf::from(backup)
}

/// `board init` skips its check of the board when config.json already exists, so a token issued to another machine
/// name would be accepted and then refused on every claim; with the file set aside init checks first and names the machine.
pub struct Held {
    config: PathBuf,
    backup: Option<PathBuf>,
}

pub fn hold(config: &Path) -> io::Result<Held> {
    recover(config)?;
    let backup = match fs::symlink_metadata(config) {
        Ok(_) => {
            let backup = backup_of(config);
            fs::rename(config, &backup)?;
            Some(backup)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(Held {
        config: config.to_owned(),
        backup,
    })
}

impl Held {
    pub fn restore(self) -> io::Result<()> {
        match self.backup {
            Some(backup) => fs::rename(backup, &self.config),
            None => Ok(()),
        }
    }

    pub fn keep_with_board_name(self) -> io::Result<Option<String>> {
        let named = read(&self.config);
        match (self.backup, &named) {
            (Some(backup), Some(name)) => {
                set(&backup, name)?;
                fs::rename(backup, &self.config)?;
            }
            (Some(backup), None) => fs::rename(backup, &self.config)?,
            (None, _) => {}
        }
        Ok(named.or_else(|| read(&self.config)))
    }
}

pub fn recover(config: &Path) -> io::Result<()> {
    let backup = backup_of(config);
    if backup.exists() && fs::symlink_metadata(config).is_err() {
        fs::rename(backup, config)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_is_replaced_and_the_rest_of_the_file_stays() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        fs::write(&config, r#"{"machine":"mac","lanes":{"main":2}}"#).unwrap();

        set(&config, "tnk_one").unwrap();

        assert_eq!(read(&config).as_deref(), Some("tnk_one"));
        let kept: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        assert_eq!(kept["lanes"]["main"], 2);
        assert!(!dir.path().join("config.json.tmp").exists());
    }

    #[test]
    fn a_missing_or_unreadable_config_names_no_machine() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");

        assert_eq!(read(&config), None);
        fs::write(&config, "not json").unwrap();
        assert_eq!(read(&config), None);
        assert!(set(&config, "x").is_err());
    }

    #[test]
    fn a_held_config_comes_back_with_the_board_name_after_a_good_enrolment() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        fs::write(&config, r#"{"machine":"mac","lanes":{"main":2}}"#).unwrap();

        let held = hold(&config).unwrap();
        assert!(!config.exists());
        fs::write(&config, r#"{"machine":"tnk_one","lanes":{}}"#).unwrap();
        let name = held.keep_with_board_name().unwrap();

        assert_eq!(name.as_deref(), Some("tnk_one"));
        let kept: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        assert_eq!(kept["machine"], "tnk_one");
        assert_eq!(kept["lanes"]["main"], 2);
        assert!(!backup_of(&config).exists());
    }

    #[test]
    fn a_held_config_comes_back_untouched_after_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        fs::write(&config, r#"{"machine":"mac"}"#).unwrap();

        hold(&config).unwrap().restore().unwrap();

        assert_eq!(fs::read_to_string(&config).unwrap(), r#"{"machine":"mac"}"#);
        assert!(!backup_of(&config).exists());
    }

    #[test]
    fn a_first_enrolment_has_nothing_to_hold_and_reports_the_name_init_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");

        let held = hold(&config).unwrap();
        fs::write(&config, r#"{"machine":"tnk_one"}"#).unwrap();

        assert_eq!(
            held.keep_with_board_name().unwrap().as_deref(),
            Some("tnk_one")
        );
    }

    #[test]
    fn a_file_left_aside_by_an_enrolment_that_never_finished_is_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        fs::write(&config, r#"{"machine":"mac"}"#).unwrap();
        let held = hold(&config).unwrap();
        drop(held);

        recover(&config).unwrap();

        assert_eq!(fs::read_to_string(&config).unwrap(), r#"{"machine":"mac"}"#);
    }
}
