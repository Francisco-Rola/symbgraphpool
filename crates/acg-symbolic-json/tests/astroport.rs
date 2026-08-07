use std::collections::BTreeMap;

use acg_core::{AccessMode, ContractCodeHash, EntrypointKind, EntrypointSelector, RuntimeId};
use acg_symbolic_json::{normalize_document, parse_slice, raw::RawDelegation, IngestionContext};

const FIXTURE: &[u8] = include_bytes!("fixtures/astroport_pair_compact_config_fields.json");

fn context() -> IngestionContext {
    IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([1; 32]),
        1,
    )
}

#[test]
fn parses_and_normalizes_attached_analyzer_output() {
    let raw = parse_slice(FIXTURE).unwrap();
    assert_eq!(raw.profiles.len(), 20);

    let profiles = normalize_document(raw, &context()).unwrap();
    assert_eq!(profiles.len(), 20);

    let swap = profiles
        .iter()
        .find(|profile| profile.entrypoint_name == "execute::Swap")
        .unwrap();
    assert_eq!(swap.descriptor.entrypoint_kind, EntrypointKind::Execute);
    assert_eq!(swap.accesses.len(), 4);
    assert_eq!(
        swap.accesses
            .iter()
            .filter(|access| access.mode == AccessMode::Write)
            .count(),
        3
    );

    let config = &swap.accesses[0];
    assert!(config
        .semantic_key_components
        .contains(&"pair_info".to_owned()));

    let balance = swap
        .accesses
        .iter()
        .find(|access| access.resource.as_str() == "BALANCES")
        .unwrap();
    assert_eq!(
        balance.semantic_key_components,
        vec!["asset_info".to_owned()]
    );
    assert_eq!(
        balance
            .key_dependency
            .as_ref()
            .unwrap()
            .origin_input
            .as_deref(),
        Some("offer_asset.info")
    );
}

#[test]
fn selector_override_is_used_in_profile_identity() {
    let raw = parse_slice(FIXTURE).unwrap();
    let mut context = context();
    context.selector_overrides =
        BTreeMap::from([("execute::Swap".to_owned(), EntrypointSelector(42))]);

    let profiles = normalize_document(raw, &context).unwrap();
    let swap = profiles
        .iter()
        .find(|profile| profile.entrypoint_name == "execute::Swap")
        .unwrap();
    assert_eq!(
        swap.descriptor.numeric_entrypoint_selector,
        EntrypointSelector(42)
    );
}

#[test]
fn profile_keys_are_independent_of_document_order() {
    let mut raw = parse_slice(FIXTURE).unwrap();
    let normal = normalize_document(raw.clone(), &context()).unwrap();
    raw.profiles.reverse();
    let reversed = normalize_document(raw, &context()).unwrap();

    let mut normal_keys: Vec<_> = normal
        .into_iter()
        .map(|profile| profile.stable_key)
        .collect();
    let mut reversed_keys: Vec<_> = reversed
        .into_iter()
        .map(|profile| profile.stable_key)
        .collect();
    normal_keys.sort();
    reversed_keys.sort();
    assert_eq!(normal_keys, reversed_keys);
}

#[test]
fn rejects_unknown_selector_override() {
    let raw = parse_slice(FIXTURE).unwrap();
    let mut context = context();
    context.selector_overrides =
        BTreeMap::from([("execute::DoesNotExist".to_owned(), EntrypointSelector(99))]);

    let error = normalize_document(raw, &context).unwrap_err();
    assert!(error.to_string().contains("unknown entrypoint"));
}

#[test]
fn preserves_delegation_metadata() {
    let raw = parse_slice(FIXTURE).unwrap();
    let profiles = normalize_document(raw, &context()).unwrap();
    let receive = profiles
        .iter()
        .find(|profile| profile.entrypoint_name == "execute::Receive(Cw20HookMsg::Swap)")
        .unwrap();
    let delegation = receive.delegates_to.as_ref().unwrap();
    assert_eq!(delegation.entrypoint, "execute::Swap");
    assert!(delegation
        .input_mapping
        .iter()
        .any(|mapping| mapping.target == "offer_asset.info"));
    assert_eq!(receive.accesses.len(), 5);
    let inherited_balance = receive
        .accesses
        .iter()
        .find(|access| access.resource.as_str() == "BALANCES" && !access.delegation_path.is_empty())
        .unwrap();
    assert_eq!(
        inherited_balance
            .key_dependency
            .as_ref()
            .unwrap()
            .origin_input
            .as_deref(),
        Some("Token { contract_addr: info.sender }")
    );
    assert_eq!(
        inherited_balance.delegation_path[0].target_entrypoint,
        "execute::Swap"
    );
}

#[test]
fn rejects_missing_delegation_target() {
    let mut raw = parse_slice(FIXTURE).unwrap();
    raw.profiles
        .iter_mut()
        .find(|profile| profile.entrypoint == "execute::Receive(Cw20HookMsg::Swap)")
        .unwrap()
        .delegates_to
        .as_mut()
        .unwrap()
        .entrypoint = "execute::Missing".to_owned();

    let error = normalize_document(raw, &context()).unwrap_err();
    assert!(error.to_string().contains("missing entrypoint"));
}

#[test]
fn rejects_delegation_cycles() {
    let mut raw = parse_slice(FIXTURE).unwrap();
    raw.profiles
        .iter_mut()
        .find(|profile| profile.entrypoint == "execute::Swap")
        .unwrap()
        .delegates_to = Some(RawDelegation {
        entrypoint: "execute::Receive(Cw20HookMsg::Swap)".to_owned(),
        input_mapping: BTreeMap::new(),
    });

    let error = normalize_document(raw, &context()).unwrap_err();
    assert!(error.to_string().contains("delegation cycle"));
}
