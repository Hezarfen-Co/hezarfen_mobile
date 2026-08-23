mod proxy;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// Where the API lives. Baked in at build time so release artifacts point at
/// production without any runtime configuration:
///
/// ```sh
/// HEZARFEN_BACKEND_ORIGIN=https://api.hezarfen.co cargo tauri android build
/// ```
const BUILD_TIME_BACKEND: Option<&str> = option_env!("HEZARFEN_BACKEND_ORIGIN");

/// Dev default. On an emulator or a USB-attached device this is the developer's
/// own machine, reached through `adb reverse tcp:8080 tcp:8080` (see README) —
/// the same address the web app's Vite proxy targets, so one backend serves
/// both.
const DEFAULT_BACKEND: &str = "http://127.0.0.1:8080";

/// A QA override, read once at startup from `<app-config>/backend.txt`. Lets a
/// tester repoint a signed build at staging without a rebuild.
fn runtime_backend<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<String> {
    let path = app.path().app_config_dir().ok()?.join("backend.txt");
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

fn backend_origin<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> String {
    runtime_backend(app)
        .or_else(|| BUILD_TIME_BACKEND.map(str::to_string))
        .unwrap_or_else(|| DEFAULT_BACKEND.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_os::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let backend = backend_origin(&handle);

            // Under `tauri dev` the bundle is empty and Vite serves the app; we
            // proxy to it so dev and release share one origin (and one cookie
            // jar). Tauri sets up `adb reverse` for this port on Android, so
            // the device reaches the host's dev server on localhost.
            let dev_url = if tauri::is_dev() {
                app.config()
                    .build
                    .dev_url
                    .as_ref()
                    .map(|url| url.to_string())
            } else {
                None
            };

            let port = proxy::start(handle, backend.clone(), dev_url)?;
            log::info!("origin server on 127.0.0.1:{port}, backend {backend}");

            let url = format!("http://127.0.0.1:{port}/")
                .parse()
                .expect("the loopback URL is always valid");

            // The window is built here rather than declared in tauri.conf.json
            // because its URL is only known once the server has a port.
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("Hezarfen")
                .inner_size(430.0, 932.0)
                .build()?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Hezarfen app");
}
