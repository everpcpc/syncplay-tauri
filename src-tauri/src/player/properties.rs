/// Player state extracted from MPV properties
#[derive(Debug, Clone)]
pub struct PlayerState {
    pub position: Option<f64>,
    pub paused: Option<bool>,
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
            filename: None,
            duration: None,
            path: None,
            speed: Some(1.0),
        }
    }
}
