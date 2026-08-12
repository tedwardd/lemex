use std::{fs, path::{Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};

use lemmy::{AppConfig, AppError};

struct CurrentDirGuard(PathBuf);

impl CurrentDirGuard {
    fn enter(path: &Path) -> Self {
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        Self(previous)
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).unwrap();
    }
}

fn temporary_directory() -> PathBuf {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("lemmy-config-profiles-{unique}"));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn config_round_trips_non_secret_profile_metadata() {
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\naccount_label = 'primary'\n";
    let config = AppConfig::from_toml(source).unwrap();
    let encoded = config.to_toml().unwrap();
    assert_eq!(AppConfig::from_toml(&encoded).unwrap(), config);
}

#[test]
fn duplicate_profile_ids_are_rejected() {
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://one.test'\n[[profiles]]\nid = 'main'\ninstance_url = 'https://two.test'\n";
    assert!(matches!(AppConfig::from_toml(source), Err(AppError::Configuration(_))));
}

#[test]
fn credential_like_fields_are_rejected() {
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\npassword = 'secret'\n";
    assert!(matches!(AppConfig::from_toml(source), Err(AppError::Configuration(_))));
}

#[test]
fn profile_only_config_preserves_media_defaults() {
    let source = "[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n";
    let config = AppConfig::from_toml(source).unwrap();

    assert!(config.media.mailcap_enabled);
    assert_eq!(config.media.collision_policy, "prompt");
}

#[test]
fn relative_config_path_writes_and_reloads() {
    let directory = temporary_directory();
    {
        let _current_directory = CurrentDirGuard::enter(&directory);
        let path = Path::new("config.toml");
        let config = AppConfig::from_toml("[[profiles]]\nid = 'main'\ninstance_url = 'https://example.test'\n").unwrap();

        config.write_atomic(path).unwrap();

        assert_eq!(AppConfig::load(path).unwrap(), config);
    }
    fs::remove_dir_all(directory).unwrap();
}
