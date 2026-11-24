use std::fs;
use std::path::Path;

use agent_board_local_server::paths::APP_IDENTIFIER;
use agent_board_local_server::secrets::{Keychain, ROOT_SERVICE_SEPARATOR};
use agent_board_local_server::settings::DEFAULT_PORT;
use agent_board_local_server::worker::credentials::{
    PENDING_DATA_KEY_ACCOUNT, PENDING_HELPER_LINK, PENDING_TOKEN_ACCOUNT,
};

fn exported(source: &str, name: &str) -> String {
    let prefix = format!("export const {name} = ");
    source
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("scripts/app-constants.ts exports no {name}"))
        .trim_end_matches(';')
        .trim_matches('\'')
        .to_owned()
}

fn script_constants() -> String {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/app-constants.ts");
    fs::read_to_string(file).expect("scripts/app-constants.ts is readable")
}

#[test]
fn the_script_constants_match_the_ones_the_app_runs_with() {
    let source = script_constants();

    assert_eq!(exported(&source, "APP_IDENTIFIER"), APP_IDENTIFIER);
    assert_eq!(exported(&source, "DEFAULT_PORT"), DEFAULT_PORT.to_string());
    assert_eq!(
        exported(&source, "ROOT_SERVICE_SEPARATOR"),
        ROOT_SERVICE_SEPARATOR
    );
    assert_eq!(
        exported(&source, "PENDING_TOKEN_ACCOUNT"),
        PENDING_TOKEN_ACCOUNT
    );
    assert_eq!(
        exported(&source, "PENDING_DATA_KEY_ACCOUNT"),
        PENDING_DATA_KEY_ACCOUNT
    );
    assert_eq!(
        exported(&source, "PENDING_HELPER_LINK"),
        PENDING_HELPER_LINK
    );
}

#[test]
fn a_scratch_root_gets_the_service_the_smoke_harness_builds() {
    let source = script_constants();
    let root = tempfile::tempdir().unwrap();

    let service = Keychain::for_root(root.path()).service().to_owned();

    assert_eq!(
        service,
        format!(
            "{}{}{}",
            exported(&source, "APP_IDENTIFIER"),
            exported(&source, "ROOT_SERVICE_SEPARATOR"),
            root.path().display()
        )
    );
}

#[test]
fn the_bundle_identifier_is_the_one_the_app_derives_its_root_from() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json");
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(file).expect("tauri.conf.json is readable"))
            .expect("tauri.conf.json is JSON");

    assert_eq!(config["identifier"], APP_IDENTIFIER);
}
