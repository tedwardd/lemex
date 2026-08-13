mod model;
pub mod paths;

pub use model::{AppConfig, CacheConfig, ColorsConfig, LogConfig, MediaConfig, parse_color};
pub use paths::{
    ConfigPaths, XdgPaths, cache_dir, cache_path, config_dir, config_path, log_path, paths,
};
