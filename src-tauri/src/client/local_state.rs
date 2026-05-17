const SEEK_THRESHOLD: f64 = 1.0;
const REMOTE_SEEK_SUPPRESSION_THRESHOLD: f64 = 1.5;

#[derive(Debug, Clone)]
pub struct LocalPlaybackState {
    position: f64,
    paused: bool,
    initialized: bool,
    suppress_seek_until_position: Option<f64>,
}

impl LocalPlaybackState {
    pub fn new() -> Self {
        Self {
            position: 0.0,
            paused: true,
            initialized: false,
            suppress_seek_until_position: None,
        }
    }

    pub fn mark_remote_seek(&mut self, position: f64) {
        self.suppress_seek_until_position = Some(position);
    }

    pub fn update_from_player(
        &mut self,
        position: f64,
        paused: bool,
        global_position: f64,
        global_paused: bool,
    ) -> (bool, bool) {
        let pause_change = self.initialized && self.paused != paused && global_paused != paused;
        let player_diff = if self.initialized {
            (self.position - position).abs()
        } else {
            0.0
        };
        let global_diff = (global_position - position).abs();
        let mut seeked =
            self.initialized && player_diff > SEEK_THRESHOLD && global_diff > SEEK_THRESHOLD;

        if seeked {
            if let Some(remote_position) = self.suppress_seek_until_position {
                if (position - remote_position).abs() <= REMOTE_SEEK_SUPPRESSION_THRESHOLD
                    || (position - global_position).abs() <= REMOTE_SEEK_SUPPRESSION_THRESHOLD
                {
                    seeked = false;
                }
                self.suppress_seek_until_position = None;
            }
        } else if let Some(remote_position) = self.suppress_seek_until_position {
            if (position - remote_position).abs() <= REMOTE_SEEK_SUPPRESSION_THRESHOLD
                || (position - global_position).abs() <= REMOTE_SEEK_SUPPRESSION_THRESHOLD
                || player_diff <= SEEK_THRESHOLD
            {
                self.suppress_seek_until_position = None;
            }
        }

        self.position = position;
        self.paused = paused;
        self.initialized = true;

        (pause_change, seeked)
    }

    pub fn current(&self) -> Option<(f64, bool)> {
        if self.initialized {
            Some((self.position, self.paused))
        } else {
            None
        }
    }

    pub fn compute_seeked(&self, position: f64, global_position: f64) -> bool {
        if !self.initialized || self.suppress_seek_until_position.is_some() {
            return false;
        }
        let player_diff = (self.position - position).abs();
        let global_diff = (global_position - position).abs();
        player_diff > SEEK_THRESHOLD && global_diff > SEEK_THRESHOLD
    }
}

impl Default for LocalPlaybackState {
    fn default() -> Self {
        Self::new()
    }
}
