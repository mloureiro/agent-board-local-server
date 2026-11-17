use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::RunEvent;

const QUIT: &str = "quit";

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let status = MenuItem::with_id(
                app,
                "status",
                "Board server: not packaged yet",
                false,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&status, &quit])?;

            TrayIconBuilder::new()
                .icon(tauri::include_image!("icons/tray.png"))
                .icon_as_template(true)
                .tooltip("agent-board local server")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    if event.id().as_ref() == QUIT {
                        app.exit(0);
                    }
                })
                .build(app)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("the app could not start")
        .run(|_app, event| {
            // Tauri asks to exit when no window is left, and this app never has one; only Quit carries a code.
            if let RunEvent::ExitRequested {
                api, code: None, ..
            } = event
            {
                api.prevent_exit();
            }
        });
}
