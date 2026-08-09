use crate::app_state::AppState;
use crate::commands::connection::emit_error_message;
use crate::config::DEFAULT_MEDIA_INDEX_TIMEOUT_SECONDS;
use crate::utils::PRIVACY_HIDDEN_FILENAME;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::time::{sleep, Duration};

#[derive(Default)]
struct MediaIndexCache {
    by_name: HashMap<String, Vec<PathBuf>>,
}

impl MediaIndexCache {
    fn insert(&mut self, filename: &str, path: PathBuf) {
        self.by_name
            .entry(filename.to_string())
            .or_default()
            .push(path);
    }

    fn insert_override(&mut self, filename: &str, path: PathBuf) {
        fn insert_front(vec: &mut Vec<PathBuf>, path: &PathBuf) {
            if let Some(existing) = vec.iter().position(|entry| entry == path) {
                vec.remove(existing);
            }
            vec.insert(0, path.clone());
        }

        insert_front(self.by_name.entry(filename.to_string()).or_default(), &path);
    }

    fn resolve(&self, filename: &str) -> Option<PathBuf> {
        self.find_existing(self.by_name.get(filename))
    }

    fn find_existing(&self, paths: Option<&Vec<PathBuf>>) -> Option<PathBuf> {
        let paths = paths?;
        for path in paths {
            if path.is_file() {
                return Some(path.clone());
            }
        }
        None
    }
}

pub struct MediaIndex {
    cache: RwLock<MediaIndexCache>,
    directories: RwLock<Vec<String>>,
    timeout_seconds: RwLock<u64>,
    updating: AtomicBool,
    disabled: AtomicBool,
}

