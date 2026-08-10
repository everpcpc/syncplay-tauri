use std::time::{Duration, Instant};

const SEEK_THRESHOLD: f64 = 1.0;
const REMOTE_SEEK_SUPPRESSION_THRESHOLD: f64 = 1.5;
const REMOTE_CORRECTION_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy)]
struct PendingCorrection<T> {
    token: u64,
    target: T,
    after_generation: u64,
    command_succeeded: bool,
    target_observed: bool,
    deadline: Option<Instant>,
}

impl<T> PendingCorrection<T> {
    fn is_active_at(&self, now: Instant) -> bool {
        !self.command_succeeded || self.deadline.is_some_and(|deadline| now < deadline)
    }
}

#[derive(Debug, Clone)]
pub struct LocalPlaybackState {
    position: f64,
    paused: bool,
    initialized: bool,
    // Accepted values are observations attributed to settled remote state or
    // local intent. Intermediate command responses must not move this baseline.
    accepted_position: f64,
    accepted_paused: bool,
    accepted_position_initialized: bool,
    accepted_paused_initialized: bool,
    position_observation_generation: u64,
    paused_observation_generation: u64,
    next_correction_token: u64,
    pending_position: Option<PendingCorrection<f64>>,
    pending_paused: Option<PendingCorrection<bool>>,
}

impl LocalPlaybackState {
    pub fn new() -> Self {
        Self {
            position: 0.0,
            paused: true,
            initialized: false,
            accepted_position: 0.0,
            accepted_paused: true,
            accepted_position_initialized: false,
            accepted_paused_initialized: false,
            position_observation_generation: 0,
            paused_observation_generation: 0,
            next_correction_token: 1,
            pending_position: None,
            pending_paused: None,
        }
    }

    pub fn reset_player_observation(&mut self) {
        let next_correction_token = self.next_correction_token;
        *self = Self::new();
        self.next_correction_token = next_correction_token;
    }

    pub fn clear_remote_corrections(&mut self) {
        self.pending_position = None;
        self.pending_paused = None;
    }

    pub fn begin_remote_position(&mut self, position: f64, after_generation: u64) -> u64 {
        let token = self.next_token();
        self.pending_position = Some(PendingCorrection {
            token,
            target: position,
            after_generation,
            command_succeeded: false,
            target_observed: false,
            deadline: None,
        });
        token
    }

    pub fn complete_remote_position(&mut self, token: u64) {
        self.complete_remote_position_at(token, Instant::now());
    }

    fn complete_remote_position_at(&mut self, token: u64, now: Instant) {
        if let Some(mut pending) = self
            .pending_position
            .filter(|pending| pending.token == token)
        {
            pending.command_succeeded = true;
            pending.deadline = Some(now + REMOTE_CORRECTION_TIMEOUT);
            self.pending_position = if pending.target_observed {
                None
            } else {
                Some(pending)
            };
        }
    }

    pub fn cancel_remote_position(&mut self, token: u64) {
        if self
            .pending_position
            .is_some_and(|pending| pending.token == token)
        {
            self.pending_position = None;
        }
    }

    pub fn begin_remote_pause(&mut self, paused: bool, after_generation: u64) -> u64 {
        let token = self.next_token();
        self.pending_paused = Some(PendingCorrection {
            token,
            target: paused,
            after_generation,
            command_succeeded: false,
            target_observed: false,
            deadline: None,
        });
        token
    }

    pub fn complete_remote_pause(&mut self, token: u64) {
        self.complete_remote_pause_at(token, Instant::now());
    }

    fn complete_remote_pause_at(&mut self, token: u64, now: Instant) {
        if let Some(mut pending) = self.pending_paused.filter(|pending| pending.token == token) {
            pending.command_succeeded = true;
            pending.deadline = Some(now + REMOTE_CORRECTION_TIMEOUT);
            self.pending_paused = if pending.target_observed {
                None
            } else {
                Some(pending)
            };
        }
    }

    pub fn cancel_remote_pause(&mut self, token: u64) {
        if self
            .pending_paused
            .is_some_and(|pending| pending.token == token)
        {
            self.pending_paused = None;
        }
    }

    pub fn remote_position_is_active_for(&self, position: f64) -> bool {
        self.pending_position.is_some_and(|pending| {
            pending.is_active_at(Instant::now())
                && (pending.target - position).abs() <= REMOTE_SEEK_SUPPRESSION_THRESHOLD
        })
    }

    #[cfg(test)]
    fn remote_position_is_active_for_at(&self, position: f64, now: Instant) -> bool {
        self.pending_position.is_some_and(|pending| {
            pending.is_active_at(now)
                && (pending.target - position).abs() <= REMOTE_SEEK_SUPPRESSION_THRESHOLD
        })
    }

