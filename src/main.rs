use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use lemmy::{
    api::HttpLemmyApi,
    app::{App, run_terminal},
    cache::SqliteCacheStore,
    config::{AppConfig, cache_dir, config_path},
    domain::ProfileContext,
    error::{AppError, Result},
    profiles::{CredentialStore, KeyringCredentialStore},
};

/// Initialize the opt-in tracing subscriber. Logging is disabled by default;
/// when enabled, the level comes from configuration and every log line still
/// redacts credentials and private content (the application never logs
/// secrets). Re-initialization is ignored silently.
fn init_logging(config: &AppConfig) {
    if !config.logging.enabled {
        return;
    }
    let level = config
        .logging
        .level
        .as_deref()
        .and_then(|level| level.parse::<tracing::Level>().ok())
        .map(tracing_subscriber::filter::LevelFilter::from_level)
        .unwrap_or(tracing_subscriber::filter::LevelFilter::INFO);
    let _ = tracing_subscriber::fmt().with_max_level(level).try_init();
}

async fn build_app() -> Result<(App, HashMap<String, String>, String)> {
    let path = config_path();
    let config = if path.exists() {
        AppConfig::load(&path)?
    } else {
        AppConfig::default()
    };
    init_logging(&config);
    let media = config.media.clone();
    let keymaps = config.keymaps.clone();
    let profile = config.profiles.into_iter().next().ok_or_else(|| {
        AppError::Configuration(format!("no profiles configured in {}", path.display()))
    })?;
    let cache_root = config.cache.directory.unwrap_or_else(cache_dir);
    fs::create_dir_all(&cache_root).map_err(|error| {
        AppError::Storage(format!(
            "cannot create cache directory {}: {error}",
            cache_root.display()
        ))
    })?;
    let cache = SqliteCacheStore::open_with_size_limit(
        cache_root.join(PathBuf::from("cache.sqlite3")),
        config.cache.max_size_bytes,
    )?;
    let api = HttpLemmyApi::new()?;
    // Restore a previously stored session from the OS credential store when
    // one is available. Secrets never touch the config file. A missing or
    // unavailable keyring (headless session, unsupported target, no secret
    // service) must not block launch: start anonymous and let `:login`
    // surface any credential-store failure when the user actually signs in.
    let credentials = Arc::new(KeyringCredentialStore::default());
    let session = match credentials.get_session(&profile.id).await {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(%error, "keyring unavailable at startup; starting anonymous");
            None
        }
    };
    Ok((
        App::with_media(
            Arc::new(api),
            Arc::new(cache),
            ProfileContext { profile, session },
            credentials,
            media,
        ),
        keymaps,
        config.startup,
    ))
}

/// Non-interactive usage text printed for `lemmy --help`. The interactive
/// default (no arguments) starts the terminal client unchanged.
const USAGE: &str = "\
lemmy — a terminal client for Lemmy

Usage:
  lemmy            start the interactive terminal client
  lemmy --help     show this help and exit

The interactive client is a Vim-like modal TUI. Run :help inside the client
for the full command index; a summary follows.";

fn print_help() {
    println!("{USAGE}");
    println!();
    println!("Commands:");
    for entry in lemmy::app::help::HelpIndex::default().search("") {
        println!("  {:<30} {}", entry.command, entry.description);
    }
}

fn main() -> Result<()> {
    // The help path must exit successfully without entering the TUI (no
    // config load, no runtime, no alternate screen), so it is checked before
    // anything else runs.
    if std::env::args()
        .skip(1)
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print_help();
        return Ok(());
    }
    // One runtime for the whole process: startup session restoration and the
    // terminal event loop share it, so application state is never handed
    // between two runtimes and blocking work stays on the multi-thread pool.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| AppError::Terminal(format!("could not start Tokio runtime: {error}")))?;
    let (app, keymaps, startup) = runtime.block_on(build_app())?;
    let terminal = ratatui::init();
    let result = run_terminal(app, terminal, &runtime, &keymaps, &startup);
    ratatui::restore();
    result
}
