use std::{fs, path::PathBuf};

use acg_core::{ContractCodeHash, RuntimeId};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};

const FAMILIES: &[&str] = &[
    "cw20-base",
    "controlled-cw20",
    "fiat-token-cw20",
    "fee-token-cw20",
    "wrapped-native-token",
    "cw721-mintable",
    "astroport-pair",
    "xen-like",
    "cw1155-like",
    "marketplace-router",
    "operator-filter-helper",
    "cw721-drop",
    "stargate-cw20",
];

#[test]
fn native_s3_artifacts_parse_and_normalize() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (index, family) in FAMILIES.iter().enumerate() {
        let path = root.join(format!(
            "benchmarks/symbolic/native-s3/{family}.symbolic.json"
        ));
        let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let raw = parse_slice(&bytes).unwrap_or_else(|error| panic!("{family}: {error}"));
        let mut hash = [0_u8; 32];
        hash[0] = u8::try_from(index + 1).unwrap();
        let context = IngestionContext::new(
            RuntimeId::new("cosmwasm").unwrap(),
            ContractCodeHash(hash),
            1,
        );
        let normalized =
            normalize_document(raw, &context).unwrap_or_else(|error| panic!("{family}: {error}"));
        assert!(!normalized.is_empty(), "{family} produced no profiles");
    }
}
