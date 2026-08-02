//! Pure state transitions for playlist-driven media loading.
//!
//! The reducer intentionally performs no I/O. Callers execute the returned
//! effects and feed committed player media back with the corresponding load ID.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LoadId(pub u64);

impl LoadId {
    const FIRST: Self = Self(1);

    fn next(self) -> Self {
        Self(
            self.0
                .checked_add(1)
                .expect("playback load ID space exhausted"),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommittedMedia {
    /// Canonical, unredacted name used to match an item in the playlist.
    pub name: String,
    pub size: Option<u64>,
    pub duration: Option<f64>,
}

impl CommittedMedia {
    pub fn new(name: impl Into<String>, size: Option<u64>, duration: Option<f64>) -> Self {
        Self {
            name: name.into(),
            size,
            duration,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadOrigin {
    /// A GUI selection or EOF advance. The index is announced after the file commits.
    Local,
    /// An authoritative server index. It must never be echoed back to the server.
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLoad {
    pub id: LoadId,
    pub target: String,
    pub playlist_index: usize,
    pub reset_position: bool,
    pub origin: LoadOrigin,
    pub status: PendingLoadStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingLoadStatus {
    Requested,
    Loading,
    WaitingForMedia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackState {
    pub playlist_items: Vec<String>,
    pub authoritative_index: Option<usize>,
    authoritative_target: Option<String>,
    pub proposed_index: Option<usize>,
    pub confirmed_media: Option<CommittedMedia>,
    pub pending_load: Option<PendingLoad>,
    pub interrupted_load: Option<PendingLoad>,
    pub next_load_id: LoadId,
    pub received_server_index: bool,
    pub media_uncertain: bool,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl PlaybackState {
    pub fn new(playlist_items: Vec<String>) -> Self {
        Self {
            playlist_items,
            authoritative_index: None,
            authoritative_target: None,
            proposed_index: None,
            confirmed_media: None,
            pending_load: None,
            interrupted_load: None,
            next_load_id: LoadId::FIRST,
            received_server_index: false,
            media_uncertain: false,
        }
    }

    pub fn reduce(&mut self, event: PlaybackEvent) -> Vec<PlaybackEffect> {
        match event {
            PlaybackEvent::LocalSelect {
                index,
                reset_position,
            } => self.local_select(index, reset_position),
            PlaybackEvent::ServerPlaylist { items } => {
                self.apply_server_playlist(items);
                Vec::new()
            }
            #[cfg(test)]
            PlaybackEvent::ReplacePlaylist { items, index } => {
                self.replace_playlist(items, index);
                Vec::new()
            }
            PlaybackEvent::LocalPlaylistEdit { items, index } => {
                self.apply_local_playlist_edit(items, index);
                Vec::new()
            }
            PlaybackEvent::ServerIndex {
                index,
                reset_position,
            } => self.apply_server_index(index, reset_position),
            PlaybackEvent::ServerIndexObserved { index } => {
                self.record_server_index(index);
                Vec::new()
            }
            PlaybackEvent::ReconcileSharedPlaylist => self.reconcile_shared_playlist(),
            PlaybackEvent::PlayerMediaCommitted { load_id, media } => {
                self.commit_player_media(load_id, media)
            }
            PlaybackEvent::LoadFailed { load_id } => {
                if self.pending_load.as_ref().map(|pending| pending.id) == Some(load_id) {
                    self.media_uncertain |= self
                        .pending_load
                        .as_ref()
                        .is_some_and(|pending| pending.status == PendingLoadStatus::Loading);
                    self.pending_load = None;
                }
                if self
                    .interrupted_load
                    .as_ref()
                    .is_some_and(|pending| pending.id == load_id)
                {
                    self.interrupted_load = None;
                }
                Vec::new()
            }
            PlaybackEvent::LoadStarted { load_id } => {
                if self.pending_load.as_ref().map(|pending| pending.id) == Some(load_id) {
                    self.interrupted_load = None;
                    if let Some(pending) = self.pending_load.as_mut() {
                        pending.status = PendingLoadStatus::Loading;
                    }
                }
                Vec::new()
            }
            PlaybackEvent::LoadDeferred { load_id } => {
                if let Some(pending) = self
                    .pending_load
                    .as_mut()
                    .filter(|pending| pending.id == load_id)
                {
                    pending.status = PendingLoadStatus::WaitingForMedia;
                }
                Vec::new()
            }
            PlaybackEvent::RetryPending => self.retry_pending(),
            PlaybackEvent::Reconnect => {
                let restore_index = self.displayed_index();
                self.interrupt_loading();
                self.authoritative_index = restore_index;
                self.authoritative_target =
                    restore_index.and_then(|index| self.playlist_items.get(index).cloned());
                self.proposed_index = None;
                self.received_server_index = false;
                Vec::new()
            }
            PlaybackEvent::PlayerDisconnected => {
                self.media_uncertain |= self.confirmed_media.is_some();
                self.pending_load = None;
                self.interrupted_load = None;
                Vec::new()
            }
            PlaybackEvent::Reset => {
                let next_load_id = self.next_load_id;
                *self = Self::default();
                self.next_load_id = next_load_id;
                Vec::new()
            }
        }
    }

    #[cfg(test)]
    pub fn replace_playlist(&mut self, items: Vec<String>, index: Option<usize>) {
        self.playlist_items = items;
        self.authoritative_index = index.filter(|index| *index < self.playlist_items.len());
        self.authoritative_target = self
            .authoritative_index
            .and_then(|index| self.playlist_items.get(index).cloned());
        self.proposed_index = None;
        self.remap_pending_load();
    }

    fn apply_local_playlist_edit(&mut self, items: Vec<String>, index: Option<usize>) {
        self.replace_playlist_preserving_authority(items);
        self.proposed_index = index.filter(|index| *index < self.playlist_items.len());
        self.remap_pending_load();
    }

    fn replace_playlist_preserving_authority(&mut self, items: Vec<String>) {
        let authoritative_target = self.authoritative_target.clone();
        self.playlist_items = items;
        self.authoritative_index = authoritative_target
            .as_ref()
            .and_then(|target| self.playlist_items.iter().position(|item| item == target));
        self.authoritative_target = self
            .authoritative_index
            .and_then(|index| self.playlist_items.get(index).cloned());
    }

    fn remap_pending_load(&mut self) {
        if let Some(pending) = self.pending_load.as_mut() {
            if let Some(index) = self
                .playlist_items
                .iter()
                .position(|item| item == &pending.target)
            {
                pending.playlist_index = index;
            } else {
                self.interrupt_loading();
            }
        }
    }

    pub fn displayed_index(&self) -> Option<usize> {
        self.pending_load
            .as_ref()
            .filter(|pending| pending.origin == LoadOrigin::Local)
            .map(|pending| pending.playlist_index)
            .or(self
                .proposed_index
                .filter(|index| *index < self.playlist_items.len()))
            .or(self
                .authoritative_index
                .filter(|index| *index < self.playlist_items.len()))
    }

    fn local_select(&mut self, index: usize, reset_position: bool) -> Vec<PlaybackEffect> {
        let Some(target) = self.playlist_items.get(index).cloned() else {
            return Vec::new();
        };

        if let Some(pending) = self.pending_load.as_ref() {
            if pending.target == target {
                if pending.status == PendingLoadStatus::WaitingForMedia {
                    return vec![self.begin_load(index, target, reset_position, LoadOrigin::Local)];
                }
                return Vec::new();
            }
            return vec![self.begin_load(index, target, reset_position, LoadOrigin::Local)];
        }

        if !self.media_uncertain
            && self
                .confirmed_media
                .as_ref()
                .is_some_and(|media| media.name == target)
        {
            if !self.authoritative_matches(index, &target) && self.proposed_index != Some(index) {
                self.proposed_index = Some(index);
                return vec![PlaybackEffect::SendPlaylistIndex {
                    index,
                    reset_position,
                }];
            }
            return Vec::new();
        }

        vec![self.begin_load(index, target, reset_position, LoadOrigin::Local)]
    }

    fn apply_server_playlist(&mut self, items: Vec<String>) {
        let displayed_target = self
            .displayed_index()
            .and_then(|index| self.playlist_items.get(index).cloned());
        self.replace_playlist_preserving_authority(items);
        self.proposed_index = displayed_target.as_ref().and_then(|target| {
            self.playlist_items
                .iter()
                .position(|item| item == target)
                .filter(|index| !self.authoritative_matches(*index, target))
        });
        self.remap_pending_load();
    }

    fn apply_server_index(
        &mut self,
        index: Option<usize>,
        reset_position: bool,
    ) -> Vec<PlaybackEffect> {
        let reset_position = self.received_server_index && reset_position;
        let Some((index, target)) = self.record_server_index(index) else {
            return Vec::new();
        };
        self.load_server_selection(index, target, reset_position)
    }

    fn record_server_index(&mut self, index: Option<usize>) -> Option<(usize, String)> {
        self.received_server_index = true;
        let index = index?;
        self.proposed_index = None;
        let Some(target) = self.playlist_items.get(index).cloned() else {
            self.authoritative_index = None;
            self.authoritative_target = None;
            self.interrupt_loading();
            return None;
        };
        self.authoritative_index = Some(index);
        self.authoritative_target = Some(target.clone());
        Some((index, target))
    }

    fn reconcile_shared_playlist(&mut self) -> Vec<PlaybackEffect> {
        self.proposed_index = None;
        let Some(index) = self.authoritative_index else {
            return Vec::new();
        };
        let Some(target) = self.playlist_items.get(index).cloned() else {
            return Vec::new();
        };
        self.authoritative_target = Some(target.clone());
        self.load_server_selection(index, target, false)
    }

    fn load_server_selection(
        &mut self,
        index: usize,
        target: String,
        reset_position: bool,
    ) -> Vec<PlaybackEffect> {
        if let Some(pending) = self.pending_load.as_mut() {
            if pending.target == target {
                if pending.status == PendingLoadStatus::WaitingForMedia {
                    return vec![self.begin_load(
                        index,
                        target,
                        reset_position,
                        LoadOrigin::Server,
                    )];
                }
                pending.playlist_index = index;
                pending.reset_position |= reset_position;
                pending.origin = LoadOrigin::Server;
                return Vec::new();
            }
            return vec![self.begin_load(index, target, reset_position, LoadOrigin::Server)];
        }

        if self
            .interrupted_load
            .as_ref()
            .is_some_and(|pending| pending.target == target)
        {
            let mut pending = self
                .interrupted_load
                .take()
                .expect("matching interrupted load disappeared");
            pending.playlist_index = index;
            pending.origin = LoadOrigin::Server;
            pending.reset_position |= reset_position;
            self.pending_load = Some(pending);
            return Vec::new();
        }

        if !self.media_uncertain
            && self
                .confirmed_media
                .as_ref()
                .is_some_and(|media| media.name == target)
        {
            return Vec::new();
        }

        vec![self.begin_load(index, target, reset_position, LoadOrigin::Server)]
    }

    fn authoritative_matches(&self, index: usize, target: &str) -> bool {
        self.authoritative_index == Some(index)
            && self.authoritative_target.as_deref() == Some(target)
    }

    fn commit_player_media(
        &mut self,
        load_id: Option<LoadId>,
        media: CommittedMedia,
    ) -> Vec<PlaybackEffect> {
        let Some(load_id) = load_id else {
            return self.commit_external_media(media);
        };
        let Some(pending) = self.pending_load.as_ref() else {
            if self
                .interrupted_load
                .as_ref()
                .is_some_and(|pending| pending.id == load_id)
            {
                self.interrupted_load = None;
                self.confirmed_media = Some(media.clone());
                self.media_uncertain = false;
                return vec![PlaybackEffect::SendFile { media }];
            }
            return Vec::new();
        };
        if pending.id != load_id {
            if self
                .interrupted_load
                .as_ref()
                .is_some_and(|pending| pending.id == load_id)
            {
                self.interrupted_load = None;
                self.confirmed_media = Some(media.clone());
                self.media_uncertain = false;
                return vec![PlaybackEffect::SendFile { media }];
            }
            return Vec::new();
        }

        let pending = self
            .pending_load
            .take()
            .expect("pending load disappeared during media commit");
        self.confirmed_media = Some(media.clone());
        self.media_uncertain = false;

        let committed_playlist_index = self
            .playlist_items
            .iter()
            .position(|item| item == &media.name)
            .or_else(|| {
                self.playlist_items
                    .get(pending.playlist_index)
                    .filter(|item| *item == &pending.target)
                    .map(|_| pending.playlist_index)
            });
        let mut effects = vec![PlaybackEffect::SendFile { media }];
        if let Some(index) =
            committed_playlist_index.filter(|_| pending.origin == LoadOrigin::Local)
        {
            let target = self
                .playlist_items
                .get(index)
                .expect("committed playlist index disappeared");
            if self.authoritative_matches(index, target) {
                return effects;
            }
            self.proposed_index = Some(index);
            effects.push(PlaybackEffect::SendPlaylistIndex {
                index,
                reset_position: pending.reset_position,
            });
        }
        effects
    }

    fn commit_external_media(&mut self, media: CommittedMedia) -> Vec<PlaybackEffect> {
        // An untagged observation during an active load is ambiguous and cannot
        // supersede a generation-tagged request safely.
        if self.pending_load.is_some() || self.interrupted_load.is_some() {
            return Vec::new();
        }
        if self.confirmed_media.as_ref() == Some(&media) {
            self.media_uncertain = false;
            self.proposed_index = self
                .playlist_items
                .iter()
                .position(|item| item == &media.name)
                .filter(|index| !self.authoritative_matches(*index, &media.name));
            return Vec::new();
        }

        let playlist_index = self
            .playlist_items
            .iter()
            .position(|item| item == &media.name);
        self.confirmed_media = Some(media.clone());
        self.media_uncertain = false;
        self.proposed_index =
            playlist_index.filter(|index| !self.authoritative_matches(*index, &media.name));

        let mut effects = vec![PlaybackEffect::SendFile { media }];
        if let Some(index) = self.proposed_index {
            effects.push(PlaybackEffect::SendPlaylistIndex {
                index,
                reset_position: true,
            });
        }
        effects
    }

    fn begin_load(
        &mut self,
        playlist_index: usize,
        target: String,
        reset_position: bool,
        origin: LoadOrigin,
    ) -> PlaybackEffect {
        let load_id = self.next_load_id;
        self.next_load_id = load_id.next();
        if self
            .pending_load
            .as_ref()
            .is_some_and(|pending| pending.status == PendingLoadStatus::Loading)
        {
            self.media_uncertain = true;
            self.interrupted_load = self.pending_load.take();
        } else {
            self.pending_load = None;
        }
        self.pending_load = Some(PendingLoad {
            id: load_id,
            target: target.clone(),
            playlist_index,
            reset_position,
            origin,
            status: PendingLoadStatus::Requested,
        });
        PlaybackEffect::Load {
            load_id,
            target,
            reset_position,
        }
    }

    fn interrupt_loading(&mut self) {
        let pending = self.pending_load.take();
        if let Some(pending) =
            pending.filter(|pending| pending.status == PendingLoadStatus::Loading)
        {
            self.media_uncertain = true;
            self.interrupted_load = Some(pending);
        }
    }

    fn retry_pending(&mut self) -> Vec<PlaybackEffect> {
        let Some(pending) = self
            .pending_load
            .as_ref()
            .filter(|pending| pending.status == PendingLoadStatus::WaitingForMedia)
            .cloned()
        else {
            return Vec::new();
        };
        vec![self.begin_load(
            pending.playlist_index,
            pending.target,
            pending.reset_position,
            pending.origin,
        )]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackEvent {
    /// GUI selection and EOF advancement both load first and announce on commit.
    LocalSelect {
        index: usize,
        reset_position: bool,
    },
    ServerPlaylist {
        items: Vec<String>,
    },
    #[cfg(test)]
    ReplacePlaylist {
        items: Vec<String>,
        index: Option<usize>,
    },
    /// A local content edit keeps server authority separate from the desired index.
    LocalPlaylistEdit {
        items: Vec<String>,
        index: Option<usize>,
    },
    ServerIndex {
        index: Option<usize>,
        reset_position: bool,
    },
    /// Server authority is always observed, even when shared playlist loading is disabled.
    ServerIndexObserved {
        index: Option<usize>,
    },
    /// Reload the last server-authoritative selection after shared playlists are enabled.
    ReconcileSharedPlaylist,
    /// `None` is reserved for a stable file change initiated outside Syncplay.
    PlayerMediaCommitted {
        load_id: Option<LoadId>,
        media: CommittedMedia,
    },
    LoadFailed {
        load_id: LoadId,
    },
    LoadStarted {
        load_id: LoadId,
    },
    LoadDeferred {
        load_id: LoadId,
    },
    RetryPending,
    Reconnect,
    PlayerDisconnected,
    Reset,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackEffect {
    Load {
        load_id: LoadId,
        target: String,
        reset_position: bool,
    },
    SendFile {
        media: CommittedMedia,
    },
    SendPlaylistIndex {
        index: usize,
        reset_position: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(name: &str) -> CommittedMedia {
        CommittedMedia::new(name, Some(1), Some(120.0))
    }

    fn state_with_playlist() -> PlaybackState {
        let mut state = PlaybackState::new(vec![
            "a.mkv".to_string(),
            "b.mkv".to_string(),
            "c.mkv".to_string(),
        ]);
        state.authoritative_index = Some(0);
        state.authoritative_target = Some("a.mkv".to_string());
        state.confirmed_media = Some(media("a.mkv"));
        state
    }

    fn load_id(effects: &[PlaybackEffect]) -> LoadId {
        match effects {
            [PlaybackEffect::Load { load_id, .. }] => *load_id,
            other => panic!("expected one load effect, got {other:?}"),
        }
    }

    #[test]
    fn local_selection_loads_before_announcing_index() {
        let mut state = state_with_playlist();

        let effects = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let id = load_id(&effects);

        assert_eq!(state.authoritative_index, Some(0));
        assert_eq!(
            effects,
            vec![PlaybackEffect::Load {
                load_id: id,
                target: "b.mkv".to_string(),
                reset_position: true,
            }]
        );

        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(id),
            media: media("b.mkv"),
        });

        assert_eq!(
            effects,
            vec![
                PlaybackEffect::SendFile {
                    media: media("b.mkv"),
                },
                PlaybackEffect::SendPlaylistIndex {
                    index: 1,
                    reset_position: true,
                },
            ]
        );
        assert_eq!(state.confirmed_media, Some(media("b.mkv")));
        assert!(state.pending_load.is_none());
    }

    #[test]
    fn server_index_loads_without_echoing_index() {
        let mut state = state_with_playlist();

        let effects = state.reduce(PlaybackEvent::ServerIndex {
            index: Some(1),
            reset_position: true,
        });
        let id = load_id(&effects);
        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(id),
            media: media("b.mkv"),
        });

        assert_eq!(
            effects,
            vec![PlaybackEffect::SendFile {
                media: media("b.mkv"),
            }]
        );
        assert_eq!(state.authoritative_index, Some(1));
    }

    #[test]
    fn first_server_index_never_resets_position() {
        let mut state = PlaybackState::new(vec!["a.mkv".into(), "b.mkv".into()]);

        let effects = state.reduce(PlaybackEvent::ServerIndex {
            index: Some(1),
            reset_position: true,
        });

        assert!(matches!(
            effects.as_slice(),
            [PlaybackEffect::Load {
                target,
                reset_position: false,
                ..
            }] if target == "b.mkv"
        ));
    }

    #[test]
    fn null_server_index_consumes_first_index_without_loading() {
        let mut state = PlaybackState::new(vec!["a.mkv".into(), "b.mkv".into()]);

        assert!(state
            .reduce(PlaybackEvent::ServerIndex {
                index: None,
                reset_position: true,
            })
            .is_empty());
        let effects = state.reduce(PlaybackEvent::ServerIndex {
            index: Some(1),
            reset_position: true,
        });

        assert!(matches!(
            effects.as_slice(),
            [PlaybackEffect::Load {
                target,
                reset_position: true,
                ..
            }] if target == "b.mkv"
        ));
    }

    #[test]
    fn observed_null_index_consumes_first_index_while_loading_is_disabled() {
        let mut state = PlaybackState::new(vec!["a.mkv".into(), "b.mkv".into()]);

        state.reduce(PlaybackEvent::ServerIndexObserved { index: None });
        let effects = state.reduce(PlaybackEvent::ServerIndex {
            index: Some(1),
            reset_position: true,
        });

        assert!(matches!(
            effects.as_slice(),
            [PlaybackEffect::Load {
                target,
                reset_position: true,
                ..
            }] if target == "b.mkv"
        ));
    }

    #[test]
    fn local_commit_keeps_proposed_index_visible_until_server_echo() {
        let mut state = state_with_playlist();
        let effects = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let id = load_id(&effects);

        state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(id),
            media: media("b.mkv"),
        });

        assert_eq!(state.authoritative_index, Some(0));
        assert_eq!(state.proposed_index, Some(1));
        assert_eq!(state.displayed_index(), Some(1));

        state.reduce(PlaybackEvent::ServerIndex {
            index: Some(1),
            reset_position: true,
        });
        assert_eq!(state.authoritative_index, Some(1));
        assert_eq!(state.proposed_index, None);
        assert_eq!(state.displayed_index(), Some(1));
    }

    #[test]
    fn local_playlist_edit_keeps_pending_selection_separate_from_server_authority() {
        let mut state = state_with_playlist();
        let load = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let id = load_id(&load);

        state.reduce(PlaybackEvent::LocalPlaylistEdit {
            items: vec![
                "a.mkv".into(),
                "b.mkv".into(),
                "c.mkv".into(),
                "d.mkv".into(),
            ],
            index: Some(1),
        });
        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(id),
            media: media("b.mkv"),
        });

        assert!(effects
            .iter()
            .any(|effect| matches!(effect, PlaybackEffect::SendPlaylistIndex { index: 1, .. })));
        assert_eq!(state.authoritative_index, Some(0));
    }

    #[test]
    fn server_playlist_reorder_preserves_displayed_target_identity() {
        let mut state = state_with_playlist();
        let effects = state.reduce(PlaybackEvent::ServerIndex {
            index: Some(1),
            reset_position: false,
        });
        let id = load_id(&effects);
        state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(id),
            media: media("b.mkv"),
        });

        state.reduce(PlaybackEvent::ServerPlaylist {
            items: vec!["b.mkv".into(), "c.mkv".into()],
        });

        assert_eq!(state.displayed_index(), Some(0));
        assert_eq!(state.authoritative_index, Some(0));
    }

    #[test]
    fn playlist_identity_does_not_collapse_fuzzy_filename_matches() {
        let mut state = PlaybackState::new(vec![
            "Episode-01.mkv".to_string(),
            "Episode 01.mkv".to_string(),
        ]);
        state.authoritative_index = Some(0);
        state.authoritative_target = Some("Episode-01.mkv".to_string());
        state.confirmed_media = Some(media("Episode-01.mkv"));

        let effects = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });

        assert!(matches!(
            effects.as_slice(),
            [PlaybackEffect::Load { target, .. }] if target == "Episode 01.mkv"
        ));
    }

    #[test]
    fn server_playlist_remaps_authority_by_exact_target() {
        let mut state = PlaybackState::new(vec![
            "Episode-01.mkv".to_string(),
            "Episode 01.mkv".to_string(),
        ]);
        state.authoritative_index = Some(0);
        state.authoritative_target = Some("Episode-01.mkv".to_string());

        state.reduce(PlaybackEvent::ServerPlaylist {
            items: vec!["Episode 01.mkv".into(), "Episode-01.mkv".into()],
        });

        assert_eq!(state.authoritative_index, Some(1));
        assert_eq!(state.displayed_index(), Some(1));
    }

    #[test]
    fn local_commit_publishes_the_exact_observed_playlist_item() {
        let mut state = PlaybackState::new(vec![
            "Episode-01.mkv".to_string(),
            "Episode 01.mkv".to_string(),
        ]);
        let load = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let id = load_id(&load);

        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(id),
            media: media("Episode-01.mkv"),
        });

        assert!(matches!(
            effects.as_slice(),
            [
                PlaybackEffect::SendFile { .. },
                PlaybackEffect::SendPlaylistIndex { index: 0, .. }
            ]
        ));
        assert_eq!(state.proposed_index, Some(0));
    }

