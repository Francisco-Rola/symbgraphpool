use acg_core::{
    BoundExpression, ClauseResolution, DelegationFrame, DependencyKind, GuardRef, InputMapping,
    InstanceId, KeyMatch, PredicateClause, PredicateTemplate, ResourceFamily, UnknownReason,
};
use acg_predicate::{CompiledExpression, CompiledPredicate, InputBindings, PredicateResult};
use serde_json::json;

fn predicate_clause(
    key_match: KeyMatch,
    left_guard: (&str, DependencyKind),
    resolution: ClauseResolution,
    unknown_reasons: Vec<UnknownReason>,
) -> PredicateClause {
    PredicateClause {
        resource: ResourceFamily("TEST".to_owned()),
        semantic_key_component: "key".to_owned(),
        resolution,
        unknown_reasons,
        require_same_contract_instance: true,
        key_match,
        left_guard: GuardRef {
            expression: left_guard.0.to_owned(),
            dependency_kind: left_guard.1,
            delegation_path: Vec::new(),
        },
        right_guard: GuardRef {
            expression: "true".to_owned(),
            dependency_kind: DependencyKind::None,
            delegation_path: Vec::new(),
        },
    }
}

fn clause(key_match: KeyMatch, left_guard: (&str, DependencyKind)) -> PredicateTemplate {
    let (resolution, unknown_reasons) = if matches!(key_match, KeyMatch::Unresolved) {
        (
            ClauseResolution::Unknown,
            vec![UnknownReason::UnresolvedKey],
        )
    } else {
        (ClauseResolution::Conditional, Vec::new())
    };
    PredicateTemplate {
        clauses: vec![predicate_clause(
            key_match,
            left_guard,
            resolution,
            unknown_reasons,
        )],
    }
}

fn input_equality(left: &str, right: &str) -> KeyMatch {
    KeyMatch::InputEquality {
        left: BoundExpression {
            expression: left.to_owned(),
            delegation_path: Vec::new(),
        },
        right: BoundExpression {
            expression: right.to_owned(),
            delegation_path: Vec::new(),
        },
    }
}

#[test]
fn compares_simple_and_tuple_input_keys() {
    let simple = CompiledPredicate::compile(&clause(
        input_equality("account", "account"),
        ("true", DependencyKind::None),
    ));
    let alice = InputBindings::from_value(json!({"account": "alice"}));
    let bob = InputBindings::from_value(json!({"account": "bob"}));
    assert_eq!(
        simple.evaluate(InstanceId(1), &alice, InstanceId(1), &alice),
        PredicateResult::True
    );
    assert_eq!(
        simple.evaluate(InstanceId(1), &alice, InstanceId(1), &bob),
        PredicateResult::False
    );

    let tuple = CompiledPredicate::compile(&clause(
        input_equality("(owner, spender)", "(owner, spender)"),
        ("true", DependencyKind::None),
    ));
    let first = InputBindings::from_value(json!({"owner":"alice", "spender":"bob"}));
    let second = InputBindings::from_value(json!({"owner":"alice", "spender":"carol"}));
    assert_eq!(
        tuple.evaluate(InstanceId(1), &first, InstanceId(1), &second),
        PredicateResult::False
    );
}

#[test]
fn wildcard_tuple_keeps_array_indices_synchronized() {
    let expression =
        CompiledExpression::compile("(lines[i].supply_warehouse_id, lines[i].item_id)");
    let bindings = InputBindings::from_value(json!({
        "lines": [
            {"supply_warehouse_id": 1, "item_id": 10},
            {"supply_warehouse_id": 2, "item_id": 20}
        ]
    }));
    let values = bindings.get(&expression);
    assert!(values.complete);
    assert_eq!(values.values.len(), 2);

    let predicate = CompiledPredicate::compile(&clause(
        input_equality(
            "(lines[i].supply_warehouse_id, lines[i].item_id)",
            "(warehouse_id, item_id)",
        ),
        ("true", DependencyKind::None),
    ));
    let matching = InputBindings::from_value(json!({"warehouse_id": 2, "item_id": 20}));
    let crossed = InputBindings::from_value(json!({"warehouse_id": 1, "item_id": 20}));
    assert_eq!(
        predicate.evaluate(InstanceId(3), &bindings, InstanceId(3), &matching),
        PredicateResult::True
    );
    assert_eq!(
        predicate.evaluate(InstanceId(3), &bindings, InstanceId(3), &crossed),
        PredicateResult::False
    );
}

