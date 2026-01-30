use crate::config::UserPreferences;
use tracing::{debug, info};

const FASTFORWARD_EXTRA_TIME: f64 = 0.25;
const FASTFORWARD_RESET_THRESHOLD: f64 = 3.0;
const FASTFORWARD_BEHIND_THRESHOLD: f64 = 1.75;

/// Synchronization action to take
#[derive(Debug, Clone, PartialEq)]
pub enum SyncAction {
    /// No action needed
    None,
    /// Seek to position
    Seek(f64),
    /// Set pause state
    SetPaused(bool),
    /// Apply slowdown
    Slowdown,
    /// Reset speed to normal
    ResetSpeed,
}

pub struct SyncInputs {
    pub local_position: f64,
    pub local_paused: bool,
    pub global_position: f64,
    pub global_paused: bool,
    pub message_age: f64,
    pub do_seek: bool,
    pub allow_fastforward: bool,
}

/// Synchronization engine
pub struct SyncEngine {
    /// Whether slowdown is currently active
    slowdown_active: bool,
    behind_first_detected: Option<std::time::Instant>,
    seek_threshold_rewind: f64,
    seek_threshold_fastforward: f64,
    slowdown_threshold: f64,
    slowdown_reset_threshold: f64,
    slowdown_rate: f64,
    slow_on_desync: bool,
    rewind_on_desync: bool,
    fastforward_on_desync: bool,
}

impl SyncEngine {
    pub fn new() -> Self {
        Self {
            slowdown_active: false,
            behind_first_detected: None,
            seek_threshold_rewind: 4.0,
            seek_threshold_fastforward: 5.0,
            slowdown_threshold: 1.5,
            slowdown_reset_threshold: 0.1,
            slowdown_rate: 0.95,
            slow_on_desync: true,
            rewind_on_desync: true,
            fastforward_on_desync: true,
        }
    }

    pub fn update_from_config(&mut self, prefs: &UserPreferences) {
        self.seek_threshold_rewind = prefs.seek_threshold_rewind;
        self.seek_threshold_fastforward = prefs.seek_threshold_fastforward;
        self.slowdown_threshold = prefs.slowdown_threshold;
        self.slowdown_reset_threshold = prefs.slowdown_reset_threshold;
        self.slowdown_rate = prefs.slowdown_rate;
        self.slow_on_desync = prefs.slow_on_desync && !prefs.dont_slow_down_with_me;
        self.rewind_on_desync = prefs.rewind_on_desync;
        self.fastforward_on_desync = prefs.fastforward_on_desync;
    }

    pub fn slowdown_rate(&self) -> f64 {
        self.slowdown_rate
    }

    /// Calculate synchronization actions needed
    pub fn calculate_sync_actions(&mut self, inputs: SyncInputs) -> Vec<SyncAction> {
        let mut actions = Vec::new();

        // Adjust global position for message age
        let adjusted_global_position = if !inputs.global_paused {
            inputs.global_position + inputs.message_age
        } else {
            inputs.global_position
        };

        // Calculate position difference
        let diff = inputs.local_position - adjusted_global_position;

        debug!(
            "Sync check: local={:.2}s ({}), global={:.2}s ({}), diff={:.2}s",
            inputs.local_position,
            if inputs.local_paused {
                "paused"
            } else {
                "playing"
            },
            adjusted_global_position,
            if inputs.global_paused {
                "paused"
            } else {
                "playing"
            },
            diff
        );

        // Check pause state first
        if inputs.local_paused != inputs.global_paused {
            info!(
                "Pause state mismatch: local={}, global={} - syncing",
                inputs.local_paused, inputs.global_paused
            );
            actions.push(SyncAction::SetPaused(inputs.global_paused));
        }

        if inputs.do_seek {
            actions.push(SyncAction::Seek(adjusted_global_position));
            if self.slowdown_active {
                actions.push(SyncAction::ResetSpeed);
            }
            self.slowdown_active = false;
        }

        // Only sync position if both are playing or both are paused
        if !inputs.do_seek && inputs.local_paused == inputs.global_paused {
            // Rewind when we're ahead of global
            if self.rewind_on_desync && diff > self.seek_threshold_rewind {
                info!(
                    "Ahead by {:.2}s (threshold: {:.2}s) - seeking backward",
                    diff, self.seek_threshold_rewind
                );
                actions.push(SyncAction::Seek(adjusted_global_position));
                self.slowdown_active = false;
                self.behind_first_detected = None;
            } else if inputs.allow_fastforward && self.fastforward_on_desync {
                if diff < -FASTFORWARD_BEHIND_THRESHOLD {
                    let now = std::time::Instant::now();
                    match self.behind_first_detected {
                        None => {
                            self.behind_first_detected = Some(now);
                        }
                        Some(start) => {
                            let duration_behind = now
                                .checked_duration_since(start)
                                .unwrap_or_default()
                                .as_secs_f64();
                            if duration_behind
                                > (self.seek_threshold_fastforward - FASTFORWARD_BEHIND_THRESHOLD)
                                && diff < -self.seek_threshold_fastforward
                            {
                                info!(
                                    "Behind by {:.2}s (threshold: {:.2}s) - seeking forward",
                                    diff.abs(),
                                    self.seek_threshold_fastforward
                                );
                                actions.push(SyncAction::Seek(
                                    adjusted_global_position + FASTFORWARD_EXTRA_TIME,
                                ));
                                self.slowdown_active = false;
                                self.behind_first_detected = Some(
                                    now + std::time::Duration::from_secs_f64(
                                        FASTFORWARD_RESET_THRESHOLD,
                                    ),
                                );
                            }
                        }
                    }
                } else {
                    self.behind_first_detected = None;
                }
            } else if self.slow_on_desync
                && !inputs.global_paused
                && diff.abs() > self.slowdown_threshold
            {
                // Minor desync while playing - apply slowdown
                if !self.slowdown_active {
                    info!(
                        "Minor desync {:.2}s (threshold: {:.2}s) - applying slowdown",
                        diff.abs(),
                        self.slowdown_threshold
                    );
                    actions.push(SyncAction::Slowdown);
                    self.slowdown_active = true;
                }
            } else if self.slowdown_active && diff.abs() < self.slowdown_reset_threshold {
                // Back in sync - reset speed
                info!(
                    "Back in sync ({:.2}s < {:.2}s) - resetting speed",
                    diff.abs(),
                    self.slowdown_reset_threshold
                );
                actions.push(SyncAction::ResetSpeed);
                self.slowdown_active = false;
            } else if self.slowdown_active && !self.slow_on_desync {
                // Slowdown disabled, reset to normal speed.
                actions.push(SyncAction::ResetSpeed);
                self.slowdown_active = false;
            }
        }

        if actions.is_empty() {
            actions.push(SyncAction::None);
        }

        actions
    }

