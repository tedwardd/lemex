use std::{fs, path::PathBuf, sync::Arc};

use lemmy::{
    api::HttpLemmyApi,
    app::{run_terminal, App},
    cache::SqliteCacheStore,
    config::{cache_dir, config_path, AppConfig},
    domain::ProfileContext,
    error::{AppError, Result},
    profiles::KeyringCredentialStore,
};

fn build_app() -> Result<App> {
    let path = config_path();
    let config = if path.exists() { AppConfig::load(&path)? } else { AppConfig::default() };
    let media = config.media.clone();
    let profile = config
        .profiles
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Configuration(format!("no profiles configured in {}", path.display())))?;
    let cache_root = config.cache.directory.unwrap_or_else(cache_dir);
    fs::create_dir_all(&cache_root)
        .map_err(|error| AppError::Storage(format!("cannot create cache directory {}: {error}", cache_root.display())))?;
    let cache = SqliteCacheStore::open(cache_root.join(PathBuf::from("cache.sqlite3")))?;
    let api = HttpLemmyApi::new()?;
    Ok(App::with_media(
        Arc::new(api),
        Arc::new(cache),
        ProfileContext { profile, session: None },
        Arc::new(KeyringCredentialStore::default()),
        media,
    ))
}

fn main() -> Result<()> {
    let app = build_app()?;
    let terminal = ratatui::init();
    let result = run_terminal(app, terminal);
    ratatui::restore();
    result
}
