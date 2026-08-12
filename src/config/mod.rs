mod model;
pub mod paths;

pub use model::{AppConfig, CacheConfig, LogConfig, MediaConfig};
pub use paths::{ConfigPaths, XdgPaths, cache_dir, cache_path, config_dir, config_path, paths};