impl MediaIndex {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cache: RwLock::new(MediaIndexCache::default()),
            directories: RwLock::new(Vec::new()),
            timeout_seconds: RwLock::new(DEFAULT_MEDIA_INDEX_TIMEOUT_SECONDS),
            updating: AtomicBool::new(false),
            disabled: AtomicBool::new(false),
        })
    }

    pub fn update_settings(&self, directories: Vec<String>, timeout_seconds: u64) -> bool {
        let cleaned: Vec<String> = directories
            .into_iter()
            .map(|dir| dir.trim().to_string())
            .filter(|dir| !dir.is_empty())
            .collect();
        let timeout_seconds = timeout_seconds.max(1);
        let mut directories_guard = self.directories.write();
        let mut timeout_guard = self.timeout_seconds.write();
        if *directories_guard == cleaned && *timeout_guard == timeout_seconds {
            return false;
        }
        *directories_guard = cleaned;
        *timeout_guard = timeout_seconds;
        self.disabled.store(false, Ordering::SeqCst);
        true
    }

    pub fn resolve_path(&self, filename: &str) -> Option<PathBuf> {
        if filename == PRIVACY_HIDDEN_FILENAME {
            return None;
        }
        let path = Path::new(filename);
        if path.is_absolute() && path.is_file() {
            return Some(path.to_path_buf());
        }
        self.cache.read().resolve(filename)
    }

    pub fn add_override_path(&self, filename: &str, path: PathBuf) {
        self.cache.write().insert_override(filename, path);
    }

    pub fn is_refreshing(&self) -> bool {
        self.updating.load(Ordering::SeqCst)
    }

    pub(crate) fn finish_refresh(&self) {
        self.updating.store(false, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn set_refreshing_for_test(&self, refreshing: bool) {
        self.updating.store(refreshing, Ordering::SeqCst);
    }

    pub fn spawn_indexer(self: Arc<Self>, state: Arc<AppState>) {
        tauri::async_runtime::spawn(async move {
            self.refresh(&state).await;
        });
    }

    pub fn request_refresh(self: Arc<Self>, state: Arc<AppState>) {
        tauri::async_runtime::spawn(async move {
            self.refresh(&state).await;
        });
    }

    pub fn request_refresh_force(self: Arc<Self>, state: Arc<AppState>) {
        self.disabled.store(false, Ordering::SeqCst);
        self.request_refresh(state);
    }

    async fn refresh(&self, state: &Arc<AppState>) {
        if self.disabled.load(Ordering::SeqCst) {
            return;
        }
        if self.updating.swap(true, Ordering::SeqCst) {
            return;
        }
        state.emit_event(
            "media-index-refreshing",
            serde_json::json!({ "refreshing": true }),
        );
        let directories = self.directories.read().clone();
        let timeout_seconds = *self.timeout_seconds.read();
        if !directories.is_empty() {
            let result = tokio::task::spawn_blocking(move || {
                scan_directories(&directories, timeout_seconds)
            })
            .await;
            match result {
                Ok(Ok(cache)) => {
                    *self.cache.write() = cache;
                    state.emit_event(
                        "media-index-updated",
                        serde_json::json!({ "timestamp": chrono::Utc::now().to_rfc3339() }),
                    );
                }
                Ok(Err(ScanError::FirstFileTimeout(dir))) => {
                    self.disabled.store(true, Ordering::SeqCst);
                    emit_error_message(
                        state,
                        &format!("Media directory scan timed out while accessing '{}'", dir),
                    );
                }
                Ok(Err(ScanError::ScanTimeout(dir))) => {
                    self.disabled.store(true, Ordering::SeqCst);
                    emit_error_message(
                        state,
                        &format!("Media directory scan timed out in '{}'", dir),
                    );
                }
                Ok(Err(ScanError::NoDirectories)) => {}
                Ok(Err(ScanError::Io(_))) | Err(_) => {
                    emit_error_message(state, "Media directory scan failed");
                }
            }
        }
        crate::client::playback_runtime::media_index_refresh_finished(state).await;
        state.emit_event(
            "media-index-refreshing",
            serde_json::json!({ "refreshing": false }),
        );
        sleep(Duration::from_millis(10)).await;
    }
}

pub(crate) fn resolve_exact_in_directory(directory: &Path, filename: &str) -> Option<PathBuf> {
    let candidate = directory.join(filename);
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

#[allow(dead_code)]
#[derive(Debug)]
enum ScanError {
    NoDirectories,
    FirstFileTimeout(String),
    ScanTimeout(String),
    Io(std::io::Error),
}

fn scan_directories(
    directories: &[String],
    timeout_seconds: u64,
) -> Result<MediaIndexCache, ScanError> {
    if directories.is_empty() {
        return Err(ScanError::NoDirectories);
    }
    let mut cache = MediaIndexCache::default();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_seconds.max(1));

    for directory in directories {
        let directory = directory.trim();
        if directory.is_empty() {
            continue;
        }
        let root = Path::new(directory);
        if !root.is_dir() {
            continue;
        }
        let first_start = Instant::now();
        let mut entries = std::fs::read_dir(root).map_err(ScanError::Io)?;
        let _ = entries.next();
        if first_start.elapsed() > timeout {
            return Err(ScanError::FirstFileTimeout(directory.to_string()));
        }
    }

    for directory in directories {
        let directory = directory.trim();
        if directory.is_empty() {
            continue;
        }
        let root = Path::new(directory);
        if !root.is_dir() {
            continue;
        }
        let mut stack = vec![root.to_path_buf()];
        while let Some(current) = stack.pop() {
            if start.elapsed() > timeout {
                return Err(ScanError::ScanTimeout(directory.to_string()));
            }
            let entries = match std::fs::read_dir(&current) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            let mut child_directories = Vec::new();
            for entry in entries.flatten() {
                if start.elapsed() > timeout {
                    return Err(ScanError::ScanTimeout(directory.to_string()));
                }
                let path = entry.path();
                if path.is_dir() {
                    child_directories.push(path);
                    continue;
                }
                if !path.is_file() {
                    continue;
                }
                let filename_os = entry.file_name();
                let filename = match filename_os.to_str() {
                    Some(name) => name,
                    None => continue,
                };
                cache.insert(filename, path);
            }
            stack.extend(child_directories.into_iter().rev());
        }
    }

    Ok(cache)
}

#[cfg(test)]
mod tests {
    use super::{scan_directories, MediaIndex};
    use crate::config::DEFAULT_MEDIA_INDEX_TIMEOUT_SECONDS;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn resolve_path_preserves_index_order_instead_of_preferring_the_previous_directory() {
        let previous_dir = TempDir::new().unwrap();
        let media_dir = TempDir::new().unwrap();
        let previous_file = previous_dir.path().join("episode-01.mkv");
        let next_file = previous_dir.path().join("episode-02.mkv");
        let indexed_file = media_dir.path().join("episode-02.mkv");
        fs::write(&previous_file, b"previous").unwrap();
        fs::write(&next_file, b"next").unwrap();
        fs::write(&indexed_file, b"indexed").unwrap();

        let index = MediaIndex::new();
        index.add_override_path("episode-02.mkv", indexed_file.clone());
        index.add_override_path("episode-01.mkv", previous_file);

        assert_eq!(index.resolve_path("episode-02.mkv"), Some(indexed_file));
        assert!(next_file.is_file());
    }

    #[test]
    fn resolve_path_does_not_fuzzy_match_a_different_filename() {
        let media_dir = TempDir::new().unwrap();
        let similar_file = media_dir.path().join("A_B.mkv");
        fs::write(&similar_file, b"similar").unwrap();

        let cache = scan_directories(
            &[media_dir.path().to_string_lossy().to_string()],
            DEFAULT_MEDIA_INDEX_TIMEOUT_SECONDS,
        )
        .unwrap();

        assert_eq!(cache.resolve("A_B.mkv"), Some(similar_file));
        assert_eq!(cache.resolve("A-B.mkv"), None);
    }

    #[test]
    fn resolve_path_does_not_drop_relative_directory_components() {
        let media_dir = TempDir::new().unwrap();
        let nested_dir = media_dir.path().join("nested");
        fs::create_dir(&nested_dir).unwrap();
        let nested_file = nested_dir.join("movie.mkv");
        fs::write(&nested_file, b"nested").unwrap();

        let cache = scan_directories(
            &[media_dir.path().to_string_lossy().to_string()],
            DEFAULT_MEDIA_INDEX_TIMEOUT_SECONDS,
        )
        .unwrap();

        assert_eq!(cache.resolve("nested/movie.mkv"), None);
    }

    #[test]
    fn scan_directories_preserves_directory_order_for_duplicate_names() {
        let first_dir = TempDir::new().unwrap();
        let second_dir = TempDir::new().unwrap();
        let first_file = first_dir.path().join("movie.mp4");
        let second_file = second_dir.path().join("movie.mp4");
        fs::write(&first_file, b"first").unwrap();
        fs::write(&second_file, b"second").unwrap();

        let directories = vec![
            second_dir.path().to_string_lossy().to_string(),
            first_dir.path().to_string_lossy().to_string(),
        ];
        let cache = scan_directories(&directories, DEFAULT_MEDIA_INDEX_TIMEOUT_SECONDS).unwrap();

        assert_eq!(cache.resolve("movie.mp4"), Some(second_file));
    }

    #[test]
    fn scan_directories_preserves_sibling_enumeration_order() {
        let root = TempDir::new().unwrap();
        for directory in ["first", "second"] {
            let directory = root.path().join(directory);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("movie.mp4"), b"movie").unwrap();
        }
        let expected = fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.is_dir())
            .unwrap()
            .join("movie.mp4");

        let cache = scan_directories(
            &[root.path().to_string_lossy().to_string()],
            DEFAULT_MEDIA_INDEX_TIMEOUT_SECONDS,
        )
        .unwrap();

        assert_eq!(cache.resolve("movie.mp4"), Some(expected));
    }
}
