use std::sync::Arc;

use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent};
use tokio::sync::Mutex;

use crate::lock::{self, LockError};
use crate::paths::{AppPaths, Resources, Sidecars};
use crate::runtime::{self, Config, Running, WorkerConfig};
use crate::secrets::Keychain;
use crate::settings::Settings;
use crate::supervisor::StatusBoard;
use crate::tray::{abort_dialogs, handle_click, TrayMenu, QUIT};

fn login_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let Some(root) = AppPaths::configured_root() else {
                eprintln!("HOME is not set; the app root cannot be found");
                std::process::exit(1);
            };
            let keychain = Keychain::for_root(&root);
            let paths = AppPaths::new(root);
            let lock = match lock::acquire(&paths.lock_file()) {
                Ok(lock) => lock,
                Err(LockError::AlreadyRunning) => std::process::exit(0),
                Err(LockError::Io(error)) => {
                    eprintln!("cannot take the app lock: {error}");
                    std::process::exit(1);
                }
            };

            let tray_menu = Arc::new(TrayMenu::build(app.handle())?);

            let board = Arc::new(StatusBoard::default());
            let running: Running = Arc::new(Mutex::new(None));
            let startup = running
                .clone()
                .try_lock_owned()
                .expect("nobody else holds the lock yet");
            app.manage(running.clone());

            let clicked_board = board.clone();
            let clicked_running = running.clone();
            TrayIconBuilder::new()
                .icon(tauri::include_image!("icons/tray.png"))
                .icon_as_template(true)
                .tooltip("agent-board local server")
                .menu(tray_menu.menu())
                .on_menu_event(move |app, event| {
                    if event.id().as_ref() == QUIT {
                        quit_app(app);
                    } else {
                        handle_click(event.id().as_ref(), &clicked_board, &clicked_running);
                    }
                })
                .build(app)?;

            let mut changes = board.subscribe();
            let reader = board.clone();
            tauri::async_runtime::spawn(async move {
                while changes.changed().await.is_ok() {
                    tray_menu.refresh(&reader);
                }
            });

            let signalled = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::signal::unix::{signal, SignalKind};
                let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
                    signal(SignalKind::terminate()),
                    signal(SignalKind::interrupt()),
                    signal(SignalKind::hangup()),
                ) else {
                    eprintln!("cannot listen for SIGTERM, SIGINT and SIGHUP");
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
            let keychain = Arc::new(keychain);
            let worker_secrets = keychain.clone();
            let proxy_secrets = keychain;
            tauri::async_runtime::spawn(async move {
                let mut startup = startup;
                let resources = handle.path().resource_dir().unwrap_or_default();
                let exe_dir = std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
                    .unwrap_or_default();
                let config = Config {
                    sidecars: Sidecars::beside(&exe_dir),
                    resources: Resources::new(resources),
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
                match runtime::start(config, lock, proxy_secrets, board.clone()).await {
                    Ok(running) => {
                        apply_autostart(&handle, &paths);
                        *startup = Some(running);
                    }
                    Err(error) => eprintln!("{error}"),
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("the app could not start")
        .run(|app, event| match event {
            // Tauri asks to exit when no window is left, and this app never has one; only Quit carries a code.
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
    abort_dialogs();
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
    let running = app.state::<Running>().inner().clone();
    let app = app.clone();
    abort_dialogs();
    tauri::async_runtime::spawn(async move {
        if let Some(handle) = running.lock().await.take() {
            handle.stop().await;
        }
        app.exit(0);
    });
}

/// A development build would register a login item that points into the build directory.
fn apply_autostart(app: &AppHandle, paths: &AppPaths) {
    // The login item starts the app without the root variable, so a moved root must not touch it.
    if cfg!(debug_assertions) || AppPaths::default_root().as_deref() != Some(paths.root()) {
        return;
    }
    use tauri_plugin_autostart::ManagerExt;
    let Ok(settings) = Settings::load_or_create(paths, &login_name()) else {
        return;
    };
    let launcher = app.autolaunch();
    let result = if settings.start_at_login {
        launcher.enable()
    } else {
        launcher.disable()
    };
    if let Err(error) = result {
        eprintln!("cannot update the login item: {error}");
    }
}
