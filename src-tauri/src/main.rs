// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod network;
mod player;
mod client;
mod commands;
mod config;

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

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::connection::connect_to_server,
            commands::connection::disconnect_from_server,
            commands::connection::get_connection_status,
            commands::chat::send_chat_message,
            commands::room::change_room,
            commands::room::set_ready,
            commands::playlist::update_playlist,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
