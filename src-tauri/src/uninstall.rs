use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::lock::{self, LockError};
use crate::login_item;
use crate::paths::{has_parent_component, identity, is_within, resolve, AppPaths, APP_IDENTIFIER};
use crate::secrets::{Keychain, SecretStore, ACCOUNTS};
use crate::settings::Settings;

pub const USAGE: &str = "usage: --uninstall [--delete-data]";
pub const QUIT_FIRST: &str = "quit the app first";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub delete_data: bool,
}

pub fn parse_args(args: &[OsString]) -> Option<Result<Options, &'static str>> {
    if !args.iter().any(|arg| arg == "--uninstall") {
        return None;
    }
    let delete_data = args.iter().any(|arg| arg == "--delete-data");
    let expected = 1 + usize::from(delete_data);
    if args.len() == expected && args[0] == "--uninstall" {
        return Some(Ok(Options { delete_data }));
    }
    Some(Err(USAGE))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Refuse(&'static str),
    BootOut,
    RemoveLoginItem,
    DeleteKeychainItems,
    RemoveAppFolder(PathBuf),
    Note(String),
}

pub struct Inputs<'a> {
    pub root: &'a Path,
    pub default_root: bool,
    pub running: bool,
    pub delete_data: bool,
    pub settings: Option<&'a Settings>,
    pub home: Option<&'a Path>,
}

fn mirrors_home(root: &Path, home: &Path) -> bool {
    let home = resolve(home);
    home.ancestors().any(|ancestor| {
        let Ok(relative) = ancestor.strip_prefix("/") else {
            return false;
        };
        identity(ancestor).is_some() && identity(&root.join(relative)) == identity(ancestor)
    })
}

pub fn deletable(root: &Path, home: Option<&Path>) -> bool {
    let bare: PathBuf = root.components().collect();
    let is_link = fs::symlink_metadata(&bare).is_ok_and(|meta| meta.is_symlink());
    let resolved = resolve(root);
    !has_parent_component(root)
        && !is_link
        && resolved.parent().is_some()
        && home.is_none_or(|home| !is_within(home, root) && !mirrors_home(root, home))
}

fn has_marker(root: &Path) -> bool {
    fs::symlink_metadata(AppPaths::new(root).settings_file()).is_ok_and(|meta| meta.is_file())
}

fn refusal(root: &Path, home: Option<&Path>) -> Option<String> {
    if !deletable(root, home) {
        return Some(format!(
            "{} is not removed: it is a symlink, has a .. component, holds the home folder or is a filesystem root",
            root.display()
        ));
    }
    let missing = fs::symlink_metadata(root).is_err_and(|e| e.kind() == io::ErrorKind::NotFound);
    if !missing && !has_marker(root) {
        return Some(format!(
            "{} is not removed: it does not look like this app's folder (no settings.json)",
            root.display()
        ));
    }
    None
}

pub fn plan(inputs: &Inputs) -> Vec<Step> {
    if inputs.running {
        return vec![Step::Refuse(QUIT_FIRST)];
    }
    let mut steps = Vec::new();
    if inputs.default_root {
        steps.push(Step::BootOut);
        steps.push(Step::RemoveLoginItem);
    } else {
        steps.push(Step::Note(
            "login item left alone: the root is not the default one".into(),
        ));
    }
    steps.push(Step::DeleteKeychainItems);
    if !inputs.delete_data {
        return steps;
    }
    if let Some(reason) = refusal(inputs.root, inputs.home) {
        steps.push(Step::Note(reason));
        return steps;
    }
    match inputs.settings {
        Some(settings) => {
            let paths = AppPaths::new(inputs.root);
            let outside = [
                ("dataDir", settings.data_dir.clone()),
                ("backupDir", settings.backups_dir(&paths)),
            ];
            for (name, dir) in outside {
                if !is_within(&dir, inputs.root) {
                    steps.push(Step::Note(format!(
                        "{name} {} is outside the app folder and is left in place",
                        dir.display()
                    )));
                }
            }
        }
        None => steps.push(Step::Note(
            "settings.json cannot be read, so a dataDir or backupDir outside the app folder was not checked".into(),
        )),
    }
    steps.push(Step::RemoveAppFolder(inputs.root.to_path_buf()));
    steps
}

pub struct Env<'a> {
    pub launch_agents: &'a Path,
    pub store: &'a dyn SecretStore,
    pub boot_out: &'a dyn Fn(),
}

pub struct Report {
    pub lines: Vec<String>,
    pub ok: bool,
}

