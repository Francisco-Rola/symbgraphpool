use std::collections::BTreeMap;

use acg_core::{
    AccessDescriptor, AccessMode, AccessScope, BoundExpression, ClauseResolution, ConflictKinds,
    ContractCodeHash, DependencyKind, EdgeRelation, GuardRef, KeyMatch, PredicateClause,
    PredicateTemplate, ProfileDefinition, ProfileEdgeDefinition, ResourceFamily, RuntimeId,
    SemanticKeyKind, StableProfileKey, UnknownReason,
};
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct EdgeBuildConfig {
    pub conditional_score: f32,
    pub unconditional_score: f32,
    pub unknown_score: f32,
}

impl Default for EdgeBuildConfig {
    fn default() -> Self {
        Self {
            conditional_score: 0.75,
            unconditional_score: 0.95,
            unknown_score: 0.50,
        }
    }
}

#[derive(Debug, Error)]
pub enum EdgeDerivationError {
    #[error("symbolic score {field} must be finite and within [0, 1], got {value}")]
    InvalidScore { field: &'static str, value: f32 },
    #[error("duplicate stable profile key {0}")]
    DuplicateProfile(StableProfileKey),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AccessIndexKey {
    runtime_id: RuntimeId,
    code_hash: ContractCodeHash,
    resource: ResourceFamily,
    semantic_component: String,
}

#[derive(Clone, Copy, Debug)]
struct AccessRef {
    profile_index: usize,
    access_index: usize,
}

#[derive(Clone, Debug)]
struct EdgeAccumulator {
    conflict_kinds: ConflictKinds,
    predicate: PredicateTemplate,
}

pub fn derive_profile_edges(
    profiles: &[ProfileDefinition],
    config: &EdgeBuildConfig,
) -> Result<Vec<ProfileEdgeDefinition>, EdgeDerivationError> {
    validate_config(config)?;
    validate_unique_profiles(profiles)?;

    let mut access_index = BTreeMap::<AccessIndexKey, Vec<AccessRef>>::new();
    for (profile_index, profile) in profiles.iter().enumerate() {
        for (access_index_in_profile, access) in profile.accesses.iter().enumerate() {
            for semantic_component in &access.semantic_key_components {
                access_index
                    .entry(AccessIndexKey {
                        runtime_id: profile.descriptor.runtime_id.clone(),
                        code_hash: profile.descriptor.contract_code_hash,
                        resource: access.resource.clone(),
                        semantic_component: semantic_component.clone(),
                    })
                    .or_default()
                    .push(AccessRef {
                        profile_index,
                        access_index: access_index_in_profile,
                    });
            }
        }
    }

    let mut edges = BTreeMap::<(StableProfileKey, StableProfileKey), EdgeAccumulator>::new();
    for (index_key, accesses) in access_index {
        for (left_position, &first_ref) in accesses.iter().enumerate() {
            for &second_ref in accesses.iter().skip(left_position) {
                let first_profile = &profiles[first_ref.profile_index];
                let second_profile = &profiles[second_ref.profile_index];
                let first_access = &first_profile.accesses[first_ref.access_index];
                let second_access = &second_profile.accesses[second_ref.access_index];

                if first_access.mode == AccessMode::Read && second_access.mode == AccessMode::Read {
                    continue;
                }

                let (source_profile, source_access, target_profile, target_access) =
                    orient_by_stable_key(
                        first_profile,
                        first_access,
                        second_profile,
                        second_access,
                    );
                let pair = (source_profile.stable_key, target_profile.stable_key);
                let self_profile = source_profile.stable_key == target_profile.stable_key;
                let clause = build_clause(
                    &index_key.resource,
                    &index_key.semantic_component,
                    source_access,
                    target_access,
                );
                let conflict_kinds =
                    classify_conflict_kinds(self_profile, source_access.mode, target_access.mode);

                let accumulator = edges.entry(pair).or_insert_with(|| EdgeAccumulator {
                    conflict_kinds: ConflictKinds::empty(),
                    predicate: PredicateTemplate::default(),
                });
                accumulator.conflict_kinds |= conflict_kinds;
                push_clause_if_new(&mut accumulator.predicate, clause);
                if self_profile {
                    let mirrored = build_clause(
                        &index_key.resource,
                        &index_key.semantic_component,
                        target_access,
                        source_access,
                    );
                    push_clause_if_new(&mut accumulator.predicate, mirrored);
                }
            }
        }
    }

    Ok(edges
        .into_iter()
        .map(|((source, target), mut accumulator)| {
            accumulator.predicate.clauses.sort();
            let relation = EdgeRelation::summarize_clauses(&accumulator.predicate.clauses);
            let symbolic_score = match relation {
                EdgeRelation::Conditional => config.conditional_score,
                EdgeRelation::Unconditional => config.unconditional_score,
                EdgeRelation::Unknown => config.unknown_score,
            };
            ProfileEdgeDefinition {
                source,
                target,
                relation,
                conflict_kinds: accumulator.conflict_kinds,
                symbolic_score,
                predicate: accumulator.predicate,
            }
        })
        .collect())
}

fn push_clause_if_new(predicate: &mut PredicateTemplate, clause: PredicateClause) {
    if !predicate.clauses.contains(&clause) {
        predicate.clauses.push(clause);
    }
}

fn validate_config(config: &EdgeBuildConfig) -> Result<(), EdgeDerivationError> {
    for (field, value) in [
        ("conditional_score", config.conditional_score),
        ("unconditional_score", config.unconditional_score),
        ("unknown_score", config.unknown_score),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(EdgeDerivationError::InvalidScore { field, value });
        }
    }
    Ok(())
}

fn validate_unique_profiles(profiles: &[ProfileDefinition]) -> Result<(), EdgeDerivationError> {
    let mut keys = profiles
        .iter()
        .map(|profile| profile.stable_key)
        .collect::<Vec<_>>();
    keys.sort();
    for pair in keys.windows(2) {
        if pair[0] == pair[1] {
            return Err(EdgeDerivationError::DuplicateProfile(pair[0]));
        }
    }
    Ok(())
}

fn orient_by_stable_key<'a>(
    first_profile: &'a ProfileDefinition,
    first_access: &'a AccessDescriptor,
    second_profile: &'a ProfileDefinition,
    second_access: &'a AccessDescriptor,
) -> (
    &'a ProfileDefinition,
    &'a AccessDescriptor,
    &'a ProfileDefinition,
    &'a AccessDescriptor,
) {
    if first_profile.stable_key <= second_profile.stable_key {
        (first_profile, first_access, second_profile, second_access)
    } else {
        (second_profile, second_access, first_profile, first_access)
    }
}

