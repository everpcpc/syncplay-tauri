// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[allow(dead_code, unused_imports)]
mod app_state;
#[allow(dead_code, unused_imports)]
mod client;
#[allow(dead_code, unused_imports)]
mod commands;
#[allow(dead_code, unused_imports)]
mod config;
#[allow(dead_code, unused_imports)]
mod network;
#[allow(dead_code, unused_imports)]
mod player;
#[allow(dead_code, unused_imports)]
mod utils;

use app_state::AppState;
#[cfg(target_os = "macos")]
use tauri::utils::TitleBarStyle;
use tauri::Manager;
#[cfg(windows)]
use tauri_plugin_frame::FramePluginBuilder;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "syncplay_tauri=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Create global app state
    let app_state = AppState::new();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build());

    let builder = with_frame_plugin(builder);

    builder
        .manage(app_state.clone())
        .setup(move |app| {
            // Store app handle for event emission
            app_state.set_app_handle(app.handle().clone());
            #[cfg(target_os = "macos")]
            {
                if let Some(window) = app.get_webview_window("main") {
                    if let Err(err) = window.set_decorations(true) {
                        tracing::warn!("Failed to enable window decorations on macOS: {}", err);
                    }
                    if let Err(err) = window.set_title_bar_style(TitleBarStyle::Overlay) {
                        tracing::warn!("Failed to set title bar style on macOS: {}", err);
                    }
                }
            }
            #[cfg(windows)]
            {
                if let Some(window) = app.get_webview_window("main") {
                    if let Err(err) = window.set_decorations(false) {
                        tracing::warn!("Failed to disable window decorations on Windows: {}", err);
                    }
                }
            }
            let config = crate::config::load_config(app.handle()).unwrap_or_else(|e| {
                tracing::error!("Failed to load config: {}", e);
                crate::config::SyncplayConfig::default()
            });
            *app_state.config.lock() = config.clone();
            app_state
                .sync_engine
                .lock()
                .update_from_config(&config.user);
            app_state.media_index.update_settings(
                config.player.media_directories.clone(),
                config.player.media_index_timeout_seconds,
            );
            app_state
                .media_index
                .clone()
                .spawn_indexer(app_state.clone());
            if !config.player.media_directories.is_empty() {
                app_state
                    .media_index
                    .clone()
                    .request_refresh(app_state.clone());
            }
            let state = app_state.clone();
            tauri::async_runtime::spawn(async move {
                crate::player::controller::spawn_player_state_loop(state);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::connection::connect_to_server,
            commands::connection::disconnect_from_server,
            commands::connection::get_connection_status,
            commands::chat::send_chat_message,
            commands::room::change_room,
            commands::room::set_ready,
            commands::playlist::update_playlist,
            commands::playlist::check_playlist_items,
            commands::config::get_config,
            commands::config::update_config,
            commands::config::get_config_path,
            commands::config::refresh_media_index,
            commands::config::get_media_index_refreshing,
            commands::player::detect_available_players,
            commands::player::get_cached_players,
            commands::player::refresh_player_detection,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(windows)]
fn with_frame_plugin<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.plugin(
        FramePluginBuilder::new()
            .titlebar_height(32)
            .button_width(46)
            .auto_titlebar(true)
            .build(),
    )
}

#[cfg(not(windows))]
fn with_frame_plugin<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
}
