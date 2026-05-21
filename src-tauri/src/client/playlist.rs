use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

/// Playlist item
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistItem {
    pub filename: String,
    pub duration: Option<f64>,
}

impl PlaylistItem {
    pub fn new(filename: String) -> Self {
        Self {
            filename,
            duration: None,
        }
    }

    pub fn with_duration(filename: String, duration: f64) -> Self {
        Self {
            filename,
            duration: Some(duration),
        }
    }
}

/// Shared playlist manager
pub struct Playlist {
    items: RwLock<Vec<PlaylistItem>>,
    current_index: RwLock<Option<usize>>,
    queued_index_filename: RwLock<Option<String>>,
    previous_playlist: RwLock<Option<Vec<String>>>,
    previous_playlist_room: RwLock<Option<String>>,
    switch_to_new_item: RwLock<bool>,
    last_index_change: RwLock<Option<Instant>>,
}

impl Playlist {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            items: RwLock::new(Vec::new()),
            current_index: RwLock::new(None),
            queued_index_filename: RwLock::new(None),
            previous_playlist: RwLock::new(None),
            previous_playlist_room: RwLock::new(None),
            switch_to_new_item: RwLock::new(false),
            last_index_change: RwLock::new(None),
        })
    }

    /// Get all playlist items
    pub fn get_items(&self) -> Vec<PlaylistItem> {
        self.items.read().clone()
    }

    pub fn get_item_filenames(&self) -> Vec<String> {
        self.items
            .read()
            .iter()
            .map(|item| item.filename.clone())
            .collect()
    }

    /// Get current index
    pub fn get_current_index(&self) -> Option<usize> {
        *self.current_index.read()
    }

    /// Get current item
    pub fn get_current_item(&self) -> Option<PlaylistItem> {
        let index = *self.current_index.read();
        let items = self.items.read();
        index.and_then(|i| items.get(i).cloned())
    }

    pub fn get_current_filename(&self) -> Option<String> {
        self.get_current_item().map(|item| item.filename)
    }

    pub fn get_queued_index_filename(&self) -> Option<String> {
        self.queued_index_filename.read().clone()
    }

    pub fn set_queued_index_filename(&self, filename: Option<String>) {
        *self.queued_index_filename.write() = filename;
    }

    pub fn mark_switch_to_new_item(&self) {
        *self.switch_to_new_item.write() = true;
    }

    pub fn opened_file(&self) {
        *self.last_index_change.write() = Some(Instant::now());
    }

    pub fn not_just_changed(&self, threshold_seconds: f64) -> bool {
        let guard = self.last_index_change.read();
        let Some(last_change) = guard.as_ref() else {
            return true;
        };
        last_change.elapsed().as_secs_f64() > threshold_seconds
    }

    /// Set playlist items (replaces entire playlist)
    pub fn set_items(&self, items: Vec<String>) {
        info!("Setting playlist with {} items", items.len());
        let playlist_items: Vec<PlaylistItem> = items.into_iter().map(PlaylistItem::new).collect();

        *self.items.write() = playlist_items;

        if !self.items.read().is_empty() {
            let mut current = self.current_index.write();
            *current = Some(0);
            *self.last_index_change.write() = Some(Instant::now());
        } else {
            *self.current_index.write() = None;
        }
    }

    pub fn set_items_with_index(&self, items: Vec<String>, index: Option<usize>) {
        let playlist_items: Vec<PlaylistItem> = items.into_iter().map(PlaylistItem::new).collect();
        *self.items.write() = playlist_items;
        let len = self.items.read().len();
        let mut current = self.current_index.write();
        let next_index = match (len, index) {
            (0, _) => None,
            (_, Some(idx)) if idx < len => Some(idx),
            _ => Some(0),
        };
        if *current != next_index {
            *current = next_index;
            *self.last_index_change.write() = Some(Instant::now());
        }
    }

    /// Add item to playlist
    pub fn add_item(&self, filename: String) {
        info!("Adding item to playlist: {}", filename);
        let mut items = self.items.write();
        items.push(PlaylistItem::new(filename));

        // If this is the first item, set it as current
        if items.len() == 1 {
            *self.current_index.write() = Some(0);
            *self.last_index_change.write() = Some(Instant::now());
        }
    }

    /// Remove item from playlist
    pub fn remove_item(&self, index: usize) -> bool {
        let mut items = self.items.write();

        if index >= items.len() {
            warn!("Cannot remove item at index {}: out of bounds", index);
            return false;
        }

        info!(
            "Removing item at index {}: {}",
            index, items[index].filename
        );
        items.remove(index);

        // Adjust current index if needed
        let mut current = self.current_index.write();
        if let Some(current_idx) = *current {
            if current_idx == index {
                // Current item was removed
                if items.is_empty() {
                    *current = None;
                } else if current_idx >= items.len() {
                    *current = Some(items.len() - 1);
                }
            } else if current_idx > index {
                // Current item shifted down
                *current = Some(current_idx - 1);
            }
        }

        if *current != Some(index) {
            *self.last_index_change.write() = Some(Instant::now());
        }
        true
    }

    /// Set current index
    pub fn set_current_index(&self, index: usize) -> bool {
        let items = self.items.read();

        if index >= items.len() {
            warn!("Cannot set index to {}: out of bounds", index);
            return false;
        }

        info!(
            "Setting current index to {}: {}",
            index, items[index].filename
        );
        let mut current = self.current_index.write();
        if *current != Some(index) {
            *current = Some(index);
            *self.last_index_change.write() = Some(Instant::now());
        }
        true
    }

    pub fn index_of_filename(&self, filename: &str) -> Option<usize> {
        self.items
            .read()
            .iter()
            .position(|item| item.filename == filename)
    }

    pub fn compute_valid_index(&self, new_playlist: &[String]) -> usize {
        if *self.switch_to_new_item.read() {
            *self.switch_to_new_item.write() = false;
            return self.items.read().len();
        }

        let current_index = *self.current_index.read();
        if current_index.is_none() || new_playlist.len() <= 1 {
            return 0;
        }
        let current_items = self.get_item_filenames();
        let start_index = current_index.unwrap_or(0);

        let mut i = start_index;
        while i <= current_items.len() {
            if let Some(filename) = current_items.get(i) {
                if let Some(valid_index) = new_playlist.iter().position(|item| item == filename) {
                    return valid_index;
                }
            }
            i += 1;
        }

        let mut i = start_index;
        while i > 0 {
            if let Some(filename) = current_items.get(i) {
                if let Some(valid_index) = new_playlist.iter().position(|item| item == filename) {
                    if valid_index < new_playlist.len().saturating_sub(1) {
                        return valid_index + 1;
                    }
                    return valid_index;
                }
            }
            i -= 1;
        }

        0
    }

    /// Move to next item
    pub fn next(&self) -> Option<PlaylistItem> {
        let items = self.items.read();
        let mut current = self.current_index.write();

        if items.is_empty() {
            return None;
        }

        let next_index = match *current {
            Some(idx) if idx + 1 < items.len() => idx + 1,
            Some(_) => return None,
            None => 0,
        };

        *current = Some(next_index);
        *self.last_index_change.write() = Some(Instant::now());
        info!("Moving to next item: index {}", next_index);
        items.get(next_index).cloned()
    }

    /// Move to next item with optional loop
    pub fn next_with_loop(&self, loop_at_end: bool) -> Option<PlaylistItem> {
        let items = self.items.read();
        let mut current = self.current_index.write();

        if items.is_empty() {
            return None;
        }

        let next_index = match *current {
            Some(idx) if idx + 1 < items.len() => idx + 1,
            Some(_) if loop_at_end => 0,
            Some(_) => return None,
            None => 0,
        };

        *current = Some(next_index);
        *self.last_index_change.write() = Some(Instant::now());
        info!("Moving to next item: index {}", next_index);
        items.get(next_index).cloned()
    }

    /// Move to previous item
    pub fn previous(&self) -> Option<PlaylistItem> {
        let items = self.items.read();
        let mut current = self.current_index.write();

        if items.is_empty() {
            return None;
        }

        let prev_index = match *current {
            Some(0) => return None,
            Some(idx) => idx - 1,
            None => return None,
        };

        *current = Some(prev_index);
        *self.last_index_change.write() = Some(Instant::now());
        info!("Moving to previous item: index {}", prev_index);
        items.get(prev_index).cloned()
    }

    /// Clear playlist
    pub fn clear(&self) {
        info!("Clearing playlist");
        self.items.write().clear();
        *self.current_index.write() = None;
        *self.last_index_change.write() = Some(Instant::now());
        *self.queued_index_filename.write() = None;
    }

    /// Get playlist size
    pub fn len(&self) -> usize {
        self.items.read().len()
    }

    /// Check if playlist is empty
    pub fn is_empty(&self) -> bool {
        self.items.read().is_empty()
    }

    /// Reorder playlist items
    pub fn reorder(&self, from_index: usize, to_index: usize) -> bool {
        let mut items = self.items.write();

        if from_index >= items.len() || to_index >= items.len() {
            warn!("Cannot reorder: indices out of bounds");
            return false;
        }

        if from_index == to_index {
            return true;
        }

        info!("Reordering playlist: moving {} to {}", from_index, to_index);
        let item = items.remove(from_index);
        items.insert(to_index, item);

        // Adjust current index if needed
        let mut current = self.current_index.write();
        if let Some(current_idx) = *current {
            if current_idx == from_index {
                *current = Some(to_index);
            } else if from_index < current_idx && to_index >= current_idx {
                *current = Some(current_idx - 1);
            } else if from_index > current_idx && to_index <= current_idx {
                *current = Some(current_idx + 1);
            }
        }
        *self.last_index_change.write() = Some(Instant::now());

        true
    }

    pub fn update_previous_playlist(&self, new_playlist: &[String], room: &str) {
        if self.playlist_buffer_is_from_old_room(room) {
            self.move_playlist_buffer_to_new_room(room);
            return;
        }
        if self.playlist_buffer_needs_updating(new_playlist) {
            let current_items = self.get_item_filenames();
            *self.previous_playlist.write() = Some(current_items);
        }
    }

    pub fn previous_playlist(&self) -> Option<Vec<String>> {
        self.previous_playlist.read().clone()
    }

    pub fn playlist_buffer_is_from_old_room(&self, room: &str) -> bool {
        self.previous_playlist_room
            .read()
            .as_deref()
            .map(|stored| stored != room)
            .unwrap_or(true)
    }

    fn move_playlist_buffer_to_new_room(&self, room: &str) {
        *self.previous_playlist.write() = None;
        *self.previous_playlist_room.write() = Some(room.to_string());
    }

    fn playlist_buffer_needs_updating(&self, new_playlist: &[String]) -> bool {
        let current_items = self.get_item_filenames();
        let previous = self.previous_playlist.read();
        previous.as_ref() != Some(&current_items) && current_items != new_playlist
    }

    pub fn can_undo(&self) -> bool {
        let current_items = self.get_item_filenames();
        let previous = self.previous_playlist.read();
        previous.is_some() && previous.as_ref() != Some(&current_items)
    }
}

