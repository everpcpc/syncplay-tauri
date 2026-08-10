/// Player state extracted from MPV properties
#[derive(Debug, Clone)]
pub struct PlayerState {
    pub position: Option<f64>,
    pub paused: Option<bool>,
    // Command-side optimistic cache writes only update the current values above.
    // Observed values and their generations form snapshots returned by the player.
    pub observed_position: Option<f64>,
    pub observed_paused: Option<bool>,
    pub position_observation_generation: u64,
    pub paused_observation_generation: u64,
    pub filename: Option<String>,
    pub duration: Option<f64>,
    pub path: Option<String>,
    pub speed: Option<f64>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            position: None,
            paused: Some(true),
            observed_position: None,
            observed_paused: None,
            position_observation_generation: 0,
            paused_observation_generation: 0,
            filename: None,
            duration: None,
            path: None,
            speed: Some(1.0),
        }
    }
}

impl PlayerState {
    pub fn observe_position(&mut self, position: Option<f64>) {
        self.position = position;
        self.observed_position = position;
        self.position_observation_generation =
            self.position_observation_generation.saturating_add(1);
    }

    pub fn observe_paused(&mut self, paused: Option<bool>) {
        self.paused = paused;
        self.observed_paused = paused;
        self.paused_observation_generation = self.paused_observation_generation.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_update_current_and_observed_snapshots() {
        let mut state = PlayerState::default();

        state.observe_position(Some(12.5));
        state.observe_paused(Some(false));

        assert_eq!(state.position, Some(12.5));
        assert_eq!(state.observed_position, Some(12.5));
        assert_eq!(state.position_observation_generation, 1);
        assert_eq!(state.paused, Some(false));
        assert_eq!(state.observed_paused, Some(false));
        assert_eq!(state.paused_observation_generation, 1);
    }

    #[test]
    fn optimistic_writes_do_not_change_observed_snapshots() {
        let mut state = PlayerState::default();
        state.observe_position(Some(12.5));
        state.observe_paused(Some(false));

        state.position = Some(42.0);
        state.paused = Some(true);

        assert_eq!(state.position, Some(42.0));
        assert_eq!(state.observed_position, Some(12.5));
        assert_eq!(state.position_observation_generation, 1);
        assert_eq!(state.paused, Some(true));
        assert_eq!(state.observed_paused, Some(false));
        assert_eq!(state.paused_observation_generation, 1);
    }
}