#[test]
fn same_instance_requirement_prunes_cross_contract_storage() {
    let predicate = CompiledPredicate::compile(&clause(
        input_equality("account", "account"),
        ("true", DependencyKind::None),
    ));
    let bindings = InputBindings::from_value(json!({"account": "alice"}));
    assert_eq!(
        predicate.evaluate(InstanceId(1), &bindings, InstanceId(2), &bindings),
        PredicateResult::False
    );
}

#[test]
fn input_guard_can_disprove_edge_but_state_guard_stays_unknown() {
    let input_guard = CompiledPredicate::compile(&clause(
        input_equality("from", "from"),
        ("info.sender == from", DependencyKind::Input),
    ));
    let valid = InputBindings::from_value(json!({"from":"alice", "info":{"sender":"alice"}}));
    let invalid = InputBindings::from_value(json!({"from":"alice", "info":{"sender":"mallory"}}));
    assert_eq!(
        input_guard.evaluate(InstanceId(1), &valid, InstanceId(1), &valid),
        PredicateResult::True
    );
    assert_eq!(
        input_guard.evaluate(InstanceId(1), &invalid, InstanceId(1), &valid),
        PredicateResult::False
    );

    let state_guard = CompiledPredicate::compile(&clause(
        input_equality("account", "account"),
        (
            "expected_epoch == CONFIG.epoch",
            DependencyKind::InputAndState,
        ),
    ));
    let state_bindings = InputBindings::from_value(json!({"account":"alice", "expected_epoch":7}));
    let detailed = state_guard.evaluate_detailed(
        InstanceId(1),
        &state_bindings,
        InstanceId(1),
        &state_bindings,
    );
    assert_eq!(detailed.result, PredicateResult::Unknown);
    assert!(detailed.clauses[0]
        .unknown_reasons
        .contains(&UnknownReason::StateDependentGuard));
}

#[test]
fn unresolved_key_is_conservative_and_reports_static_reason() {
    let predicate = CompiledPredicate::compile(&clause(
        KeyMatch::Unresolved,
        ("true", DependencyKind::None),
    ));
    let bindings = InputBindings::empty();
    let detailed = predicate.evaluate_detailed(InstanceId(1), &bindings, InstanceId(1), &bindings);
    assert_eq!(detailed.result, PredicateResult::Unknown);
    assert_eq!(detailed.clauses[0].resolution, ClauseResolution::Unknown);
    assert_eq!(
        detailed.clauses[0].unknown_reasons,
        vec![UnknownReason::UnresolvedKey]
    );
}

#[test]
fn mixed_clauses_use_three_valued_or_without_edge_level_shortcut() {
    let mixed = PredicateTemplate {
        clauses: vec![
            predicate_clause(
                input_equality("account", "account"),
                ("true", DependencyKind::None),
                ClauseResolution::Conditional,
                Vec::new(),
            ),
            predicate_clause(
                KeyMatch::Unresolved,
                ("true", DependencyKind::None),
                ClauseResolution::Unknown,
                vec![UnknownReason::StateDerivedKey],
            ),
        ],
    };
    let predicate = CompiledPredicate::compile(&mixed);
    let alice = InputBindings::from_value(json!({"account":"alice"}));
    let bob = InputBindings::from_value(json!({"account":"bob"}));

    let true_or_unknown = predicate.evaluate_detailed(InstanceId(1), &alice, InstanceId(1), &alice);
    assert_eq!(true_or_unknown.result, PredicateResult::True);
    assert_eq!(true_or_unknown.clauses[0].result, PredicateResult::True);
    assert_eq!(true_or_unknown.clauses[1].result, PredicateResult::Unknown);

    let false_or_unknown = predicate.evaluate_detailed(InstanceId(1), &alice, InstanceId(1), &bob);
    assert_eq!(false_or_unknown.result, PredicateResult::Unknown);
    assert_eq!(false_or_unknown.clauses[0].result, PredicateResult::False);
    assert_eq!(false_or_unknown.clauses[1].result, PredicateResult::Unknown);
    assert!(false_or_unknown.clauses[1]
        .unknown_reasons
        .contains(&UnknownReason::StateDerivedKey));

    let both_precise = PredicateTemplate {
        clauses: vec![
            predicate_clause(
                input_equality("account", "account"),
                ("true", DependencyKind::None),
                ClauseResolution::Conditional,
                Vec::new(),
            ),
            predicate_clause(
                input_equality("shard_id", "shard_id"),
                ("true", DependencyKind::None),
                ClauseResolution::Conditional,
                Vec::new(),
            ),
        ],
    };
    let precise = CompiledPredicate::compile(&both_precise);
    let left = InputBindings::from_value(json!({"account":"alice", "shard_id":1}));
    let right = InputBindings::from_value(json!({"account":"bob", "shard_id":2}));
    assert_eq!(
        precise.evaluate(InstanceId(1), &left, InstanceId(1), &right),
        PredicateResult::False
    );
}

