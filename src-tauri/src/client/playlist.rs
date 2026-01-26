use parking_lot::RwLock;
use std::sync::Arc;
use tracing::{debug, info, warn};

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
}

impl Playlist {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            items: RwLock::new(Vec::new()),
            current_index: RwLock::new(None),
        })
    }

    /// Get all playlist items
    pub fn get_items(&self) -> Vec<PlaylistItem> {
        self.items.read().clone()
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

    /// Set playlist items (replaces entire playlist)
    pub fn set_items(&self, items: Vec<String>) {
        info!("Setting playlist with {} items", items.len());
        let playlist_items: Vec<PlaylistItem> = items
            .into_iter()
            .map(PlaylistItem::new)
            .collect();

        *self.items.write() = playlist_items;

        // Reset index if playlist is not empty
        if !self.items.read().is_empty() {
            *self.current_index.write() = Some(0);
        } else {
            *self.current_index.write() = None;
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
        }
    }

    /// Remove item from playlist
    pub fn remove_item(&self, index: usize) -> bool {
        let mut items = self.items.write();

        if index >= items.len() {
            warn!("Cannot remove item at index {}: out of bounds", index);
            return false;
        }

        info!("Removing item at index {}: {}", index, items[index].filename);
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

        true
    }

    /// Set current index
    pub fn set_current_index(&self, index: usize) -> bool {
        let items = self.items.read();

        if index >= items.len() {
            warn!("Cannot set index to {}: out of bounds", index);
            return false;
        }

        info!("Setting current index to {}: {}", index, items[index].filename);
        *self.current_index.write() = Some(index);
        true
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
            Some(_) => 0, // Wrap around to beginning
            None => 0,
        };

        *current = Some(next_index);
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
            Some(0) => items.len() - 1, // Wrap around to end
            Some(idx) => idx - 1,
            None => items.len() - 1,
        };

        *current = Some(prev_index);
        info!("Moving to previous item: index {}", prev_index);
        items.get(prev_index).cloned()
    }

    /// Clear playlist
    pub fn clear(&self) {
        info!("Clearing playlist");
        self.items.write().clear();
        *self.current_index.write() = None;
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

        true
    }
}

impl Default for Playlist {
    fn default() -> Self {
        Self {
            items: RwLock::new(Vec::new()),
            current_index: RwLock::new(None),
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

        // Wrap around
        playlist.next();
        assert_eq!(playlist.get_current_index(), Some(0));

        // Move to previous
        let item = playlist.previous();
        assert_eq!(item.unwrap().filename, "file3.mp4");
        assert_eq!(playlist.get_current_index(), Some(2));
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