    pub fn has_active_remote_position(&self) -> bool {
        self.pending_position
            .is_some_and(|pending| pending.is_active_at(Instant::now()))
    }

    pub fn remote_pause_is_active_for(&self, paused: bool) -> bool {
        self.pending_paused
            .is_some_and(|pending| pending.target == paused && pending.is_active_at(Instant::now()))
    }

    pub fn remote_pause_is_handled(&self, paused: bool, observed_paused: Option<bool>) -> bool {
        match self.pending_paused {
            Some(pending) if pending.target != paused => false,
            Some(pending) => {
                pending.is_active_at(Instant::now()) || observed_paused == Some(paused)
            }
            None => observed_paused == Some(paused),
        }
    }

    #[cfg(test)]
    fn has_pending_remote_position(&self) -> bool {
        self.pending_position.is_some()
    }

    fn next_token(&mut self) -> u64 {
        let token = self.next_correction_token;
        self.next_correction_token = self.next_correction_token.saturating_add(1);
        token
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_from_player(
        &mut self,
        position: f64,
        paused: bool,
        observed_position: Option<f64>,
        observed_paused: Option<bool>,
        position_observation_generation: u64,
        paused_observation_generation: u64,
        global_position: f64,
        global_paused: bool,
    ) -> (bool, bool) {
        self.update_from_player_at(
            position,
            paused,
            observed_position,
            observed_paused,
            position_observation_generation,
            paused_observation_generation,
            global_position,
            global_paused,
            Instant::now(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn update_from_player_at(
        &mut self,
        position: f64,
        paused: bool,
        observed_position: Option<f64>,
        observed_paused: Option<bool>,
        position_observation_generation: u64,
        paused_observation_generation: u64,
        global_position: f64,
        global_paused: bool,
        now: Instant,
    ) -> (bool, bool) {
        let position_snapshot_is_current =
            position_observation_generation >= self.position_observation_generation;
        let paused_snapshot_is_current =
            paused_observation_generation >= self.paused_observation_generation;
        let fresh_position = position_observation_generation > self.position_observation_generation;
        let fresh_paused = paused_observation_generation > self.paused_observation_generation;

        if position_snapshot_is_current {
            self.position = position;
            self.initialized = true;
        }
        if paused_snapshot_is_current {
            self.paused = paused;
            self.initialized = true;
        }

        let mut seeked = false;
        let mut pause_change = false;

        if let Some(observed_position) = observed_position.filter(|_| position_snapshot_is_current)
        {
            let candidate_seek = self.accepted_position_initialized
                && (self.accepted_position - observed_position).abs() > SEEK_THRESHOLD
                && (global_position - observed_position).abs() > SEEK_THRESHOLD;
            let mut accept_position = fresh_position;

            if let Some(mut pending) = self.pending_position {
                let observed_after_command =
                    position_observation_generation > pending.after_generation;
                if observed_after_command {
                    let reached_target = (observed_position - pending.target).abs()
                        <= REMOTE_SEEK_SUPPRESSION_THRESHOLD
                        || (observed_position - global_position).abs()
                            <= REMOTE_SEEK_SUPPRESSION_THRESHOLD;
                    if reached_target {
                        pending.target_observed = true;
                        accept_position = true;
                        self.pending_position = if pending.command_succeeded {
                            None
                        } else {
                            Some(pending)
                        };
                    } else if pending.command_succeeded
                        && pending.deadline.is_some_and(|deadline| now >= deadline)
                        && candidate_seek
                    {
                        seeked = true;
                        accept_position = true;
                        self.pending_position = None;
                    } else {
                        accept_position = false;
                    }
                } else {
                    accept_position = false;
                }
            } else if fresh_position {
                seeked = candidate_seek;
            }

            if accept_position {
                self.accepted_position = observed_position;
                self.accepted_position_initialized = true;
            }
        }

        if let Some(observed_paused) = observed_paused.filter(|_| paused_snapshot_is_current) {
            let candidate_pause_change = self.accepted_paused_initialized
                && self.accepted_paused != observed_paused
                && global_paused != observed_paused;
            let mut accept_paused = fresh_paused;

            if let Some(mut pending) = self.pending_paused {
                let observed_after_command =
                    paused_observation_generation > pending.after_generation;
                if observed_after_command {
                    if observed_paused == pending.target {
                        pending.target_observed = true;
                        accept_paused = true;
                        self.pending_paused = if pending.command_succeeded {
                            None
                        } else {
                            Some(pending)
                        };
                    } else if pending.command_succeeded
                        && pending.deadline.is_some_and(|deadline| now >= deadline)
                        && candidate_pause_change
                    {
                        pause_change = true;
                        accept_paused = true;
                        self.pending_paused = None;
                    } else {
                        accept_paused = false;
                    }
                } else {
                    accept_paused = false;
                }
            } else if fresh_paused {
                pause_change = candidate_pause_change;
            }

            if accept_paused {
                self.accepted_paused = observed_paused;
                self.accepted_paused_initialized = true;
            }
        }

        if fresh_position {
            self.position_observation_generation = position_observation_generation;
        }
        if fresh_paused {
            self.paused_observation_generation = paused_observation_generation;
        }

        (pause_change, seeked)
    }

    pub fn current(&self) -> Option<(f64, bool)> {
        self.initialized.then_some((self.position, self.paused))
    }

    pub fn protocol_state(&self, global_position: f64, global_paused: bool) -> Option<(f64, bool)> {
        self.current().map(|(position, paused)| {
            (
                if self.pending_position.is_some() {
                    global_position
                } else {
                    position
                },
                if self.pending_paused.is_some() {
                    global_paused
                } else {
                    paused
                },
            )
        })
    }

    pub fn compute_seeked(&self, position: f64, global_position: f64) -> bool {
        if !self.initialized || self.pending_position.is_some() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn update(
        state: &mut LocalPlaybackState,
        position: f64,
        paused: bool,
        observed_position: f64,
        observed_paused: bool,
        position_generation: u64,
        paused_generation: u64,
        global_position: f64,
        global_paused: bool,
        now: Instant,
    ) -> (bool, bool) {
        state.update_from_player_at(
            position,
            paused,
            Some(observed_position),
            Some(observed_paused),
            position_generation,
            paused_generation,
            global_position,
            global_paused,
            now,
        )
    }

    #[test]
    fn remote_position_suppresses_stale_and_intermediate_observations() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        assert_eq!(
            update(&mut state, 120.0, false, 120.0, false, 1, 1, 120.0, false, now),
            (false, false)
        );

        let token = state.begin_remote_position(80.0, 1);
        state.complete_remote_position_at(token, now);
        for (generation, position) in [(2, 120.0), (3, 100.0), (4, 80.0)] {
            assert_eq!(
                update(
                    &mut state, position, false, position, false, generation, 1, 80.0, false, now
                ),
                (false, false)
            );
        }
        assert!(!state.has_pending_remote_position());

        assert_eq!(
            update(&mut state, 60.0, false, 60.0, false, 5, 1, 80.0, false, now),
            (false, true)
        );
    }

    #[test]
    fn command_cache_cannot_confirm_a_remote_position() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        update(
            &mut state, 120.0, false, 120.0, false, 1, 1, 120.0, false, now,
        );
        let token = state.begin_remote_position(80.0, 1);

        assert_eq!(
            update(&mut state, 80.0, false, 120.0, false, 1, 1, 80.0, false, now),
            (false, false)
        );
        state.complete_remote_position_at(token, now);
        assert!(state.has_pending_remote_position());
        assert_eq!(state.protocol_state(80.0, false), Some((80.0, false)));
    }

    #[test]
    fn confirmation_received_while_command_is_running_settles_on_completion() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        update(
            &mut state, 120.0, false, 120.0, false, 1, 1, 120.0, false, now,
        );
        let token = state.begin_remote_position(80.0, 1);

        assert_eq!(
            update(&mut state, 80.0, false, 80.0, false, 2, 1, 80.0, false, now),
            (false, false)
        );
        assert!(state.has_pending_remote_position());
        state.complete_remote_position_at(token, now);
        assert!(!state.has_pending_remote_position());
    }

    #[test]
    fn delayed_older_snapshot_cannot_settle_a_correction() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        update(
            &mut state, 120.0, false, 120.0, false, 1, 1, 120.0, false, now,
        );
        let token = state.begin_remote_position(80.0, 1);
        state.complete_remote_position_at(token, now);

        update(
            &mut state, 100.0, false, 100.0, false, 3, 1, 80.0, false, now,
        );
        assert_eq!(
            update(&mut state, 80.0, false, 80.0, false, 2, 1, 80.0, false, now),
            (false, false)
        );
        assert!(state.has_pending_remote_position());
        assert_eq!(state.position_observation_generation, 3);
    }

    #[test]
    fn rejected_position_expires_for_retry_but_keeps_protocol_ack_authoritative() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        update(
            &mut state, 120.0, false, 120.0, false, 1, 1, 120.0, false, now,
        );
        let token = state.begin_remote_position(80.0, 1);
        state.complete_remote_position_at(token, now);
        update(
            &mut state, 120.0, false, 120.0, false, 2, 1, 80.0, false, now,
        );

        assert!(state.remote_position_is_active_for_at(80.0, now));
        assert!(!state.remote_position_is_active_for_at(80.0, now + REMOTE_CORRECTION_TIMEOUT));
        assert_eq!(state.protocol_state(81.25, false), Some((81.25, false)));
    }