#[test]
fn detailed_evaluation_reports_unsupported_expression_and_missing_binding() {
    let unsupported = CompiledPredicate::compile(&clause(
        input_equality("Token { contract_addr: info.sender }", "account"),
        ("true", DependencyKind::None),
    ));
    let bindings = InputBindings::from_value(json!({"account":"alice"}));
    let detailed =
        unsupported.evaluate_detailed(InstanceId(1), &bindings, InstanceId(1), &bindings);
    assert_eq!(detailed.result, PredicateResult::Unknown);
    assert!(detailed.clauses[0]
        .unknown_reasons
        .contains(&UnknownReason::UnsupportedExpression));

    let missing = CompiledPredicate::compile(&clause(
        input_equality("account", "account"),
        ("true", DependencyKind::None),
    ));
    let empty = InputBindings::empty();
    let detailed = missing.evaluate_detailed(InstanceId(1), &empty, InstanceId(1), &bindings);
    assert_eq!(detailed.result, PredicateResult::Unknown);
    assert!(detailed.clauses[0]
        .unknown_reasons
        .contains(&UnknownReason::MissingInputBinding));
}

#[test]
fn delegation_path_remaps_guard_operands() {
    let frame = DelegationFrame {
        target_entrypoint: "execute::Inner".to_owned(),
        input_mapping: vec![InputMapping {
            target: "owner".to_owned(),
            expression: "delegated_owner".to_owned(),
        }],
    };
    let template = PredicateTemplate {
        clauses: vec![PredicateClause {
            resource: ResourceFamily("TEST".to_owned()),
            semantic_key_component: "key".to_owned(),
            resolution: ClauseResolution::Conditional,
            unknown_reasons: Vec::new(),
            require_same_contract_instance: true,
            key_match: KeyMatch::WholeResource,
            left_guard: GuardRef {
                expression: "info.sender == owner".to_owned(),
                dependency_kind: DependencyKind::Input,
                delegation_path: vec![frame],
            },
            right_guard: GuardRef {
                expression: "true".to_owned(),
                dependency_kind: DependencyKind::None,
                delegation_path: Vec::new(),
            },
        }],
    };
    let predicate = CompiledPredicate::compile(&template);
    let valid = InputBindings::from_value(json!({
        "delegated_owner": "alice",
        "info": {"sender": "alice"}
    }));
    let invalid = InputBindings::from_value(json!({
        "delegated_owner": "alice",
        "info": {"sender": "mallory"}
    }));
    assert_eq!(
        predicate.evaluate(InstanceId(1), &valid, InstanceId(1), &valid),
        PredicateResult::True
    );
    assert_eq!(
        predicate.evaluate(InstanceId(1), &invalid, InstanceId(1), &valid),
        PredicateResult::False
    );
}

#[test]
fn miniwarehouse_non_empty_collection_guard_is_input_resolvable() {
    let predicate = CompiledPredicate::compile(&clause(
        input_equality("warehouse_id", "warehouse_id"),
        ("lines is non-empty", DependencyKind::Input),
    ));
    let populated = InputBindings::from_value(json!({
        "warehouse_id": 1,
        "lines": [{"item_id": 10}]
    }));
    let empty = InputBindings::from_value(json!({
        "warehouse_id": 1,
        "lines": []
    }));
    let missing = InputBindings::from_value(json!({"warehouse_id": 1}));

    assert_eq!(
        predicate.evaluate(InstanceId(1), &populated, InstanceId(1), &populated),
        PredicateResult::True
    );
    assert_eq!(
        predicate.evaluate(InstanceId(1), &empty, InstanceId(1), &populated),
        PredicateResult::False
    );
    let detailed = predicate.evaluate_detailed(InstanceId(1), &missing, InstanceId(1), &populated);
    assert_eq!(detailed.result, PredicateResult::Unknown);
    assert!(detailed.clauses[0]
        .unknown_reasons
        .contains(&UnknownReason::MissingInputBinding));
    assert!(!detailed.clauses[0]
        .unknown_reasons
        .contains(&UnknownReason::UnsupportedGuard));
}