pub fn execute(steps: &[Step], env: &Env) -> Report {
    let mut report = Report {
        lines: Vec::new(),
        ok: true,
    };
    for step in steps {
        match step {
            Step::Refuse(reason) => {
                report.lines.push((*reason).to_owned());
                report.ok = false;
            }
            Step::BootOut => (env.boot_out)(),
            Step::RemoveLoginItem => match login_item::disable(env.launch_agents) {
                Ok(()) => report.lines.push("login item removed".into()),
                Err(error) => fail(&mut report, format!("login item not removed: {error}")),
            },
            Step::DeleteKeychainItems => {
                let failed: Vec<String> = ACCOUNTS
                    .iter()
                    .filter_map(|account| {
                        env.store
                            .delete(account)
                            .err()
                            .map(|error| format!("{account}: {error}"))
                    })
                    .collect();
                if failed.is_empty() {
                    report.lines.push("Keychain items deleted".into());
                } else {
                    fail(
                        &mut report,
                        format!("Keychain items not deleted: {}", failed.join("; ")),
                    );
                }
            }
            Step::RemoveAppFolder(folder) => match remove_folder(folder) {
                Ok(true) => report.lines.push(format!("{} removed", folder.display())),
                Ok(false) => report
                    .lines
                    .push(format!("{} was not there", folder.display())),
                Err(error) => fail(
                    &mut report,
                    format!("{} not removed: {error}", folder.display()),
                ),
            },
            Step::Note(text) => report.lines.push(text.clone()),
        }
    }
    report
}

fn fail(report: &mut Report, line: String) {
    report.lines.push(line);
    report.ok = false;
}

