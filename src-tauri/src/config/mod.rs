pub mod settings;
pub mod persistence;

pub use settings::{ServerConfig, UserPreferences, SyncplayConfig};
pub use persistence::{load_config, save_config, get_config_path};
