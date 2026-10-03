mod archive;
mod engine;
mod protocol;

use engine::{Engine, FileItem, Snapshot};
use std::sync::Arc;
use tauri::{Manager, State};
use tauri_plugin_opener::OpenerExt;

type Backend<'a> = State<'a, Arc<Engine>>;

#[tauri::command]
fn get_snapshot(engine: Backend<'_>) -> Snapshot {
    engine.snapshot()
}

#[tauri::command]
async fn choose_files(engine: Backend<'_>) -> Result<Vec<FileItem>, String> {
    let Some(files) = rfd::AsyncFileDialog::new()
        .set_title("选择要发送的文件")
        .pick_files()
        .await
    else {
        return Ok(vec![]);
    };
    engine
        .inspect(
            files
                .into_iter()
                .map(|f| f.path().to_string_lossy().into_owned())
                .collect(),
        )
        .await
}
#[tauri::command]
async fn choose_folders(engine: Backend<'_>) -> Result<Vec<FileItem>, String> {
    let Some(folders) = rfd::AsyncFileDialog::new()
        .set_title("选择要发送的文件夹（自动压缩为 ZIP）")
        .pick_folders()
        .await
    else {
        return Ok(vec![]);
    };
    engine
        .inspect(
            folders
                .into_iter()
                .map(|folder| folder.path().to_string_lossy().into_owned())
                .collect(),
        )
        .await
}
#[tauri::command]
async fn inspect_files(engine: Backend<'_>, paths: Vec<String>) -> Result<Vec<FileItem>, String> {
    engine.inspect(paths).await
}
#[tauri::command]
async fn choose_save_directory(engine: Backend<'_>) -> Result<Option<String>, String> {
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_title("选择接收文件夹")
        .set_directory(engine.snapshot().save_dir)
        .pick_folder()
        .await
    else {
        return Ok(None);
    };
    engine
        .set_save_dir(folder.path().to_path_buf())
        .await
        .map(Some)
}
#[tauri::command]
async fn send_files(
    engine: Backend<'_>,
    address: String,
    paths: Vec<String>,
    peer_name: Option<String>,
) -> Result<String, String> {
    engine.inner().send_files(address, paths, peer_name).await
}
#[tauri::command]
async fn send_text(
    engine: Backend<'_>,
    address: String,
    text: String,
    name: Option<String>,
    peer_name: Option<String>,
) -> Result<String, String> {
    engine
        .inner()
        .send_text(address, text, name, peer_name)
        .await
}
#[tauri::command]
fn respond_transfer(
    engine: Backend<'_>,
    id: String,
    accept: bool,
    trust: Option<bool>,
) -> Result<(), String> {
    engine.respond(&id, accept, trust.unwrap_or(false))
}
#[tauri::command]
fn revoke_trusted_device(engine: Backend<'_>, public_key: String) -> Result<(), String> {
    engine.revoke_trusted_device(&public_key)
}
#[tauri::command]
fn cancel_transfer(engine: Backend<'_>, id: String) -> Result<(), String> {
    engine.cancel(&id)
}
#[tauri::command]
fn clear_history(engine: Backend<'_>) {
    engine.clear_history();
}
#[tauri::command]
fn set_device_name(engine: Backend<'_>, name: String) -> Result<(), String> {
    engine.set_name(name)
}
#[tauri::command]
async fn open_save_directory(app: tauri::AppHandle, engine: Backend<'_>) -> Result<(), String> {
    let directory = engine.snapshot().save_dir;
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|e| e.to_string())?;
    app.opener()
        .open_path(directory, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let save_dir = dirs::download_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(std::env::temp_dir)
                .join("FileHop");
            let settings_path = Some(app.path().app_config_dir()?.join("settings.json"));
            let engine = Engine::new(save_dir, settings_path);
            app.manage(engine.clone());
            tauri::async_runtime::spawn(async move {
                let _ = engine.start(53318, true).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            choose_files,
            choose_folders,
            inspect_files,
            choose_save_directory,
            send_files,
            send_text,
            respond_transfer,
            revoke_trusted_device,
            cancel_transfer,
            clear_history,
            set_device_name,
            open_save_directory
        ])
        .run(tauri::generate_context!())
        .expect("无法启动 FileHop 应用");
}
