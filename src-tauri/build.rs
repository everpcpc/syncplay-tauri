fn main() {
    const APP_COMMANDS: &[&str] = &[
        "connect_to_server",
        "disconnect_from_server",
        "get_connection_status",
        "send_chat_message",
        "change_room",
        "set_ready",
        "update_playlist",
        "get_config",
        "update_config",
        "get_config_path",
        "detect_available_players",
    ];

    let manifest = tauri_build::AppManifest::new().commands(APP_COMMANDS);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("failed to build tauri application");
}
