mod external;
mod offsets;
mod server;
mod types;

use external::WinProc;
use server::tokio_init;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager, WindowEvent,
};
use tokio::sync::Mutex;
use types::PlayerInfo;

struct AppState {
    proc: Mutex<Option<WinProc>>,
}

#[tauri::command]
async fn find_and_attach(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let Ok(mut win_proc) = WinProc::from_name("Client-Win64-Shipping.exe") else {
        return Err(String::from("FAILED FIND PROC"));
    };

    if !win_proc.init() {
        return Err(String::from("FAILED ATTACH PROC"));
    }
    *state.proc.lock().await = Some(win_proc);
    Ok(())
}

// remember to call `.manage(MyState::default())`
#[tauri::command]
async fn get_location(state: tauri::State<'_, AppState>) -> Result<PlayerInfo, String> {
    let state = state.clone();
    let proc_lock = state.proc.lock().await;
    let Some(ref proc) = *proc_lock else {
        return Err(String::from("Process not initialized"));
    };
    return proc.get_location();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[tokio::main]
pub async fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let app_handle = app.handle().clone();

            let quit_menu = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
            let show_menu = MenuItem::with_id(app, "show", "창 표시", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_menu, &quit_menu])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .on_menu_event(|app, event| {
                    let window = app.get_webview_window("main").unwrap();

                    match event.id.as_ref() {
                        "quit" => {
                            std::process::exit(0);
                        }
                        "show" => {
                            window.show().unwrap();
                            window.set_focus().unwrap();
                        }
                        _ => {}
                    }
                })
                .menu(&menu)
                .menu_on_left_click(true)
                .build(app)?;

            if let Some(window) = app.get_webview_window("main") {
                let window_handle = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        window_handle.hide().unwrap();
                        api.prevent_close();
                    }
                });
            }
            // Actix 서버를 setup 단계에서 비동기적으로 시작
            tokio::spawn(async move {
                tokio_init(app_handle).await;
            });
            Ok(())
        })
        .manage(AppState {
            proc: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![find_and_attach, get_location])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
