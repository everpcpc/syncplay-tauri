// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]
#![allow(unused_imports)]

mod app_state;
mod client;
mod commands;
mod config;
mod network;
mod player;

use app_state::AppState;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "syncplay_tauri=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Create global app state
    let app_state = AppState::new();

    tauri::Builder::default()
        .manage(app_state.clone())
        .setup(move |app| {
            // Store app handle for event emission
            app_state.set_app_handle(app.handle());
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
            commands::config::get_config,
            commands::config::update_config,
            commands::config::get_config_path,
            commands::player::detect_available_players,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
