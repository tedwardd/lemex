use lemmy::{AppConfig, AppError};

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