    /// Reset slowdown state
    pub fn reset_slowdown(&mut self) {
        self.slowdown_active = false;
    }

    /// Check if slowdown is active
    pub fn is_slowdown_active(&self) -> bool {
        self.slowdown_active
    }
}

impl Default for SyncEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_no_action_when_in_sync() {
        let mut engine = SyncEngine::new();
        let actions = engine.calculate_sync_actions(SyncInputs {
            local_position: 10.0,
            local_paused: false,
            global_position: 10.0,
            global_paused: false,
            message_age: 0.0,
            do_seek: false,
            allow_fastforward: true,
        });
        assert_eq!(actions, vec![SyncAction::None]);
    }

    #[test]
    fn test_sync_seek_when_behind() {
        let mut engine = SyncEngine::new();
        let actions = engine.calculate_sync_actions(SyncInputs {
            local_position: 5.0,
            local_paused: false,
            global_position: 10.0,
            global_paused: false,
            message_age: 0.0,
            do_seek: false,
            allow_fastforward: true,
        });
        assert!(matches!(actions[0], SyncAction::Seek(_)));
    }

    #[test]
    fn test_sync_seek_when_ahead() {
        let mut engine = SyncEngine::new();
        let actions = engine.calculate_sync_actions(SyncInputs {
            local_position: 20.0,
            local_paused: false,
            global_position: 10.0,
            global_paused: false,
            message_age: 0.0,
            do_seek: false,
            allow_fastforward: true,
        });
        assert!(matches!(actions[0], SyncAction::Seek(_)));
    }

    #[test]
    fn test_sync_pause_state() {
        let mut engine = SyncEngine::new();
        let actions = engine.calculate_sync_actions(SyncInputs {
            local_position: 10.0,
            local_paused: true,
            global_position: 10.0,
            global_paused: false,
            message_age: 0.0,
            do_seek: false,
            allow_fastforward: true,
        });
        assert!(matches!(actions[0], SyncAction::SetPaused(false)));
    }

    #[test]
    fn test_sync_slowdown() {
        let mut engine = SyncEngine::new();
        let actions = engine.calculate_sync_actions(SyncInputs {
            local_position: 8.0,
            local_paused: false,
            global_position: 10.0,
            global_paused: false,
            message_age: 0.0,
            do_seek: false,
            allow_fastforward: true,
        });
        assert!(matches!(actions[0], SyncAction::Slowdown));
        assert!(engine.is_slowdown_active());
    }

    #[test]
    fn test_sync_reset_speed() {
        let mut engine = SyncEngine::new();
        // First apply slowdown
        engine.calculate_sync_actions(SyncInputs {
            local_position: 8.0,
            local_paused: false,
            global_position: 10.0,
            global_paused: false,
            message_age: 0.0,
            do_seek: false,
            allow_fastforward: true,
        });
        assert!(engine.is_slowdown_active());

        // Then get back in sync
        let actions = engine.calculate_sync_actions(SyncInputs {
            local_position: 10.0,
            local_paused: false,
            global_position: 10.0,
            global_paused: false,
            message_age: 0.0,
            do_seek: false,
            allow_fastforward: true,
        });
        assert!(matches!(actions[0], SyncAction::ResetSpeed));
        assert!(!engine.is_slowdown_active());
    }
}