fn build_clause(
    resource: &ResourceFamily,
    semantic_component: &str,
    left: &AccessDescriptor,
    right: &AccessDescriptor,
) -> PredicateClause {
    let require_same_contract_instance =
        left.scope == AccessScope::ContractInstance || right.scope == AccessScope::ContractInstance;
    let key_match = match (left.semantic_key_kind, right.semantic_key_kind) {
        (SemanticKeyKind::FieldSet, SemanticKeyKind::FieldSet) => KeyMatch::WholeResource,
        _ => match (
            left.key_dependency
                .as_ref()
                .and_then(|dependency| dependency.origin_input.as_ref()),
            right
                .key_dependency
                .as_ref()
                .and_then(|dependency| dependency.origin_input.as_ref()),
        ) {
            (Some(left_input), Some(right_input)) => KeyMatch::InputEquality {
                left: BoundExpression {
                    expression: left_input.clone(),
                    delegation_path: left.delegation_path.clone(),
                },
                right: BoundExpression {
                    expression: right_input.clone(),
                    delegation_path: right.delegation_path.clone(),
                },
            },
            _ => KeyMatch::Unresolved,
        },
    };

    let mut unknown_reasons = classify_unknown_reasons(left, right, &key_match);
    unknown_reasons.sort();
    unknown_reasons.dedup();
    let resolution = if !unknown_reasons.is_empty() {
        ClauseResolution::Unknown
    } else if !require_same_contract_instance
        && matches!(&key_match, KeyMatch::WholeResource)
        && left.guard.is_unconditional()
        && right.guard.is_unconditional()
    {
        ClauseResolution::Unconditional
    } else {
        ClauseResolution::Conditional
    };

    PredicateClause {
        resource: resource.clone(),
        semantic_key_component: semantic_component.to_owned(),
        resolution,
        unknown_reasons,
        require_same_contract_instance,
        key_match,
        left_guard: GuardRef {
            expression: left.guard.expression.clone(),
            dependency_kind: left.guard.dependency_kind,
            delegation_path: left.delegation_path.clone(),
        },
        right_guard: GuardRef {
            expression: right.guard.expression.clone(),
            dependency_kind: right.guard.dependency_kind,
            delegation_path: right.delegation_path.clone(),
        },
    }
}

fn classify_unknown_reasons(
    left: &AccessDescriptor,
    right: &AccessDescriptor,
    key_match: &KeyMatch,
) -> Vec<UnknownReason> {
    let mut reasons = Vec::new();
    if left.scope == AccessScope::Unknown || right.scope == AccessScope::Unknown {
        reasons.push(UnknownReason::UnknownScope);
    }
    if matches!(key_match, KeyMatch::Unresolved) {
        append_unresolved_key_reason(&mut reasons, left);
        append_unresolved_key_reason(&mut reasons, right);
        if reasons.is_empty() {
            reasons.push(UnknownReason::UnresolvedKey);
        }
    }
    reasons
}

fn append_unresolved_key_reason(reasons: &mut Vec<UnknownReason>, access: &AccessDescriptor) {
    if access.semantic_key_kind == SemanticKeyKind::FieldSet {
        return;
    }
    match access.key_dependency.as_ref() {
        Some(dependency) if dependency.origin_input.is_some() => {}
        Some(dependency)
            if matches!(
                dependency.dependency_kind,
                DependencyKind::State | DependencyKind::InputAndState
            ) =>
        {
            reasons.push(UnknownReason::StateDerivedKey);
        }
        _ => reasons.push(UnknownReason::UnresolvedKey),
    }
}

fn classify_conflict_kinds(
    self_profile: bool,
    left: AccessMode,
    right: AccessMode,
) -> ConflictKinds {
    match (left, right) {
        (AccessMode::Read, AccessMode::Read) => ConflictKinds::empty(),
        (AccessMode::Read, AccessMode::Write) if self_profile => {
            ConflictKinds::READ_WRITE | ConflictKinds::WRITE_READ
        }
        (AccessMode::Write, AccessMode::Read) if self_profile => {
            ConflictKinds::READ_WRITE | ConflictKinds::WRITE_READ
        }
        (AccessMode::Read, AccessMode::Write) => ConflictKinds::READ_WRITE,
        (AccessMode::Write, AccessMode::Read) => ConflictKinds::WRITE_READ,
        (AccessMode::Write, AccessMode::Write) => ConflictKinds::WRITE_WRITE,
    }
}
