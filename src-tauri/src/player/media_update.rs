//! Transactional boundary between requested loads and confirmed player media.
//!
//! Callers must propagate the load ID that produced an update. Unattributed
//! snapshots use `None` and cannot complete while an explicit load is pending.

use crate::utils::is_url;

pub type LoadId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMediaLoad {
    pub id: LoadId,
    pub target: String,
    cancelled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaSnapshot {
    pub filename: Option<String>,
    pub path: Option<String>,
    pub duration: Option<f64>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaField {
    Filename,
    Path,
    Duration,
    Size,
}

#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub enum MediaCommit {
    Committed(MediaSnapshot),
    Incomplete {
        load_id: Option<LoadId>,
        missing: Vec<MediaField>,
    },
    MissingIdentity {
        load_id: Option<LoadId>,
    },
    Stale {
        completed_load_id: Option<LoadId>,
        latest_load_id: Option<LoadId>,
    },
    TargetMismatch {
        load_id: LoadId,
        expected_target: String,
        snapshot: MediaSnapshot,
    },
}

#[derive(Debug, Clone)]
#[must_use]
pub struct MediaUpdateTransaction {
    load_id: Option<LoadId>,
    filename: Collected<Option<String>>,
    path: Collected<Option<String>>,
    duration: Collected<Option<f64>>,
    size: Collected<Option<u64>>,
}

impl MediaUpdateTransaction {
    pub fn load_id(&self) -> Option<LoadId> {
        self.load_id
    }

    pub fn set_filename(&mut self, filename: Option<String>) {
        self.filename = Collected::Value(filename);
    }

    pub fn set_load_id(&mut self, load_id: Option<LoadId>) {
        self.load_id = load_id;
    }

    pub fn set_path(&mut self, path: Option<String>) {
        self.path = Collected::Value(path);
    }

    pub fn set_duration(&mut self, duration: Option<f64>) {
        self.duration = Collected::Value(duration);
    }

    pub fn set_size(&mut self, size: Option<u64>) {
        self.size = Collected::Value(size);
    }

    fn missing_fields(&self) -> Vec<MediaField> {
        let mut missing = Vec::new();
        if self.filename.is_missing() {
            missing.push(MediaField::Filename);
        }
        if self.path.is_missing() {
            missing.push(MediaField::Path);
        }
        if self.duration.is_missing() {
            missing.push(MediaField::Duration);
        }
        if self.size.is_missing() {
            missing.push(MediaField::Size);
        }
        missing
    }

    fn into_snapshot(self) -> MediaSnapshot {
        MediaSnapshot {
            filename: self.filename.into_value(),
            path: self.path.into_value(),
            duration: self.duration.into_value(),
            size: self.size.into_value(),
        }
    }
}

#[derive(Debug, Clone)]
enum Collected<T> {
    Missing,
    Value(T),
}

impl<T> Collected<T> {
    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    fn into_value(self) -> T {
        match self {
            Self::Missing => unreachable!("transaction completeness is checked before commit"),
            Self::Value(value) => value,
        }
    }
}

#[derive(Debug, Default)]
pub struct MediaUpdateState {
    latest_load: Option<PendingMediaLoad>,
    confirmed: Option<MediaSnapshot>,
}

impl MediaUpdateState {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin_load(
        &mut self,
        load_id: LoadId,
        target: impl Into<String>,
    ) -> Option<PendingMediaLoad> {
        self.latest_load.replace(PendingMediaLoad {
            id: load_id,
            target: target.into(),
            cancelled: false,
        })
    }

    pub fn begin_update(&self, load_id: Option<LoadId>) -> MediaUpdateTransaction {
        MediaUpdateTransaction {
            load_id,
            filename: Collected::Missing,
            path: Collected::Missing,
            duration: Collected::Missing,
            size: Collected::Missing,
        }
    }

    pub fn cancel_load(&mut self, load_id: LoadId) -> bool {
        if self.latest_load.as_ref().map(|load| load.id) != Some(load_id) {
            return false;
        }
        if let Some(load) = self.latest_load.as_mut() {
            load.cancelled = true;
        }
        true
    }

    #[cfg(test)]
    pub fn is_loading(&self) -> bool {
        self.latest_load
            .as_ref()
            .is_some_and(|load| !load.cancelled)
    }

    #[cfg(test)]
    pub fn latest_load(&self) -> Option<&PendingMediaLoad> {
        self.latest_load.as_ref()
    }

    #[cfg(test)]
    pub fn active_load(&self) -> Option<&PendingMediaLoad> {
        self.latest_load.as_ref().filter(|load| !load.cancelled)
    }

    #[cfg(test)]
    pub fn last_confirmed(&self) -> Option<&MediaSnapshot> {
        self.confirmed.as_ref()
    }

    #[cfg(test)]
    pub fn ready_snapshot(&self) -> Option<&MediaSnapshot> {
        if self.latest_load.is_some() {
            None
        } else {
            self.confirmed.as_ref()
        }
    }

    pub fn commit(&mut self, update: MediaUpdateTransaction) -> MediaCommit {
        let completed_load_id = update.load_id;
        if completed_load_id.is_none()
            && self.latest_load.as_ref().is_some_and(|load| load.cancelled)
        {
            self.latest_load = None;
        }
        let latest_load_id = self.latest_load.as_ref().map(|load| load.id);
        if completed_load_id != latest_load_id {
            return MediaCommit::Stale {
                completed_load_id,
                latest_load_id,
            };
        }
        if self.latest_load.as_ref().is_some_and(|load| load.cancelled) {
            self.latest_load = None;
            return MediaCommit::Stale {
                completed_load_id,
                latest_load_id,
            };
        }

        let missing = update.missing_fields();
        if !missing.is_empty() {
            return MediaCommit::Incomplete {
                load_id: completed_load_id,
                missing,
            };
        }

        let snapshot = update.into_snapshot();
        if snapshot.filename.is_none() && snapshot.path.is_none() {
            return MediaCommit::MissingIdentity {
                load_id: completed_load_id,
            };
        }

        if let Some(load) = self.latest_load.as_ref() {
            if !snapshot_matches_target(&snapshot, &load.target) {
                return MediaCommit::TargetMismatch {
                    load_id: load.id,
                    expected_target: load.target.clone(),
                    snapshot,
                };
            }
        }

        self.confirmed = Some(snapshot.clone());
        self.latest_load = None;
        MediaCommit::Committed(snapshot)
    }
}

fn snapshot_matches_target(snapshot: &MediaSnapshot, target: &str) -> bool {
    snapshot
        .filename
        .iter()
        .chain(snapshot.path.iter())
        .any(|value| {
            value == target
                || (!is_url(target) && !is_url(value) && media_name(value) == media_name(target))
        })
}

fn media_name(value: &str) -> &str {
    value.rsplit(['/', '\\']).next().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_update(
        state: &MediaUpdateState,
        load_id: Option<LoadId>,
        filename: &str,
        path: &str,
        duration: f64,
        size: u64,
    ) -> MediaUpdateTransaction {
        let mut update = state.begin_update(load_id);
        update.set_filename(Some(filename.to_string()));
        update.set_path(Some(path.to_string()));
        update.set_duration(Some(duration));
        update.set_size(Some(size));
        update
    }

    #[test]
    fn metadata_from_separate_updates_is_not_mixed() {
        let mut state = MediaUpdateState::new();
        state.begin_load(1, "/media/a.mkv");

        let mut identity_update = state.begin_update(Some(1));
        identity_update.set_filename(Some("a.mkv".to_string()));
        identity_update.set_path(Some("/media/a.mkv".to_string()));
        assert_eq!(
            state.commit(identity_update),
            MediaCommit::Incomplete {
                load_id: Some(1),
                missing: vec![MediaField::Duration, MediaField::Size],
            }
        );

        let mut metadata_update = state.begin_update(Some(1));
        metadata_update.set_duration(Some(200.0));
        metadata_update.set_size(Some(2_000));
        assert_eq!(
            state.commit(metadata_update),
            MediaCommit::Incomplete {
                load_id: Some(1),
                missing: vec![MediaField::Filename, MediaField::Path],
            }
        );

        assert!(state.is_loading());
        assert!(state.ready_snapshot().is_none());
        assert!(state.last_confirmed().is_none());
    }

    #[test]
    fn late_a_completion_cannot_overwrite_latest_b_load() {
        let mut state = MediaUpdateState::new();
        state.begin_load(1, "/media/a.mkv");
        let update_a = complete_update(&state, Some(1), "a.mkv", "/media/a.mkv", 100.0, 1_000);

        state.begin_load(2, "/media/b.mkv");
        let update_b = complete_update(&state, Some(2), "b.mkv", "/media/b.mkv", 200.0, 2_000);

        assert_eq!(
            state.commit(update_a.clone()),
            MediaCommit::Stale {
                completed_load_id: Some(1),
                latest_load_id: Some(2),
            }
        );
        assert!(state.is_loading());
        assert!(state.ready_snapshot().is_none());

        let snapshot_b = match state.commit(update_b) {
            MediaCommit::Committed(snapshot) => snapshot,
            result => panic!("expected B to commit, got {result:?}"),
        };
        assert_eq!(snapshot_b.filename.as_deref(), Some("b.mkv"));

        assert_eq!(
            state.commit(update_a),
            MediaCommit::Stale {
                completed_load_id: Some(1),
                latest_load_id: None,
            }
        );
        assert_eq!(state.ready_snapshot(), Some(&snapshot_b));
    }

    #[test]
    fn explicit_generation_rejects_an_older_load_of_the_same_target() {
        let mut state = MediaUpdateState::new();
        state.begin_load(1, "/media/a.mkv");
        let update_1 = complete_update(&state, Some(1), "a.mkv", "/media/a.mkv", 100.0, 1_000);

        state.begin_load(2, "/media/a.mkv");
        let update_2 = complete_update(&state, Some(2), "a.mkv", "/media/a.mkv", 100.0, 1_000);

        assert!(matches!(
            state.commit(update_1),
            MediaCommit::Stale {
                completed_load_id: Some(1),
                latest_load_id: Some(2),
            }
        ));
        assert!(matches!(state.commit(update_2), MediaCommit::Committed(_)));
    }

    #[test]
    fn external_media_commits_only_without_a_pending_load() {
        let mut state = MediaUpdateState::new();
        let external = complete_update(
            &state,
            None,
            "external.mkv",
            "/external/external.mkv",
            300.0,
            3_000,
        );
        let external_snapshot = match state.commit(external) {
            MediaCommit::Committed(snapshot) => snapshot,
            result => panic!("expected external media to commit, got {result:?}"),
        };
        assert_eq!(state.ready_snapshot(), Some(&external_snapshot));

        state.begin_load(10, "/media/requested.mkv");
        let old_snapshot = complete_update(
            &state,
            None,
            "external.mkv",
            "/external/external.mkv",
            300.0,
            3_000,
        );
        assert_eq!(
            state.commit(old_snapshot),
            MediaCommit::Stale {
                completed_load_id: None,
                latest_load_id: Some(10),
            }
        );
        assert!(state.is_loading());
        assert!(state.ready_snapshot().is_none());
        assert_eq!(state.last_confirmed(), Some(&external_snapshot));
    }

    #[test]
    fn cancelled_load_keeps_a_tombstone_for_its_late_marker() {
        let mut state = MediaUpdateState::new();
        state.begin_load(7, "/media/late.mkv");
        let update = complete_update(&state, Some(7), "late.mkv", "/media/late.mkv", 100.0, 1_000);

        assert!(state.cancel_load(7));
        assert!(!state.is_loading());
        assert!(state.ready_snapshot().is_none());
        assert_eq!(
            state.commit(update),
            MediaCommit::Stale {
                completed_load_id: Some(7),
                latest_load_id: Some(7),
            }
        );
        assert!(state.latest_load().is_none());
        assert!(state.last_confirmed().is_none());
    }

    #[test]
    fn external_media_retires_a_cancelled_tombstone_but_late_generation_stays_stale() {
        let mut state = MediaUpdateState::new();
        state.begin_load(7, "/media/late.mkv");
        let late = complete_update(&state, Some(7), "late.mkv", "/media/late.mkv", 100.0, 1_000);
        assert!(state.cancel_load(7));

        let external = complete_update(
            &state,
            None,
            "external.mkv",
            "/media/external.mkv",
            200.0,
            2_000,
        );
        assert!(matches!(state.commit(external), MediaCommit::Committed(_)));
        assert_eq!(
            state.commit(late),
            MediaCommit::Stale {
                completed_load_id: Some(7),
                latest_load_id: None,
            }
        );
        assert_eq!(
            state
                .last_confirmed()
                .and_then(|media| media.filename.as_deref()),
            Some("external.mkv")
        );
    }

    #[test]
    fn target_matching_uses_the_resolved_media_identity() {
        let mut state = MediaUpdateState::new();
        state.begin_load(1, "/media/Movie-Name_1080p.mkv");
        let update = complete_update(
            &state,
            Some(1),
            "Movie-Name_1080p.mkv",
            "/media/Movie-Name_1080p.mkv",
            100.0,
            1_000,
        );

        assert!(matches!(state.commit(update), MediaCommit::Committed(_)));
    }

    #[test]
    fn fuzzy_playlist_name_cannot_complete_a_resolved_generation() {
        let mut state = MediaUpdateState::new();
        state.begin_load(1, "/media/Movie.Name [1080p].mkv");
        let update = complete_update(
            &state,
            Some(1),
            "Movie-Name_1080p.mkv",
            "/media/Movie-Name_1080p.mkv",
            100.0,
            1_000,
        );

        assert!(matches!(
            state.commit(update),
            MediaCommit::TargetMismatch { load_id: 1, .. }
        ));
    }

    #[test]
    fn urls_with_the_same_basename_are_distinct_media() {
        let mut state = MediaUpdateState::new();
        state.begin_load(1, "https://first.example/video.mkv");
        let update = complete_update(
            &state,
            Some(1),
            "https://second.example/video.mkv",
            "https://second.example/video.mkv",
            100.0,
            0,
        );

        assert!(matches!(
            state.commit(update),
            MediaCommit::TargetMismatch { load_id: 1, .. }
        ));
    }
}
