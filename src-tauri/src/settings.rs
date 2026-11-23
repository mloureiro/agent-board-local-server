use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;

pub const DEFAULT_PORT: u16 = 4785;
const MIN_PORT: i64 = 1024;
const MAX_PORT: i64 = 65535;
const MAX_USER_NAME: usize = 64;
const MAX_WORKER_PATH: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub user_name: String,
    pub data_dir: PathBuf,
    pub port: u16,
    pub start_at_login: bool,
    pub worker_path: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawSettings {
    user_name: Option<String>,
    data_dir: Option<PathBuf>,
    port: Option<i64>,
    start_at_login: Option<bool>,
    worker_path: Option<Vec<String>>,
    backup_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub enum SettingsError {
    Unreadable(String),
    Malformed(String),
    Invalid { field: &'static str, reason: String },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(reason) => write!(f, "settings.json cannot be read: {reason}"),
            Self::Malformed(reason) => write!(f, "settings.json is not valid: {reason}"),
            Self::Invalid { field, reason } => write!(f, "settings.json: {field} {reason}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl Settings {
    pub fn defaults(paths: &AppPaths, login_name: &str) -> Self {
        Self {
            user_name: sanitize_user_name(login_name),
            data_dir: paths.default_data_dir(),
            port: DEFAULT_PORT,
            start_at_login: true,
            worker_path: Vec::new(),
            backup_dir: None,
        }
    }

    pub fn backups_dir(&self, paths: &AppPaths) -> PathBuf {
        self.backup_dir
            .clone()
            .unwrap_or_else(|| paths.default_backups_dir())
    }

    /// An existing file is only read: a typo must be fixed by its owner, never overwritten with defaults.
    pub fn load_or_create(paths: &AppPaths, login_name: &str) -> Result<Self, SettingsError> {
        let file = paths.settings_file();
        let defaults = Self::defaults(paths, login_name);
        match fs::read_to_string(&file) {
            Ok(text) => {
                let settings = Self::parse(&text, &defaults)?;
                ensure_dir(&settings.data_dir)?;
                Ok(settings)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                write_new(&file, &defaults)?;
                ensure_dir(&defaults.data_dir)?;
                Ok(defaults)
            }
            Err(error) => Err(SettingsError::Unreadable(error.to_string())),
        }
    }

    pub fn parse(text: &str, defaults: &Self) -> Result<Self, SettingsError> {
        let raw: RawSettings = serde_json::from_str(text)
            .map_err(|error| SettingsError::Malformed(error.to_string()))?;

        let user_name = match raw.user_name {
            Some(name) => validate_user_name(&name)?,
            None => defaults.user_name.clone(),
        };
        let data_dir = match raw.data_dir {
            Some(dir) => validate_data_dir(dir)?,
            None => defaults.data_dir.clone(),
        };
        let port = match raw.port {
            Some(port) => validate_port(port)?,
            None => defaults.port,
        };

        let worker_path = match raw.worker_path {
            Some(entries) => validate_worker_path(entries)?,
            None => defaults.worker_path.clone(),
        };

        let backup_dir = match raw.backup_dir {
            Some(dir) => Some(validate_backup_dir(dir, &data_dir)?),
            None => defaults.backup_dir.clone(),
        };

        Ok(Self {
            user_name,
            data_dir,
            port,
            start_at_login: raw.start_at_login.unwrap_or(defaults.start_at_login),
            worker_path,
            backup_dir,
        })
    }
}

fn validate_user_name(name: &str) -> Result<String, SettingsError> {
    let lowered = name.to_lowercase();
    let allowed =
        |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-');
    if lowered.is_empty() || lowered.len() > MAX_USER_NAME || !lowered.chars().all(allowed) {
        return Err(SettingsError::Invalid {
            field: "userName",
            reason: "must be 1 to 64 characters of a-z, 0-9, '.', '_' or '-'".into(),
        });
    }
    Ok(lowered)
}

fn validate_data_dir(dir: PathBuf) -> Result<PathBuf, SettingsError> {
    if !dir.is_absolute() {
        return Err(SettingsError::Invalid {
            field: "dataDir",
            reason: "must be an absolute path".into(),
        });
    }
    if inside_app_bundle(&dir) {
        return Err(SettingsError::Invalid {
            field: "dataDir",
            reason: "must not be inside an app bundle".into(),
        });
    }
    Ok(dir)
}

fn validate_backup_dir(dir: PathBuf, data_dir: &Path) -> Result<PathBuf, SettingsError> {
    let invalid = |reason: &str| SettingsError::Invalid {
        field: "backupDir",
        reason: reason.into(),
    };
    if !dir.is_absolute() {
        return Err(invalid("must be an absolute path"));
    }
    if inside_app_bundle(&dir) {
        return Err(invalid("must not be inside an app bundle"));
    }
    if dir.starts_with(data_dir) {
        return Err(invalid("must not be dataDir or inside it"));
    }
    Ok(dir)
}

fn validate_worker_path(entries: Vec<String>) -> Result<Vec<PathBuf>, SettingsError> {
    let invalid = |reason: &str| SettingsError::Invalid {
        field: "workerPath",
        reason: reason.into(),
    };
    if entries.len() > MAX_WORKER_PATH {
        return Err(invalid("must hold at most 32 entries"));
    }
    for entry in &entries {
        if !Path::new(entry).is_absolute() {
            return Err(invalid("entries must be absolute paths"));
        }
        if entry.contains(':') || entry.chars().any(char::is_control) {
            return Err(invalid(
                "entries must not contain ':' or a control character",
            ));
        }
    }
    Ok(entries.into_iter().map(PathBuf::from).collect())
}

fn validate_port(port: i64) -> Result<u16, SettingsError> {
    if !(MIN_PORT..=MAX_PORT).contains(&port) {
        return Err(SettingsError::Invalid {
            field: "port",
            reason: format!("must be between {MIN_PORT} and {MAX_PORT}"),
        });
    }
    Ok(u16::try_from(port).expect("range checked above"))
}

fn inside_app_bundle(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(name) => name.to_string_lossy().to_lowercase().ends_with(".app"),
        _ => false,
    })
}

fn sanitize_user_name(login_name: &str) -> String {
    let cleaned: String = login_name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(MAX_USER_NAME)
        .collect();
    if cleaned.is_empty() {
        "user".into()
    } else {
        cleaned
    }
}

fn ensure_dir(dir: &Path) -> Result<(), SettingsError> {
    fs::create_dir_all(dir).map_err(|error| {
        SettingsError::Unreadable(format!("{} cannot be created: {error}", dir.display()))
    })
}

fn write_new(file: &Path, settings: &Settings) -> Result<(), SettingsError> {
    let unreadable = |error: io::Error| SettingsError::Unreadable(error.to_string());
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(unreadable)?;
    }
    let mut text = serde_json::to_string_pretty(settings).expect("settings serialise");
    text.push('\n');
    let mut handle = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)
        .map_err(unreadable)?;
    handle.write_all(text.as_bytes()).map_err(unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Settings {
        Settings::defaults(&AppPaths::new("/root"), "Ines")
    }

    #[test]
    fn a_partial_file_takes_the_defaults_for_the_rest() {
        let parsed = Settings::parse(r#"{"port": 5000}"#, &defaults()).unwrap();
        assert_eq!(
            parsed,
            Settings {
                user_name: "ines".into(),
                data_dir: PathBuf::from("/root/data"),
                port: 5000,
                start_at_login: true,
                worker_path: Vec::new(),
                backup_dir: None,
            }
        );
    }

    #[test]
    fn invalid_files_are_refused() {
        let too_many = format!(r#"{{"workerPath": {:?}}}"#, vec!["/bin"; 33]);
        let cases = [
            (r#"{"port": 80}"#, "port"),
            (r#"{"port": 70000}"#, "port"),
            (r#"{"userName": ""}"#, "userName"),
            (r#"{"userName": "has space"}"#, "userName"),
            (r#"{"dataDir": "relative/dir"}"#, "dataDir"),
            (
                r#"{"dataDir": "/Applications/Agent Board Server.app/Contents/data"}"#,
                "dataDir",
            ),
            (r#"{"backupDir": "backups"}"#, "backupDir"),
            (
                r#"{"backupDir": "/Applications/Agent Board Server.app/Contents/backups"}"#,
                "backupDir",
            ),
            (r#"{"backupDir": "/root/data"}"#, "backupDir"),
            (r#"{"backupDir": "/root/data/backups"}"#, "backupDir"),
            (
                r#"{"dataDir": "/elsewhere/data", "backupDir": "/elsewhere/data/b"}"#,
                "backupDir",
            ),
            (r#"{"workerPath": ["bin"]}"#, "workerPath"),
            (r#"{"workerPath": ["/a:/b"]}"#, "workerPath"),
            (r#"{"workerPath": ["/a\nb"]}"#, "workerPath"),
            (r#"{"workerPath": [""]}"#, "workerPath"),
            (too_many.as_str(), "workerPath"),
        ];
        for (text, field) in cases {
            match Settings::parse(text, &defaults()) {
                Err(SettingsError::Invalid { field: got, .. }) => assert_eq!(got, field, "{text}"),
                other => panic!("{text} should be refused on {field}, got {other:?}"),
            }
        }
        for text in [r#"{"prot": 5000}"#, r#"{"port": "5000"}"#, "not json"] {
            assert!(
                matches!(
                    Settings::parse(text, &defaults()),
                    Err(SettingsError::Malformed(_))
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn a_backup_folder_beside_the_data_folder_is_accepted_and_the_default_is_under_the_root() {
        let paths = AppPaths::new("/root");
        let sibling =
            Settings::parse(r#"{"backupDir": "/root/data-backups"}"#, &defaults()).unwrap();

        assert_eq!(
            sibling.backups_dir(&paths),
            PathBuf::from("/root/data-backups")
        );
        assert_eq!(
            defaults().backups_dir(&paths),
            PathBuf::from("/root/backups")
        );
    }

    #[test]
    fn a_new_settings_file_does_not_name_the_backup_folder() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path());

        Settings::load_or_create(&paths, "Ines").unwrap();

        assert!(!fs::read_to_string(paths.settings_file())
            .unwrap()
            .contains("backupDir"));
    }

    #[test]
    fn worker_path_entries_are_kept_in_order_and_need_not_exist() {
        let at_the_limit = format!(r#"{{"workerPath": {:?}}}"#, vec!["/bin"; 32]);

        assert_eq!(
            Settings::parse(&at_the_limit, &defaults())
                .unwrap()
                .worker_path
                .len(),
            32
        );
        assert_eq!(
            Settings::parse(
                r#"{"workerPath": ["/no/such/dir", "/opt/x y"]}"#,
                &defaults()
            )
            .unwrap()
            .worker_path,
            [PathBuf::from("/no/such/dir"), PathBuf::from("/opt/x y")]
        );
        assert!(Settings::parse(r#"{"workerPath": []}"#, &defaults())
            .unwrap()
            .worker_path
            .is_empty());
    }

    #[test]
    fn a_missing_file_is_created_and_an_existing_one_is_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path());

        let created = Settings::load_or_create(&paths, "Ines").unwrap();
        assert_eq!(created, Settings::defaults(&paths, "Ines"));
        assert!(paths.default_data_dir().is_dir());

        fs::write(paths.settings_file(), r#"{"port": 5000, "oops": 1}"#).unwrap();
        assert!(Settings::load_or_create(&paths, "Ines").is_err());
        assert_eq!(
            fs::read_to_string(paths.settings_file()).unwrap(),
            r#"{"port": 5000, "oops": 1}"#
        );
    }
}