impl Default for Playlist {
    fn default() -> Self {
        Self {
            items: RwLock::new(Vec::new()),
            current_index: RwLock::new(None),
            queued_index_filename: RwLock::new(None),
            previous_playlist: RwLock::new(None),
            previous_playlist_room: RwLock::new(None),
            switch_to_new_item: RwLock::new(false),
            last_index_change: RwLock::new(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_playlist_add_items() {
        let playlist = Playlist::new();
        playlist.add_item("file1.mp4".to_string());
        playlist.add_item("file2.mp4".to_string());

        assert_eq!(playlist.len(), 2);
        assert_eq!(playlist.get_current_index(), Some(0));
    }

    #[test]
    fn test_playlist_set_items() {
        let playlist = Playlist::new();
        playlist.set_items(vec![
            "file1.mp4".to_string(),
            "file2.mp4".to_string(),
            "file3.mp4".to_string(),
        ]);

        assert_eq!(playlist.len(), 3);
        assert_eq!(playlist.get_current_index(), Some(0));
    }

    #[test]
    fn test_playlist_navigation() {
        let playlist = Playlist::new();
        playlist.set_items(vec![
            "file1.mp4".to_string(),
            "file2.mp4".to_string(),
            "file3.mp4".to_string(),
        ]);

        // Move to next
        let item = playlist.next();
        assert_eq!(item.unwrap().filename, "file2.mp4");
        assert_eq!(playlist.get_current_index(), Some(1));

        // Move to next again
        playlist.next();
        assert_eq!(playlist.get_current_index(), Some(2));

        // End of list should not wrap
        assert!(playlist.next().is_none());
        assert_eq!(playlist.get_current_index(), Some(2));

        // Move to previous
        let item = playlist.previous();
        assert_eq!(item.unwrap().filename, "file2.mp4");
        assert_eq!(playlist.get_current_index(), Some(1));
    }

    #[test]
    fn test_playlist_navigation_with_loop() {
        let playlist = Playlist::new();
        playlist.set_items(vec!["file1.mp4".to_string(), "file2.mp4".to_string()]);
        playlist.set_current_index(1);

        let item = playlist.next_with_loop(true).unwrap();
        assert_eq!(item.filename, "file1.mp4");
        assert_eq!(playlist.get_current_index(), Some(0));
    }

    #[test]
    fn test_playlist_remove() {
        let playlist = Playlist::new();
        playlist.set_items(vec![
            "file1.mp4".to_string(),
            "file2.mp4".to_string(),
            "file3.mp4".to_string(),
        ]);

        playlist.set_current_index(1);
        playlist.remove_item(1);

        assert_eq!(playlist.len(), 2);
        assert_eq!(playlist.get_current_index(), Some(1));
    }

    #[test]
    fn test_playlist_reorder() {
        let playlist = Playlist::new();
        playlist.set_items(vec![
            "file1.mp4".to_string(),
            "file2.mp4".to_string(),
            "file3.mp4".to_string(),
        ]);

        playlist.set_current_index(0);
        playlist.reorder(0, 2);

        let items = playlist.get_items();
        assert_eq!(items[0].filename, "file2.mp4");
        assert_eq!(items[2].filename, "file1.mp4");
        assert_eq!(playlist.get_current_index(), Some(2));
    }

    #[test]
    fn test_playlist_clear() {
        let playlist = Playlist::new();
        playlist.set_items(vec!["file1.mp4".to_string()]);

        playlist.clear();
        assert!(playlist.is_empty());
        assert_eq!(playlist.get_current_index(), None);
    }
}