    #[test]
    fn observation_timeout_starts_after_a_slow_command_completes() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        update(
            &mut state, 120.0, false, 120.0, false, 1, 1, 120.0, false, now,
        );
        let token = state.begin_remote_position(80.0, 1);
        let completed_at = now + Duration::from_secs(5);
        state.complete_remote_position_at(token, completed_at);

        assert_eq!(
            update(
                &mut state,
                120.0,
                false,
                120.0,
                false,
                2,
                1,
                80.0,
                false,
                completed_at + Duration::from_millis(500),
            ),
            (false, false)
        );
        assert!(
            state.remote_position_is_active_for_at(80.0, completed_at + Duration::from_millis(500))
        );
    }

    #[test]
    fn local_seek_after_correction_timeout_is_reported_once() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        update(
            &mut state, 120.0, false, 120.0, false, 1, 1, 120.0, false, now,
        );
        let token = state.begin_remote_position(80.0, 1);
        state.complete_remote_position_at(token, now);

        assert_eq!(
            update(&mut state, 60.0, false, 60.0, false, 2, 1, 80.0, false, now),
            (false, false)
        );
        assert_eq!(
            update(
                &mut state,
                60.0,
                false,
                60.0,
                false,
                2,
                1,
                80.0,
                false,
                now + REMOTE_CORRECTION_TIMEOUT,
            ),
            (false, true)
        );
        assert_eq!(
            update(
                &mut state,
                60.0,
                false,
                60.0,
                false,
                2,
                1,
                80.0,
                false,
                now + REMOTE_CORRECTION_TIMEOUT,
            ),
            (false, false)
        );
    }

    #[test]
    fn rejected_remote_unpause_remains_an_ack_without_blocking_retry() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        update(&mut state, 120.0, true, 120.0, true, 1, 1, 120.0, true, now);
        let token = state.begin_remote_pause(false, 1);
        state.complete_remote_pause_at(token, now);

        assert_eq!(
            update(&mut state, 120.0, true, 120.0, true, 1, 2, 120.0, false, now),
            (false, false)
        );
        assert_eq!(state.protocol_state(120.0, false), Some((120.0, false)));
        assert!(state.remote_pause_is_active_for(false));
        assert!(!state
            .pending_paused
            .expect("missing rejected pause correction")
            .is_active_at(now + REMOTE_CORRECTION_TIMEOUT));
    }

    #[test]
    fn position_and_pause_observations_settle_independently() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        update(
            &mut state, 120.0, false, 120.0, false, 1, 1, 120.0, false, now,
        );
        let position = state.begin_remote_position(80.0, 1);
        let pause = state.begin_remote_pause(true, 1);
        state.complete_remote_position_at(position, now);
        state.complete_remote_pause_at(pause, now);

        assert_eq!(
            update(&mut state, 80.0, true, 80.0, false, 2, 1, 80.0, true, now),
            (false, false)
        );
        assert!(!state.has_pending_remote_position());
        assert!(state.pending_paused.is_some());

        assert_eq!(
            update(&mut state, 80.0, true, 80.0, true, 2, 2, 80.0, true, now),
            (false, false)
        );
        assert!(state.pending_paused.is_none());
    }

    #[test]
    fn opposite_pause_target_supersedes_an_inflight_correction() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        update(
            &mut state, 120.0, false, 120.0, false, 1, 1, 120.0, false, now,
        );
        let stale = state.begin_remote_pause(true, 1);
        state.complete_remote_pause_at(stale, now);

        assert!(!state.remote_pause_is_handled(false, Some(false)));
        let current = state.begin_remote_pause(false, 1);
        assert_eq!(
            state.pending_paused.map(|pending| pending.target),
            Some(false)
        );
        state.cancel_remote_pause(stale);
        assert!(state.pending_paused.is_some());
        state.cancel_remote_pause(current);
        assert!(state.pending_paused.is_none());
    }

    #[test]
    fn reset_invalidates_old_tokens_and_accepts_a_new_player_generation() {
        let now = Instant::now();
        let mut state = LocalPlaybackState::new();
        update(
            &mut state, 12.0, false, 12.0, false, 10, 11, 12.0, false, now,
        );
        let stale = state.begin_remote_position(5.0, 10);
        state.reset_player_observation();

        let current = state.begin_remote_position(3.0, 0);
        state.cancel_remote_position(stale);
        state.complete_remote_position_at(stale, now);
        assert!(state.has_pending_remote_position());
        state.cancel_remote_position(current);
        assert_eq!(
            update(&mut state, 1.0, true, 1.0, true, 1, 1, 1.0, true, now),
            (false, false)
        );
    }
}