    #[test]
    fn local_commit_keeps_the_requested_index_for_a_fuzzy_resolved_file() {
        let mut state = PlaybackState::new(vec!["Episode 01.mkv".to_string()]);
        let load = state.reduce(PlaybackEvent::LocalSelect {
            index: 0,
            reset_position: true,
        });
        let id = load_id(&load);

        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(id),
            media: media("Episode-01.mkv"),
        });

        assert!(matches!(
            effects.as_slice(),
            [
                PlaybackEffect::SendFile { .. },
                PlaybackEffect::SendPlaylistIndex { index: 0, .. }
            ]
        ));
        assert_eq!(state.displayed_index(), Some(0));
    }

    #[test]
    fn rapid_local_selections_are_latest_wins() {
        let mut state = state_with_playlist();
        let first = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let first_id = load_id(&first);
        let second = state.reduce(PlaybackEvent::LocalSelect {
            index: 2,
            reset_position: true,
        });
        let second_id = load_id(&second);

        assert!(second_id > first_id);
        assert!(state
            .reduce(PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(first_id),
                media: media("b.mkv"),
            })
            .is_empty());
        assert_eq!(state.confirmed_media, Some(media("a.mkv")));

        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(second_id),
            media: media("c.mkv"),
        });
        assert_eq!(
            effects,
            vec![
                PlaybackEffect::SendFile {
                    media: media("c.mkv"),
                },
                PlaybackEffect::SendPlaylistIndex {
                    index: 2,
                    reset_position: true,
                },
            ]
        );
    }

    #[test]
    fn reselecting_confirmed_media_supersedes_an_inflight_local_load() {
        let mut state = state_with_playlist();
        let load_b = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let load_b_id = load_id(&load_b);

        let load_a = state.reduce(PlaybackEvent::LocalSelect {
            index: 0,
            reset_position: true,
        });
        let load_a_id = load_id(&load_a);

        assert!(load_a_id > load_b_id);
        assert!(matches!(
            load_a.as_slice(),
            [PlaybackEffect::Load { target, .. }] if target == "a.mkv"
        ));
    }

    #[test]
    fn server_reselecting_confirmed_media_supersedes_an_inflight_load() {
        let mut state = state_with_playlist();
        state.received_server_index = true;
        let load_b = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let load_b_id = load_id(&load_b);

        let load_a = state.reduce(PlaybackEvent::ServerIndex {
            index: Some(0),
            reset_position: true,
        });
        let load_a_id = load_id(&load_a);

        assert!(load_a_id > load_b_id);
        assert!(matches!(
            load_a.as_slice(),
            [PlaybackEffect::Load {
                target,
                reset_position: true,
                ..
            }] if target == "a.mkv"
        ));
    }

    #[test]
    fn tagged_media_commit_trusts_the_player_generation_boundary() {
        let mut state = state_with_playlist();
        let effects = state.reduce(PlaybackEvent::LocalSelect {
            index: 2,
            reset_position: true,
        });
        let id = load_id(&effects);

        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(id),
            media: media("b.mkv"),
        });

        assert!(matches!(
            effects.as_slice(),
            [
                PlaybackEffect::SendFile { .. },
                PlaybackEffect::SendPlaylistIndex { index: 1, .. }
            ]
        ));
        assert_eq!(state.confirmed_media, Some(media("b.mkv")));
        assert!(state.pending_load.is_none());
    }

    #[test]
    fn current_load_failure_allows_the_same_selection_to_retry() {
        let mut state = state_with_playlist();
        let first = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let first_id = load_id(&first);

        state.reduce(PlaybackEvent::LoadFailed { load_id: first_id });
        let retry = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });

        assert!(load_id(&retry) > first_id);
    }

    #[test]
    fn media_scan_retries_only_a_deferred_load() {
        let mut state = state_with_playlist();
        let first = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let first_id = load_id(&first);

        assert!(state.reduce(PlaybackEvent::RetryPending).is_empty());
        state.reduce(PlaybackEvent::LoadDeferred { load_id: first_id });
        let retry = state.reduce(PlaybackEvent::RetryPending);

        assert!(load_id(&retry) > first_id);
    }

    #[test]
    fn server_echo_for_confirmed_media_does_not_reload() {
        let mut state = state_with_playlist();
        state.confirmed_media = Some(media("b.mkv"));

        let effects = state.reduce(PlaybackEvent::ServerIndex {
            index: Some(1),
            reset_position: true,
        });

        assert!(effects.is_empty());
        assert_eq!(state.authoritative_index, Some(1));
        assert!(state.pending_load.is_none());
    }

    #[test]
    fn server_index_adopts_matching_local_load_without_future_echo() {
        let mut state = state_with_playlist();
        let effects = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let id = load_id(&effects);

        assert!(state
            .reduce(PlaybackEvent::ServerIndex {
                index: Some(1),
                reset_position: true,
            })
            .is_empty());
        assert_eq!(
            state.pending_load.as_ref().map(|load| load.origin),
            Some(LoadOrigin::Server)
        );

        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(id),
            media: media("b.mkv"),
        });
        assert_eq!(
            effects,
            vec![PlaybackEffect::SendFile {
                media: media("b.mkv"),
            }]
        );
    }

    #[test]
    fn reconnect_accepts_only_the_exact_interrupted_load() {
        let mut state = state_with_playlist();
        let effects = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let stale_id = load_id(&effects);
        state.reduce(PlaybackEvent::LoadStarted { load_id: stale_id });

        assert!(state.reduce(PlaybackEvent::Reconnect).is_empty());
        assert!(state.pending_load.is_none());
        assert_eq!(
            state.interrupted_load.as_ref().map(|load| load.id),
            Some(stale_id)
        );
        assert_eq!(state.authoritative_index, Some(1));
        assert_eq!(state.confirmed_media, Some(media("a.mkv")));
        assert_eq!(state.playlist_items, vec!["a.mkv", "b.mkv", "c.mkv"]);
        assert_eq!(
            state.reduce(PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(stale_id),
                media: media("b.mkv"),
            }),
            vec![PlaybackEffect::SendFile {
                media: media("b.mkv"),
            }]
        );

        let effects = state.reduce(PlaybackEvent::LocalSelect {
            index: 2,
            reset_position: true,
        });
        assert!(load_id(&effects) > stale_id);
    }

    #[test]
    fn reconnect_rejects_unrelated_tagged_commit() {
        let mut state = state_with_playlist();
        let effects = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let interrupted_id = load_id(&effects);
        state.reduce(PlaybackEvent::LoadStarted {
            load_id: interrupted_id,
        });
        state.reduce(PlaybackEvent::Reconnect);

        assert!(state
            .reduce(PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(LoadId(interrupted_id.0 + 100)),
                media: media("b.mkv"),
            })
            .is_empty());
        assert_eq!(state.confirmed_media, Some(media("a.mkv")));
        assert!(state.media_uncertain);
    }

    #[test]
    fn interrupted_commit_is_reported_until_the_replacement_is_issued() {
        let mut state = state_with_playlist();
        let first = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let interrupted_id = load_id(&first);
        state.reduce(PlaybackEvent::LoadStarted {
            load_id: interrupted_id,
        });
        state.reduce(PlaybackEvent::Reconnect);

        let replacement = state.reduce(PlaybackEvent::ServerIndex {
            index: Some(2),
            reset_position: true,
        });
        let replacement_id = load_id(&replacement);
        assert_eq!(
            state.interrupted_load.as_ref().map(|load| load.id),
            Some(interrupted_id)
        );

        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(interrupted_id),
            media: media("b.mkv"),
        });
        assert_eq!(
            effects,
            vec![PlaybackEffect::SendFile {
                media: media("b.mkv")
            }]
        );
        assert!(state.interrupted_load.is_none());
        assert_eq!(
            state.pending_load.as_ref().map(|load| load.id),
            Some(replacement_id)
        );
    }

    #[test]
    fn player_disconnect_rejects_interrupted_generation() {
        let mut state = state_with_playlist();
        let first = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let id = load_id(&first);
        state.reduce(PlaybackEvent::LoadStarted { load_id: id });
        state.reduce(PlaybackEvent::Reconnect);
        state.reduce(PlaybackEvent::PlayerDisconnected);

        assert!(state
            .reduce(PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(id),
                media: media("b.mkv"),
            })
            .is_empty());
        assert_eq!(state.confirmed_media, Some(media("a.mkv")));
    }

    #[test]
    fn reconnect_during_load_revalidates_the_next_authoritative_index() {
        let mut state = state_with_playlist();
        let load = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        state.reduce(PlaybackEvent::LoadStarted {
            load_id: load_id(&load),
        });

        state.reduce(PlaybackEvent::Reconnect);
        let effects = state.reduce(PlaybackEvent::ServerIndex {
            index: Some(0),
            reset_position: true,
        });

        assert!(state.media_uncertain);
        assert!(matches!(
            effects.as_slice(),
            [PlaybackEffect::Load {
                target,
                reset_position: false,
                ..
            }] if target == "a.mkv"
        ));
    }

    #[test]
    fn external_media_is_announced_once_when_idle() {
        let mut state = state_with_playlist();

        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: None,
            media: media("b.mkv"),
        });
        assert_eq!(
            effects,
            vec![
                PlaybackEffect::SendFile {
                    media: media("b.mkv"),
                },
                PlaybackEffect::SendPlaylistIndex {
                    index: 1,
                    reset_position: true,
                },
            ]
        );
        assert!(state
            .reduce(PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: media("b.mkv"),
            })
            .is_empty());
    }

    #[test]
    fn matching_external_observation_clears_uncertainty_without_resending() {
        let mut state = state_with_playlist();
        state.media_uncertain = true;

        assert!(state
            .reduce(PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: media("a.mkv"),
            })
            .is_empty());
        assert!(!state.media_uncertain);
    }

    #[test]
    fn external_media_recomputes_proposed_index() {
        let mut state = state_with_playlist();
        state.proposed_index = Some(1);

        state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: None,
            media: media("a.mkv"),
        });
        assert_eq!(state.proposed_index, None);
        assert_eq!(state.displayed_index(), Some(0));

        state.proposed_index = Some(1);
        state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: None,
            media: media("outside.mkv"),
        });
        assert_eq!(state.proposed_index, None);
        assert_eq!(state.displayed_index(), Some(0));
    }

    #[test]
    fn playlist_reorder_remaps_pending_local_index() {
        let mut state = state_with_playlist();
        let effects = state.reduce(PlaybackEvent::LocalSelect {
            index: 1,
            reset_position: true,
        });
        let id = load_id(&effects);

        state.reduce(PlaybackEvent::ServerPlaylist {
            items: vec![
                "b.mkv".to_string(),
                "a.mkv".to_string(),
                "c.mkv".to_string(),
            ],
        });
        assert_eq!(
            state
                .pending_load
                .as_ref()
                .map(|pending| pending.playlist_index),
            Some(0)
        );
        let effects = state.reduce(PlaybackEvent::PlayerMediaCommitted {
            load_id: Some(id),
            media: media("b.mkv"),
        });

        assert_eq!(
            effects,
            vec![
                PlaybackEffect::SendFile {
                    media: media("b.mkv"),
                },
                PlaybackEffect::SendPlaylistIndex {
                    index: 0,
                    reset_position: true,
                },
            ]
        );
    }
}
