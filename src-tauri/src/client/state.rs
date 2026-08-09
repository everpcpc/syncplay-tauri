use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::network::messages::{FileInfo, FileSizeInfo};

/// User information
#[derive(Debug, Clone)]
pub struct User {
    pub username: String,
    pub room: String,
    pub file: Option<String>,
    pub file_size: Option<FileSizeInfo>,
    pub file_duration: Option<f64>,
    pub is_ready: Option<bool>,
    pub is_controller: bool,
    pub features: Option<Value>,
}

impl User {
    pub fn is_ready_with_file(&self) -> Option<bool> {
        self.file.as_ref()?;
        self.is_ready
    }
}

/// Global playback state
#[derive(Debug, Clone)]
pub struct GlobalPlayState {
    pub position: f64,
    pub paused: bool,
    pub set_by: Option<String>,
}

/// Client global state
pub struct ClientState {
    /// Current username
    username: RwLock<String>,
    /// Current room
    room: RwLock<String>,
    /// Last media snapshot confirmed by the player.
    file: RwLock<FileInfo>,
    /// User list (username -> User)
    users: RwLock<HashMap<String, User>>,
    /// Global playback state
    global_state: RwLock<GlobalPlayState>,
    /// Local ready state
    is_ready: RwLock<Option<bool>>,
    /// Server version
    server_version: RwLock<Option<String>>,
}

impl ClientState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            username: RwLock::new(String::new()),
            room: RwLock::new(String::new()),
            file: RwLock::new(FileInfo {
                name: None,
                size: None,
                duration: None,
            }),
            users: RwLock::new(HashMap::new()),
            global_state: RwLock::new(GlobalPlayState {
                position: 0.0,
                paused: true,
                set_by: None,
            }),
            is_ready: RwLock::new(None),
            server_version: RwLock::new(None),
        })
    }

    // Username methods
    pub fn get_username(&self) -> String {
        self.username.read().clone()
    }

    pub fn set_username(&self, username: String) {
        *self.username.write() = username;
    }

    // Room methods
    pub fn get_room(&self) -> String {
        self.room.read().clone()
    }

    pub fn set_room(&self, room: String) {
        let changed = {
            let mut current_room = self.room.write();
            if *current_room == room {
                false
            } else {
                *current_room = room.clone();
                true
            }
        };
        if !changed {
            return;
        }

        let username = self.username.read().clone();
        if let Some(user) = self.users.write().get_mut(&username) {
            user.room = room;
            user.is_controller = false;
        }
    }

    // File methods
    pub fn get_file(&self) -> Option<String> {
        self.file.read().name.clone()
    }

    pub fn set_file(&self, file: Option<String>) {
        let mut current = self.file.write();
        if current.name != file {
            current.size = None;
            current.duration = None;
        }
        current.name = file;
    }

    pub fn get_file_info(&self) -> FileInfo {
        self.file.read().clone()
    }

    pub fn set_file_info(&self, file: FileInfo) {
        *self.file.write() = file;
    }

    pub fn get_file_duration(&self) -> Option<f64> {
        self.file.read().duration
    }

    pub fn set_file_duration(&self, duration: Option<f64>) {
        self.file.write().duration = duration;
    }

    // User list methods
    pub fn add_user(&self, user: User) {
        self.users.write().insert(user.username.clone(), user);
    }

    pub fn remove_user(&self, username: &str) {
        self.users.write().remove(username);
    }

    pub fn get_user(&self, username: &str) -> Option<User> {
        self.users.read().get(username).cloned()
    }

    pub fn get_users(&self) -> Vec<User> {
        self.users.read().values().cloned().collect()
    }

    pub fn get_users_in_room(&self, room: &str) -> Vec<User> {
        self.users
            .read()
            .values()
            .filter(|u| u.room == room)
            .cloned()
            .collect()
    }

    pub fn clear_users(&self) {
        self.users.write().clear();
    }

    // Global state methods
    pub fn get_global_state(&self) -> GlobalPlayState {
        self.global_state.read().clone()
    }

    pub fn set_global_state(&self, position: f64, paused: bool, set_by: Option<String>) {
        let mut state = self.global_state.write();
        state.position = position;
        state.paused = paused;
        state.set_by = set_by;
    }

    // Ready state methods
    pub fn is_ready(&self) -> bool {
        self.ready_state().unwrap_or(false)
    }

    pub fn ready_state(&self) -> Option<bool> {
        *self.is_ready.read()
    }

    pub fn set_ready(&self, ready: bool) {
        self.set_ready_state(Some(ready));
    }

    pub fn set_ready_state(&self, ready: Option<bool>) {
        *self.is_ready.write() = ready;
    }

    // Server version methods
    pub fn get_server_version(&self) -> Option<String> {
        self.server_version.read().clone()
    }

    pub fn set_server_version(&self, version: String) {
        *self.server_version.write() = Some(version);
    }

    pub fn clear_server_version(&self) {
        *self.server_version.write() = None;
    }
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            username: RwLock::new(String::new()),
            room: RwLock::new(String::new()),
            file: RwLock::new(FileInfo {
                name: None,
                size: None,
                duration: None,
            }),
            users: RwLock::new(HashMap::new()),
            global_state: RwLock::new(GlobalPlayState {
                position: 0.0,
                paused: true,
                set_by: None,
            }),
            is_ready: RwLock::new(None),
            server_version: RwLock::new(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_room_clears_current_users_controller_status() {
        let state = ClientState::new();
        state.set_username("alice".to_string());
        state.set_room("old".to_string());
        state.add_user(User {
            username: "alice".to_string(),
            room: "old".to_string(),
            file: None,
            file_size: None,
            file_duration: None,
            is_ready: None,
            is_controller: true,
            features: None,
        });

        state.set_room("new".to_string());

        let current = state.get_user("alice").unwrap();
        assert_eq!(current.room, "new");
        assert!(!current.is_controller);
    }
}
