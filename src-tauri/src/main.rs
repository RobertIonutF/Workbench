#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

#[tauri::command]
fn server_info(state: tauri::State<'_, serde_json::Value>) -> serde_json::Value {
    state.inner().clone()
}
#[tauri::command]
fn quit_workbench(app: tauri::AppHandle) {
    app.exit(0);
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            let runtime = tokio::runtime::Runtime::new()?;
            let core = runtime.block_on(workbench_core::App::new(None))?;
            let (address, token) =
                runtime.block_on(workbench_core::api::start(core.clone(), 0, None))?;
            use tauri::menu::{Menu, MenuItem};
            let show = MenuItem::with_id(app, "show", "Open Workbench", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Workbench", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let mut tray = tauri::tray::TrayIconBuilder::new()
                .tooltip("Workbench — local agent runtime")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "quit" {
                        app.exit(0);
                    } else if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.unminimize();
                        let _ = window.set_focus();
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            app.manage(core);
            app.manage(
                serde_json::json!({ "baseUrl": format!("http://{address}"), "token": token }),
            );
            app.manage(runtime);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let core = window.state::<std::sync::Arc<workbench_core::App>>();
                if core.settings()["backgroundMode"] == true {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![server_info, quit_workbench])
        .build(tauri::generate_context!())
        .expect("Workbench could not start")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                let core = app.state::<std::sync::Arc<workbench_core::App>>();
                core.begin_shutdown();
                app.state::<tokio::runtime::Runtime>().block_on(async {
                    for _ in 0..30 {
                        if core.runs.lock().is_empty() {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                });
            }
        });
}
