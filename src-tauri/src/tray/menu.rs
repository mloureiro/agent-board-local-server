use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::{AppHandle, Wry};

use crate::core_build::CoreBuild;
use crate::supervisor::table::{PROXY, ROWS, WORKER_LISTEN};
use crate::supervisor::{Status, StatusBoard};

pub const OPEN_BOARD: &str = "open-board";
pub const CONNECT_WORKER: &str = "connect-worker";
pub const CHECK_WORKER: &str = "check-worker";
pub const RESTART_SERVER: &str = "restart-server";
pub const RESTART_WORKER: &str = "restart-worker";
pub const DISCONNECT_WORKER: &str = "disconnect-worker";
pub const OPEN_LOGS: &str = "open-logs";
pub const OPEN_SETTINGS: &str = "open-settings";
pub const QUIT: &str = "quit";

pub fn proxy_port(proxy: Option<&Status>) -> Option<u16> {
    match proxy {
        Some(Status::Running { port }) => Some(*port),
        _ => None,
    }
}

pub fn connect_label(listener: Option<&Status>) -> &'static str {
    match listener {
        None | Some(Status::NotConnected) => "Connect worker…",
        Some(_) => "Reconnect worker…",
    }
}

pub fn worker_connected(listener: Option<&Status>) -> bool {
    matches!(listener, Some(status) if *status != Status::NotConnected)
}

pub fn action_enabled(busy: bool) -> bool {
    !busy
}

pub fn worker_action_enabled(busy: bool, listener: Option<&Status>) -> bool {
    action_enabled(busy) && worker_connected(listener)
}

pub fn version_line(build: Option<&CoreBuild>) -> String {
    match build {
        Some(build) => format!(
            "core {} · core built {}",
            build.short_commit(),
            build.built_date()
        ),
        None => "core unknown".into(),
    }
}

pub struct TrayMenu {
    menu: Menu<Wry>,
    open: MenuItem<Wry>,
    connect: MenuItem<Wry>,
    check: MenuItem<Wry>,
    restart_server: MenuItem<Wry>,
    restart_worker: MenuItem<Wry>,
    disconnect: MenuItem<Wry>,
    open_logs: MenuItem<Wry>,
    open_settings: MenuItem<Wry>,
    rows: Vec<(&'static str, &'static str, MenuItem<Wry>)>,
}

impl TrayMenu {
    pub fn build(app: &AppHandle, core_build: Option<&CoreBuild>) -> tauri::Result<Self> {
        let open = MenuItem::with_id(app, OPEN_BOARD, "Open board", false, None::<&str>)?;
        let connect =
            MenuItem::with_id(app, CONNECT_WORKER, "Connect worker…", false, None::<&str>)?;
        let check = MenuItem::with_id(app, CHECK_WORKER, "Check worker", false, None::<&str>)?;
        let item = |id: &str, label: &str| MenuItem::with_id(app, id, label, false, None::<&str>);
        let restart_server = item(RESTART_SERVER, "Restart server")?;
        let restart_worker = item(RESTART_WORKER, "Restart worker")?;
        let disconnect = item(DISCONNECT_WORKER, "Disconnect worker")?;
        let open_logs = item(OPEN_LOGS, "Open logs folder")?;
        let open_settings = item(
            OPEN_SETTINGS,
            "Open settings file (applies after Quit and reopen)",
        )?;
        let quit = MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?;
        let rows: Vec<_> = ROWS
            .iter()
            .map(|row| {
                MenuItem::with_id(
                    app,
                    format!("status-{}", row.name),
                    row.initial,
                    false,
                    None::<&str>,
                )
                .map(|item| (row.name, row.title, item))
            })
            .collect::<tauri::Result<_>>()?;
        let version = MenuItem::with_id(
            app,
            "core-version",
            version_line(core_build),
            false,
            None::<&str>,
        )?;
        let divider = PredefinedMenuItem::separator(app)?;
        let mut entries: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = vec![&open, &divider];
        entries.extend(
            rows.iter()
                .map(|(_, _, item)| item as &dyn tauri::menu::IsMenuItem<Wry>),
        );
        let lower_divider = PredefinedMenuItem::separator(app)?;
        entries.extend([
            &lower_divider as &dyn tauri::menu::IsMenuItem<Wry>,
            &version,
            &restart_server,
            &restart_worker,
            &disconnect,
            &check,
            &open_logs,
            &open_settings,
            &connect,
            &quit,
        ]);
        let menu = Menu::with_items(app, &entries)?;
        Ok(Self {
            menu,
            open,
            connect,
            check,
            restart_server,
            restart_worker,
            disconnect,
            open_logs,
            open_settings,
            rows,
        })
    }

    pub fn menu(&self) -> &Menu<Wry> {
        &self.menu
    }

    pub fn refresh(&self, board: &StatusBoard, busy: bool) {
        for (name, title, item) in &self.rows {
            if let Some(current) = board.get(name) {
                set_text(item, current.label(title));
            }
        }
        let ready = proxy_port(board.get(PROXY).as_ref()).is_some();
        let _ = self.open.set_enabled(ready);
        let _ = self.connect.set_enabled(ready);
        let listener = board.get(WORKER_LISTEN);
        for item in [&self.restart_server, &self.open_logs, &self.open_settings] {
            let _ = item.set_enabled(action_enabled(busy));
        }
        for item in [&self.restart_worker, &self.disconnect, &self.check] {
            let _ = item.set_enabled(worker_action_enabled(busy, listener.as_ref()));
        }
        set_text(
            &self.connect,
            connect_label(board.get(WORKER_LISTEN).as_ref()).to_owned(),
        );
    }
}

fn set_text(item: &MenuItem<Wry>, label: String) {
    if item.text().ok().as_deref() != Some(label.as_str()) {
        let _ = item.set_text(label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_line_names_the_commit_and_the_build_day_or_says_unknown() {
        let build = CoreBuild {
            commit: "0123456789abcdef".into(),
            built_at: "2026-11-20T08:15:00.000Z".into(),
        };

        assert_eq!(
            version_line(Some(&build)),
            "core 0123456 · core built 2026-11-20"
        );
        assert_eq!(version_line(None), "core unknown");
    }

    #[test]
    fn the_board_can_be_opened_only_while_the_proxy_runs() {
        assert_eq!(
            proxy_port(Some(&Status::Running { port: 4785 })),
            Some(4785)
        );
        assert_eq!(proxy_port(Some(&Status::Starting)), None);
        assert_eq!(proxy_port(Some(&Status::Stopped)), None);
        assert_eq!(proxy_port(None), None);
    }

    #[test]
    fn a_worker_action_needs_a_connected_worker_and_a_free_gate() {
        let blocked = Status::Blocked { reason: "x".into() };

        assert!(!worker_action_enabled(false, None));
        assert!(!worker_action_enabled(false, Some(&Status::NotConnected)));
        assert!(worker_action_enabled(false, Some(&Status::Starting)));
        assert!(worker_action_enabled(false, Some(&blocked)));
        assert!(!worker_action_enabled(true, Some(&Status::Starting)));
    }

    #[test]
    fn every_other_action_needs_only_a_free_gate() {
        assert!(action_enabled(false));
        assert!(!action_enabled(true));
    }

    #[test]
    fn the_menu_offers_to_connect_until_a_worker_is_enrolled_and_to_reconnect_after() {
        assert_eq!(connect_label(None), "Connect worker…");
        assert_eq!(
            connect_label(Some(&Status::NotConnected)),
            "Connect worker…"
        );
        assert_eq!(
            connect_label(Some(&Status::Note("running".into()))),
            "Reconnect worker…"
        );
    }
}
