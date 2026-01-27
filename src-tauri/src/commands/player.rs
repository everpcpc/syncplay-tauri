use crate::player::detection::{detect_players, DetectedPlayer};

#[tauri::command]
pub fn detect_available_players() -> Vec<DetectedPlayer> {
    detect_players()
}
