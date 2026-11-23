use std::fs;
use std::path::Path;

use agent_board_local_server::paths::APP_IDENTIFIER;
use agent_board_local_server::settings::DEFAULT_PORT;

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

#[test]
fn the_script_constants_match_the_ones_the_app_runs_with() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/app-constants.ts");
    let source = fs::read_to_string(file).expect("scripts/app-constants.ts is readable");

    assert_eq!(exported(&source, "APP_IDENTIFIER"), APP_IDENTIFIER);
    assert_eq!(exported(&source, "DEFAULT_PORT"), DEFAULT_PORT.to_string());
}
