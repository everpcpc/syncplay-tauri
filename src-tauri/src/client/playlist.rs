use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;

/// Read-only UI projection and undo history for the coordinator-owned playlist.
pub struct Playlist {
    state: RwLock<PlaylistState>,
}

#[derive(Default)]
struct PlaylistState {
    items: Vec<String>,
    current_index: Option<usize>,
    previous_playlist: Option<Vec<String>>,
    previous_playlist_room: Option<String>,
    last_index_change: Option<Instant>,
}

impl Playlist {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: RwLock::new(PlaylistState::default()),
        })
    }

    pub fn snapshot(&self) -> (Vec<String>, Option<usize>) {
        let state = self.state.read();
        (state.items.clone(), state.current_index)
    }

    pub fn opened_file(&self) {
        self.state.write().last_index_change = Some(Instant::now());
    }

    pub fn not_just_changed(&self, threshold_seconds: f64) -> bool {
        let state = self.state.read();
        let Some(last_change) = state.last_index_change.as_ref() else {
            return true;
        };
        last_change.elapsed().as_secs_f64() > threshold_seconds
    }

    pub(crate) fn set_items_with_index(&self, items: Vec<String>, index: Option<usize>) {
        let mut state = self.state.write();
        state.items = items;
        let len = state.items.len();
        let next_index = match (len, index) {
            (0, _) => None,
            (_, Some(idx)) if idx < len => Some(idx),
            _ => None,
        };
        if state.current_index != next_index {
            state.current_index = next_index;
            state.last_index_change = Some(Instant::now());
        }
    }

    pub fn update_previous_playlist(&self, new_playlist: &[String], room: &str) {
        let mut state = self.state.write();
        let from_old_room = state
            .previous_playlist_room
            .as_deref()
            .map(|stored| stored != room)
            .unwrap_or(true);
        if from_old_room {
            state.previous_playlist = None;
            state.previous_playlist_room = Some(room.to_string());
            return;
        }
        let current_items = state.items.clone();
        if state.previous_playlist.as_ref() != Some(&current_items) && current_items != new_playlist
        {
            state.previous_playlist = Some(current_items);
        }
    }

    pub fn previous_playlist(&self) -> Option<Vec<String>> {
        self.state.read().previous_playlist.clone()
    }

    pub fn playlist_buffer_is_from_old_room(&self, room: &str) -> bool {
        self.state
            .read()
            .previous_playlist_room
            .as_deref()
            .map(|stored| stored != room)
            .unwrap_or(true)
    }
}

impl Default for Playlist {
    fn default() -> Self {
        Self {
            state: RwLock::new(PlaylistState::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_keeps_items_and_index_atomic() {
        let playlist = Playlist::new();
        let items = vec!["file1.mp4".to_string(), "file2.mp4".to_string()];

        playlist.set_items_with_index(items.clone(), Some(1));

        assert_eq!(playlist.snapshot(), (items, Some(1)));
    }

    #[test]
    fn projection_rejects_an_out_of_bounds_index() {
        let playlist = Playlist::new();
        playlist.set_items_with_index(vec!["file1.mp4".to_string()], Some(1));

        assert_eq!(playlist.snapshot().1, None);
    }
}