fn remove_folder(folder: &Path) -> io::Result<bool> {
    match fs::remove_dir_all(folder) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn read_settings(paths: &AppPaths) -> Option<Settings> {
    let defaults = Settings::defaults(paths, "user");
    match fs::read_to_string(paths.settings_file()) {
        Ok(text) => Settings::parse(&text, &defaults).ok(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(defaults),
        Err(_) => None,
    }
}

fn boot_out_login_item() {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let _ = Command::new("/bin/launchctl")
        .arg("bootout")
        .arg(format!("gui/{uid}/{APP_IDENTIFIER}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn hold_lock(
    paths: &AppPaths,
    root: &Path,
    delete_data: bool,
    home: Option<&Path>,
) -> Result<(bool, Option<lock::Lock>), io::Error> {
    let refused = delete_data && refusal(root, home).is_some();
    if refused || !root.exists() {
        return Ok((false, None));
    }
    match lock::acquire(&paths.lock_file()) {
        Ok(held) => Ok((false, Some(held))),
        Err(LockError::AlreadyRunning) => Ok((true, None)),
        Err(LockError::Io(error)) => Err(error),
    }
}

pub fn run(options: Options) -> i32 {
    let Some(root) = AppPaths::configured_root() else {
        eprintln!("HOME is not set; the app root cannot be found");
        return 1;
    };
    run_at(&root, options)
}

pub fn run_at(root: &Path, options: Options) -> i32 {
    let root = root.to_path_buf();
    let Some(launch_agents) = login_item::launch_agents_dir() else {
        eprintln!("HOME is not set; the login item cannot be found");
        return 1;
    };
    let paths = AppPaths::new(&root);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let held = hold_lock(&paths, &root, options.delete_data, home.as_deref());
    let (running, _lock) = match held {
        Ok(held) => held,
        Err(error) => {
            eprintln!("cannot take the app lock: {error}");
            return 1;
        }
    };
    let settings = options.delete_data.then(|| read_settings(&paths)).flatten();
    let steps = plan(&Inputs {
        root: &root,
        default_root: AppPaths::default_root().as_deref() == Some(root.as_path()),
        running,
        delete_data: options.delete_data,
        settings: settings.as_ref(),
        home: home.as_deref(),
    });
    let keychain = Keychain::for_root(&root);
    let report = execute(
        &steps,
        &Env {
            launch_agents: &launch_agents,
            store: &keychain,
            boot_out: &boot_out_login_item,
        },
    );
    for line in &report.lines {
        println!("{line}");
    }
    i32::from(!report.ok)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use crate::secrets::MemoryStore;

    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn inputs<'a>(root: &'a Path, settings: Option<&'a Settings>) -> Inputs<'a> {
        Inputs {
            root,
            default_root: true,
            running: false,
            delete_data: false,
            settings,
            home: Some(Path::new("/Users/someone")),
        }
    }

    fn settings_for(root: &Path, data_dir: &Path, backup_dir: Option<&Path>) -> Settings {
        let mut settings = Settings::defaults(&AppPaths::new(root), "user");
        settings.data_dir = data_dir.to_path_buf();
        settings.backup_dir = backup_dir.map(Path::to_path_buf);
        settings
    }

    #[test]
    fn only_uninstall_with_an_optional_delete_data_is_accepted() {
        assert_eq!(parse_args(&args(&["--worker-x"])), None);
        assert_eq!(
            parse_args(&args(&["--uninstall"])),
            Some(Ok(Options { delete_data: false }))
        );
        assert_eq!(
            parse_args(&args(&["--uninstall", "--delete-data"])),
            Some(Ok(Options { delete_data: true }))
        );
        for bad in [
            &["--uninstall", "--other"][..],
            &["--uninstall", "--delete-data", "--delete-data"],
            &["--delete-data", "--uninstall"],
            &["--uninstall", "--uninstall"],
        ] {
            assert_eq!(parse_args(&args(bad)), Some(Err(USAGE)), "{bad:?}");
        }
    }

    #[test]
    fn a_running_app_gets_the_quit_first_message_and_nothing_else() {
        let root = Path::new("/tmp/root");
        let mut held = inputs(root, None);
        held.running = true;
        held.delete_data = true;

        assert_eq!(plan(&held), vec![Step::Refuse(QUIT_FIRST)]);
    }

    #[test]
    fn the_login_item_steps_exist_for_the_default_root_only() {
        let default = plan(&inputs(Path::new("/tmp/root"), None));
        assert_eq!(
            default,
            vec![
                Step::BootOut,
                Step::RemoveLoginItem,
                Step::DeleteKeychainItems
            ]
        );

        let mut other = inputs(Path::new("/tmp/root"), None);
        other.default_root = false;
        let steps = plan(&other);
        assert!(!steps.contains(&Step::BootOut));
        assert!(!steps.contains(&Step::RemoveLoginItem));
        assert!(steps.contains(&Step::DeleteKeychainItems));
    }

    #[test]
    fn the_app_folder_is_in_the_plan_only_with_delete_data() {
        let root = PathBuf::from("/tmp/uninstall-plan-root");
        let settings = settings_for(&root, &root.join("data"), None);
        let mut wanted = inputs(&root, Some(&settings));
        assert!(!plan(&wanted)
            .iter()
            .any(|step| matches!(step, Step::RemoveAppFolder(_))));

        wanted.delete_data = true;
        assert_eq!(
            plan(&wanted).last(),
            Some(&Step::RemoveAppFolder(root.clone()))
        );
    }

    #[test]
    fn a_data_or_backup_dir_outside_the_folder_is_printed_and_never_removed() {
        let root = PathBuf::from("/tmp/uninstall-plan-root");
        let inside = settings_for(&root, &root.join("elsewhere"), Some(&root.join("b")));
        let mut wanted = inputs(&root, Some(&inside));
        wanted.delete_data = true;
        assert!(!plan(&wanted)
            .iter()
            .any(|s| matches!(s, Step::Note(t) if t.contains("outside"))));

        let outside = settings_for(
            &root,
            Path::new("/tmp/uninstall-plan-data"),
            Some(Path::new("/tmp/uninstall-plan-backups")),
        );
        wanted.settings = Some(&outside);
        let steps = plan(&wanted);
        let notes: Vec<&String> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Note(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(notes.len(), 2);
        assert!(notes[0].starts_with("dataDir /tmp/uninstall-plan-data"));
        assert!(notes[1].starts_with("backupDir /tmp/uninstall-plan-backups"));
        assert_eq!(steps.last(), Some(&Step::RemoveAppFolder(root)));
    }

    #[test]
    fn unreadable_settings_say_so_and_the_folder_is_still_the_only_removal() {
        let root = PathBuf::from("/tmp/uninstall-plan-root");
        let mut wanted = inputs(&root, None);
        wanted.delete_data = true;

        let steps = plan(&wanted);

        assert!(steps
            .iter()
            .any(|s| matches!(s, Step::Note(t) if t.contains("cannot be read"))));
        assert_eq!(steps.last(), Some(&Step::RemoveAppFolder(root)));
    }

    #[test]
    fn a_root_that_holds_the_home_folder_is_never_removed() {
        for root in ["/Users/someone", "/Users", "/"] {
            let mut wanted = inputs(Path::new(root), None);
            wanted.delete_data = true;
            assert!(
                !plan(&wanted)
                    .iter()
                    .any(|step| matches!(step, Step::RemoveAppFolder(_))),
                "{root}"
            );
        }
    }

    fn removes_folder(root: &Path, home: &Path) -> bool {
        if root.is_dir() && !root.is_symlink() {
            let _ = fs::write(AppPaths::new(root).settings_file(), "{}");
        }
        let mut wanted = inputs(root, None);
        wanted.home = Some(home);
        wanted.delete_data = true;
        plan(&wanted)
            .iter()
            .any(|step| matches!(step, Step::RemoveAppFolder(_)))
    }

    #[test]
    fn a_root_is_judged_on_its_resolved_path_against_the_resolved_home() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let root = home.join("app");
        fs::create_dir_all(&root).unwrap();
        let alias = scratch.path().join("alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        let link_to_app = scratch.path().join("app-link");
        std::os::unix::fs::symlink(&root, &link_to_app).unwrap();
        let elsewhere = scratch.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();

        assert!(removes_folder(&elsewhere, &home));
        assert!(removes_folder(&root, &home));
        assert!(!removes_folder(&home.join("app/.."), &home));
        assert!(!removes_folder(&home.join("x/.."), &home));
        assert!(!removes_folder(&alias, &home));
        assert!(!removes_folder(&alias.join(""), &home));
        let mut slashed = alias.into_os_string();
        slashed.push("/");
        assert!(!removes_folder(Path::new(&slashed), &home));
        assert!(!removes_folder(&link_to_app, &home));
        assert!(!removes_folder(&home, &home));
        assert!(!removes_folder(scratch.path(), &home));
        assert!(!removes_folder(Path::new("/"), &home));
    }

    #[test]
    fn a_folder_that_was_not_there_is_not_reported_as_removed() {
        let scratch = tempfile::tempdir().unwrap();
        let agents = scratch.path().join("LaunchAgents");
        let store = MemoryStore::default();
        let env = Env {
            launch_agents: &agents,
            store: &store,
            boot_out: &|| {},
        };
        let missing = scratch.path().join("gone");

        let report = execute(&[Step::RemoveAppFolder(missing.clone())], &env);

        assert!(report.ok);
        assert_eq!(
            report.lines,
            vec![format!("{} was not there", missing.display())]
        );
    }

    #[test]
    fn carrying_out_the_plan_clears_the_items_and_the_folder_only_with_delete_data() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("app");
        fs::create_dir_all(root.join("data")).unwrap();
        fs::write(root.join("data/file"), "x").unwrap();
        fs::write(AppPaths::new(&root).settings_file(), "{}").unwrap();
        let outside = scratch.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let agents = scratch.path().join("LaunchAgents");
        login_item::enable(&agents, Path::new("/app")).unwrap();
        fs::write(login_item::plugin_plist_file(&agents), "old").unwrap();
        let store = MemoryStore::default();
        for account in ACCOUNTS {
            store.set(account, "secret").unwrap();
        }
        store.set("unrelated", "kept").unwrap();
        let booted = Cell::new(0);
        let boot_out = || booted.set(booted.get() + 1);
        let env = Env {
            launch_agents: &agents,
            store: &store,
            boot_out: &boot_out,
        };
        let settings = settings_for(&root, &outside, None);
        let mut wanted = inputs(&root, Some(&settings));

        let kept = execute(&plan(&wanted), &env);

        assert!(kept.ok);
        assert_eq!(booted.get(), 1);
        assert!(root.join("data/file").exists());
        assert!(!login_item::plist_file(&agents).exists());
        assert!(!login_item::plugin_plist_file(&agents).exists());
        assert!(ACCOUNTS.iter().all(|a| store.get(a).unwrap().is_none()));
        assert_eq!(store.get("unrelated").unwrap().as_deref(), Some("kept"));

        wanted.delete_data = true;
        let removed = execute(&plan(&wanted), &env);

        assert!(removed.ok);
        assert!(!root.exists());
        assert!(outside.exists());
    }

    #[test]
    fn a_failed_keychain_delete_is_reported_and_fails_the_run() {
        let store = crate::secrets::testing::DeletesFailWhenArmed::default();
        store.arm();
        let agents = tempfile::tempdir().unwrap();
        let env = Env {
            launch_agents: agents.path(),
            store: &store,
            boot_out: &|| {},
        };

        let report = execute(&[Step::DeleteKeychainItems], &env);

        assert!(!report.ok);
        assert!(report.lines[0].contains("proxy-secret: keychain: locked"));
    }

    #[test]
    fn missing_settings_mean_defaults_and_malformed_ones_mean_unknown() {
        let scratch = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(scratch.path());
        assert_eq!(
            read_settings(&paths).map(|s| s.data_dir),
            Some(paths.default_data_dir())
        );

        fs::write(paths.settings_file(), "{ nope").unwrap();
        assert!(read_settings(&paths).is_none());
    }

    #[test]
    fn a_refused_root_gets_no_lock_file_and_an_accepted_one_does() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("app");
        fs::create_dir_all(&root).unwrap();
        let paths = AppPaths::new(&root);
        let home = scratch.path().join("home");

        let refused = hold_lock(&paths, &root, true, Some(&home)).unwrap();
        assert!(refused.1.is_none());
        assert!(!paths.lock_file().exists());

        let at_home = hold_lock(
            &AppPaths::new(scratch.path()),
            scratch.path(),
            true,
            Some(&root),
        )
        .unwrap();
        assert!(at_home.1.is_none());
        assert!(!AppPaths::new(scratch.path()).lock_file().exists());

        fs::write(paths.settings_file(), "{}").unwrap();
        let accepted = hold_lock(&paths, &root, true, Some(&home)).unwrap();
        assert!(accepted.1.is_some());
        assert!(paths.lock_file().exists());
    }

    fn real_home() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap())
    }

    #[test]
    fn a_firmlink_spelling_of_the_home_folder_or_above_is_refused() {
        let data_volume = Path::new("/System/Volumes/Data");
        let home = real_home();
        if !data_volume.exists() || !home.starts_with("/Users") {
            return;
        }
        let spelled = data_volume.join(home.strip_prefix("/").unwrap());
        for root in [
            spelled.clone(),
            spelled.parent().unwrap().to_path_buf(),
            data_volume.to_path_buf(),
        ] {
            assert!(!deletable(&root, Some(&home)), "{}", root.display());
            let mut wanted = inputs(&root, None);
            wanted.home = Some(&home);
            wanted.delete_data = true;
            assert!(
                !plan(&wanted)
                    .iter()
                    .any(|step| matches!(step, Step::RemoveAppFolder(_))),
                "{}",
                root.display()
            );
        }
    }

    #[test]
    fn a_root_that_is_the_same_directory_as_the_home_or_a_parent_of_it_is_refused() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("a/b/home");
        fs::create_dir_all(&home).unwrap();
        let alias = scratch.path().join("alias");
        std::os::unix::fs::symlink(scratch.path().join("a"), &alias).unwrap();
        let through_alias = alias.join("b/home");
        let sibling = scratch.path().join("a/b/other");
        fs::create_dir_all(&sibling).unwrap();

        assert!(!deletable(&through_alias, Some(&home)));
        assert!(!deletable(&alias.join("b"), Some(&home)));
        assert!(deletable(&sibling, Some(&home)));
        assert!(deletable(&alias.join("b/other"), Some(&home)));
    }

    #[test]
    fn a_root_without_a_settings_file_is_refused_and_one_with_it_is_removed() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("app");
        fs::create_dir_all(&root).unwrap();
        let home = scratch.path().join("home");
        let mut wanted = inputs(&root, None);
        wanted.home = Some(&home);
        wanted.delete_data = true;

        let refused = plan(&wanted);
        assert!(!refused
            .iter()
            .any(|step| matches!(step, Step::RemoveAppFolder(_))));
        assert!(refused
            .iter()
            .any(|s| matches!(s, Step::Note(t) if t.contains("does not look like"))));

        fs::create_dir(AppPaths::new(&root).settings_file()).unwrap();
        assert!(!plan(&wanted)
            .iter()
            .any(|step| matches!(step, Step::RemoveAppFolder(_))));
        fs::remove_dir(AppPaths::new(&root).settings_file()).unwrap();

        fs::write(AppPaths::new(&root).settings_file(), "{}").unwrap();
        assert!(plan(&wanted)
            .iter()
            .any(|step| matches!(step, Step::RemoveAppFolder(_))));
    }

    #[test]
    fn a_settings_file_that_is_a_symlink_is_not_a_marker() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("app");
        fs::create_dir_all(&root).unwrap();
        let real = scratch.path().join("real.json");
        fs::write(&real, "{}").unwrap();
        std::os::unix::fs::symlink(&real, AppPaths::new(&root).settings_file()).unwrap();

        assert!(refusal(&root, Some(&scratch.path().join("home"))).is_some());
    }
}
