use acg_evaluation::{sha256_hex, ExperimentMetadata};

#[test]
fn metadata_capture_populates_portable_fields_without_overwriting_explicit_values() {
    let metadata = ExperimentMetadata {
        experiment_id: "capture".to_owned(),
        workload: "fixture".to_owned(),
        mode: "smoke".to_owned(),
        build_profile: Some("explicit-profile".to_owned()),
        ..ExperimentMetadata::default()
    }
    .capture_standard_environment(env!("CARGO_MANIFEST_DIR"));

    assert!(metadata.started_at_utc.is_some());
    assert_eq!(metadata.build_profile.as_deref(), Some("explicit-profile"));
    assert_eq!(
        metadata.environment.get("os").map(String::as_str),
        Some(std::env::consts::OS)
    );
    assert_eq!(
        metadata.environment.get("arch").map(String::as_str),
        Some(std::env::consts::ARCH)
    );
    assert!(metadata.environment.contains_key("logical_cores"));
}

#[test]
fn sha256_digest_is_deterministic() {
    assert_eq!(sha256_hex(b"brick5f"), sha256_hex(b"brick5f"));
    assert_ne!(sha256_hex(b"brick5f"), sha256_hex(b"brick5e"));
    assert_eq!(sha256_hex(b"brick5f").len(), 64);
}
