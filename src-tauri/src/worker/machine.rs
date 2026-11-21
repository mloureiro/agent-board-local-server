use std::fs;
use std::path::Path;

pub fn read(config: &Path) -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(&fs::read(config).ok()?).ok()?;
    json.get("machine")?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_is_read_from_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        fs::write(&config, r#"{"machine":"tnk_one","lanes":{"main":2}}"#).unwrap();

        assert_eq!(read(&config).as_deref(), Some("tnk_one"));
    }

    #[test]
    fn a_missing_or_unreadable_config_names_no_machine() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");

        assert_eq!(read(&config), None);
        fs::write(&config, "not json").unwrap();
        assert_eq!(read(&config), None);
    }
}
