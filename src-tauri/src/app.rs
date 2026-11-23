use std::sync::Arc;

use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent};
use tokio::sync::Mutex;

use crate::activity::Activity;
use crate::applog::LogFile;
use crate::core_build::CoreBuild;
use crate::enrol_window;
use crate::lock::{self, LockError};
use crate::paths::{AppPaths, Resources, Sidecars};
use crate::runtime::{self, Config, Running, WorkerConfig};
use crate::secrets::Keychain;
use crate::supervisor::StatusBoard;
use crate::tray::{first_failing, handle_click, show_health, TrayMenu, QUIT};

fn login_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

pub fn run() {
    let Some(root) = AppPaths::configured_root() else {
        eprintln!("HOME is not set; the app root cannot be found");
        std::process::exit(1);
    };
    let paths = AppPaths::new(root.clone());
    let _lock = match lock::acquire(&paths.lock_file()) {
        Ok(lock) => lock,
        Err(LockError::AlreadyRunning) => std::process::exit(0),
        Err(LockError::Io(error)) => {
            eprintln!("cannot take the app lock: {error}");
            std::process::exit(1);
        }
    };

    let app_log = LogFile::new(paths.log_file("app"));

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            enrol_window::window::enrol_submit,
            enrol_window::window::enrol_cancel
        ])
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            app.manage(app_log.clone());
            app.manage(paths.clone());
            let activity = Arc::new(Activity::default());
            app.manage(activity.clone());
            let keychain = Keychain::for_root(&root);

            let resources = Resources::new(app.path().resource_dir().unwrap_or_default());
            let tray_menu = Arc::new(TrayMenu::build(
                app.handle(),
                CoreBuild::read(&resources.core_build()).as_ref(),
            )?);

            let board = Arc::new(StatusBoard::default());
            let running: Running = Arc::new(Mutex::new(None));
            let startup = running
                .clone()
                .try_lock_owned()
                .expect("nobody else holds the lock yet");
            app.manage(running.clone());

            enrol_window::manage(app.handle(), running.clone());

            let clicked_board = board.clone();
            let tray_icon = TrayIconBuilder::new()
                .icon(tauri::include_image!("icons/tray.png"))
                .icon_as_template(true)
                .tooltip("agent-board local server")
                .menu(tray_menu.menu())
                .on_menu_event(move |app, event| {
                    if event.id().as_ref() == QUIT {
                        quit_app(app);
                    } else {
                        handle_click(app, event.id().as_ref(), &clicked_board);
                    }
                })
                .build(app)?;

            let mut changes = board.subscribe();
            let mut busy = activity.subscribe();
            let reader = board.clone();
            tauri::async_runtime::spawn(async move {
                let mut shown = None;
                loop {
                    tray_menu.refresh(&reader, busy.borrow_and_update().is_some());
                    let failing = first_failing(&reader);
                    if failing != shown {
                        show_health(&tray_icon, failing.as_deref());
                        shown = failing;
                    }
                    let alive = tokio::select! {
                        changed = changes.changed() => changed.is_ok(),
                        changed = busy.changed() => changed.is_ok(),
                    };
                    if !alive {
                        break;
                    }
                }
            });

            let signalled = app.handle().clone();
            let signal_log = app_log.clone();
            tauri::async_runtime::spawn(async move {
                use tokio::signal::unix::{signal, SignalKind};
                let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
                    signal(SignalKind::terminate()),
                    signal(SignalKind::interrupt()),
                    signal(SignalKind::hangup()),
                ) else {
                    signal_log.write("cannot listen for SIGTERM, SIGINT and SIGHUP");
                    return;
                };
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                    _ = hup.recv() => {}
                }
                quit_app(&signalled);
            });

            let handle = app.handle().clone();
            let start_log = app_log.clone();
            let keychain = Arc::new(keychain);
            let worker_secrets = keychain.clone();
            let proxy_secrets = keychain;
            tauri::async_runtime::spawn(async move {
                let mut startup = startup;
                let exe_dir = std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
                    .unwrap_or_default();
                let config = Config {
                    sidecars: Sidecars::beside(&exe_dir),
                    resources,
                    login_name: login_name(),
                    port_override: None,
                    upstream_override: None,
                    worker: std::env::current_exe()
                        .ok()
                        .map(|app_executable| WorkerConfig {
                            secrets: worker_secrets,
                            app_executable,
                        }),
                    paths: paths.clone(),
                };
                match runtime::start(config, proxy_secrets, board.clone(), activity).await {
                    Ok(running) => {
                        apply_autostart(
                            &handle,
                            &paths,
                            running.settings().start_at_login,
                            &start_log,
                        );
                        *startup = Some(running);
                    }
                    Err(error) => start_log.write(&error.to_string()),
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("the app could not start")
        .run(|app, event| match event {
            // Tauri asks to exit when the last window closes, and the app lives in the tray; only Quit carries a code.
            RunEvent::ExitRequested {
                api, code: None, ..
            } => api.prevent_exit(),
            RunEvent::Exit => stop_before_exit(app),
            _ => {}
        });
}

/// The system can end the app without a Quit click (log out, restart, a terminate request); the processes it
/// started must not outlive it, so whatever Quit has not already stopped is stopped here.
fn stop_before_exit(app: &AppHandle) {
    if let Some(activity) = app.try_state::<Arc<Activity>>() {
        activity.quit();
    }
    enrol_window::close(app);
    let Some(running) = app.try_state::<Running>() else {
        return;
    };
    let running = running.inner().clone();
    let handle = running.try_lock().ok().and_then(|mut slot| slot.take());
    if let Some(handle) = handle {
        tauri::async_runtime::block_on(handle.stop());
    }
}

/// The startup guard on the shared slot makes a quit that arrives mid-start wait until what started can be stopped.
fn quit_app(app: &AppHandle) {
    app.state::<Arc<Activity>>().quit();
    let running = app.state::<Running>().inner().clone();
    let app = app.clone();
    enrol_window::close(&app);
    tauri::async_runtime::spawn(async move {
        if let Some(handle) = running.lock().await.take() {
            handle.stop().await;
        }
        app.exit(0);
    });
}

/// A development build would register a login item that points into the build directory.
fn apply_autostart(app: &AppHandle, paths: &AppPaths, start_at_login: bool, log: &LogFile) {
    // The login item starts the app without the root variable, so a moved root must not touch it.
    if cfg!(debug_assertions) || AppPaths::default_root().as_deref() != Some(paths.root()) {
        return;
    }
    use tauri_plugin_autostart::ManagerExt;
    let launcher = app.autolaunch();
    let result = if start_at_login {
        launcher.enable()
    } else {
        launcher.disable()
    };
    if let Err(error) = result {
        log.write(&format!("cannot update the login item: {error}"));
    }
}
