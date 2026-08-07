use acg_core::{ContractCodeHash, DependencyKind, RuntimeId};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};

const CONFLICTLAB: &[u8] = include_bytes!("../../../benchmarks/symbolic/conflictlab.symbolic.json");
const MINIWAREHOUSE: &[u8] =
    include_bytes!("../../../benchmarks/symbolic/miniwarehouse.symbolic.json");

fn context(code_byte: u8) -> IngestionContext {
    IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([code_byte; 32]),
        1,
    )
}

#[test]
fn parses_conflictlab_profiles_and_expands_delegation() {
    let raw = parse_slice(CONFLICTLAB).unwrap();
    assert_eq!(raw.contract, "conflictlab");
    assert_eq!(raw.profiles.len(), 18);

    let profiles = normalize_document(raw, &context(21)).unwrap();
    let receive = profiles
        .iter()
        .find(|profile| profile.entrypoint_name == "execute::ReceiveTransfer")
        .unwrap();
    assert_eq!(receive.accesses.len(), 2);
    assert!(receive.accesses.iter().all(|access| {
        access.resource.as_str() == "BALANCES" && !access.delegation_path.is_empty()
    }));
    assert!(receive.accesses.iter().all(|access| {
        access
            .key_dependency
            .as_ref()
            .and_then(|dependency| dependency.origin_input.as_deref())
            == Some("account")
    }));
}

#[test]
fn preserves_conflictlab_wildcard_and_state_derived_keys() {
    let profiles = normalize_document(parse_slice(CONFLICTLAB).unwrap(), &context(22)).unwrap();

    let reset = profiles
        .iter()
        .find(|profile| profile.entrypoint_name == "execute::ResetAllBalances")
        .unwrap();
    assert!(reset
        .accesses
        .iter()
        .filter(|access| access.resource.as_str() == "BALANCES")
        .all(|access| access.key_dependency.is_none()));

    let cancel = profiles
        .iter()
        .find(|profile| profile.entrypoint_name == "execute::CancelOrder")
        .unwrap();
    assert!(cancel
        .accesses
        .iter()
        .filter(|access| access.resource.as_str() == "BALANCES")
        .all(|access| {
            access.key_dependency.as_ref().is_some_and(|dependency| {
                dependency.dependency_kind == DependencyKind::State
                    && dependency.origin_input.is_none()
            })
        }));
}

#[test]
fn parses_miniwarehouse_multi_resource_profiles() {
    let raw = parse_slice(MINIWAREHOUSE).unwrap();
    assert_eq!(raw.contract, "miniwarehouse");
    assert_eq!(raw.profiles.len(), 14);

    let profiles = normalize_document(raw, &context(23)).unwrap();
    let new_order = profiles
        .iter()
        .find(|profile| profile.entrypoint_name == "execute::NewOrder")
        .unwrap();
    assert_eq!(new_order.accesses.len(), 10);
    for resource in [
        "WAREHOUSES",
        "DISTRICTS",
        "CUSTOMERS",
        "STOCK",
        "ORDERS",
        "NEW_ORDERS",
        "ORDER_LINES",
    ] {
        assert!(new_order
            .accesses
            .iter()
            .any(|access| access.resource.as_str() == resource));
    }
}

#[test]
fn preserves_miniwarehouse_prefix_and_state_derived_uncertainty() {
    let profiles = normalize_document(parse_slice(MINIWAREHOUSE).unwrap(), &context(24)).unwrap();
    let delivery = profiles
        .iter()
        .find(|profile| profile.entrypoint_name == "execute::Delivery")
        .unwrap();

    assert!(delivery
        .accesses
        .iter()
        .filter(|access| access.resource.as_str() == "ORDER_LINES")
        .all(|access| {
            access
                .key_dependency
                .as_ref()
                .is_some_and(|dependency| dependency.origin_input.is_none())
        }));
    assert!(delivery
        .accesses
        .iter()
        .filter(|access| access.resource.as_str() == "CUSTOMERS")
        .all(|access| {
            access.key_dependency.as_ref().is_some_and(|dependency| {
                dependency.dependency_kind == DependencyKind::State
                    && dependency.origin_input.is_none()
            })
        }));
}
