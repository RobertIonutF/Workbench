#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

#[tauri::command]
fn server_info(state: tauri::State<'_, serde_json::Value>) -> serde_json::Value {
    state.inner().clone()
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
            app.manage(core);
            app.manage(
                serde_json::json!({ "baseUrl": format!("http://{address}"), "token": token }),
            );
            app.manage(runtime);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![server_info])
        .build(tauri::generate_context!())
        .expect("Workbench could not start")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                let core = app.state::<std::sync::Arc<workbench_core::App>>();
                for cancel in core.runs.lock().values() {
                    cancel.cancel();
                }
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
