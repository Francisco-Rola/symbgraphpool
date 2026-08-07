use std::collections::{BTreeMap, BTreeSet, HashSet};

use acg_core::{
    AccessDescriptor, AccessMode, AccessScope, ContractCodeHash, Delegation, DelegationFrame,
    DependencyKind, EntrypointKind, EntrypointSelector, Evidence, GuardExpression, InputMapping,
    KeyDependency, ProfileDefinition, ProfileDescriptor, ResourceFamily, RuntimeId,
    SemanticKeyKind,
};
use thiserror::Error;

use crate::raw::{RawAccess, RawAnalyzerDocument, RawDependency, RawProfile};

/// Identity fields supplied by the contract/runtime integration rather than the analyzer JSON.
#[derive(Clone, Debug)]
pub struct IngestionContext {
    pub runtime_id: RuntimeId,
    pub contract_code_hash: ContractCodeHash,
    pub profile_schema_version: u16,
    /// Optional chain-native selector overrides keyed by the exact analyzer entrypoint name.
    pub selector_overrides: BTreeMap<String, EntrypointSelector>,
}

impl IngestionContext {
    pub fn new(
        runtime_id: RuntimeId,
        contract_code_hash: ContractCodeHash,
        profile_schema_version: u16,
    ) -> Self {
        Self {
            runtime_id,
            contract_code_hash,
            profile_schema_version,
            selector_overrides: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum NormalizeError {
    #[error("profile schema version must be non-zero")]
    InvalidProfileSchemaVersion,
    #[error("profile entrypoint cannot be empty")]
    EmptyEntrypoint,
    #[error("duplicate entrypoint {0:?} in analyzer document")]
    DuplicateEntrypoint(String),
    #[error(
        "entrypoint selector collision in {kind:?} between {first:?} and {second:?}: {selector}"
    )]
    SelectorCollision {
        kind: EntrypointKind,
        first: String,
        second: String,
        selector: u64,
    },
    #[error("selector override references unknown entrypoint {0:?}")]
    UnknownSelectorOverride(String),
    #[error(
        "entrypoint {source_entrypoint:?} delegates to missing entrypoint {target_entrypoint:?}"
    )]
    MissingDelegationTarget {
        source_entrypoint: String,
        target_entrypoint: String,
    },
    #[error("delegation cycle detected at entrypoint {0:?}")]
    DelegationCycle(String),
    #[error("entrypoint {entrypoint:?} has an access with empty resource family")]
    EmptyResource { entrypoint: String },
    #[error("entrypoint {entrypoint:?} references undeclared resource family {resource:?}")]
    UnknownResource {
        entrypoint: String,
        resource: String,
    },
    #[error("entrypoint {entrypoint:?}, resource {resource:?}: semantic key cannot be empty")]
    EmptySemanticKey {
        entrypoint: String,
        resource: String,
    },
    #[error("entrypoint {entrypoint:?}, resource {resource:?}: unsupported access kind {kind:?}")]
    UnsupportedAccessKind {
        entrypoint: String,
        resource: String,
        kind: String,
    },
    #[error(
        "entrypoint {entrypoint:?}, resource {resource:?}: unsupported dependency kind {kind:?}"
    )]
    UnsupportedDependencyKind {
        entrypoint: String,
        resource: String,
        kind: String,
    },
}

pub fn normalize_document(
    document: RawAnalyzerDocument,
    context: &IngestionContext,
) -> Result<Vec<ProfileDefinition>, NormalizeError> {
    if context.profile_schema_version == 0 {
        return Err(NormalizeError::InvalidProfileSchemaVersion);
    }
    let known_resources = document
        .storage_resources
        .keys()
        .cloned()
        .collect::<HashSet<_>>();
    let mut seen_entrypoints = HashSet::new();
    let mut seen_selectors = BTreeMap::<(EntrypointKind, EntrypointSelector), String>::new();
    let mut profiles = Vec::with_capacity(document.profiles.len());

    for raw_profile in document.profiles {
        let RawProfile {
            entrypoint,
            input_parameters,
            accesses: raw_accesses,
            delegates_to: raw_delegation,
            notes,
        } = raw_profile;
        let entrypoint_name = entrypoint.trim().to_owned();
        if entrypoint_name.is_empty() {
            return Err(NormalizeError::EmptyEntrypoint);
        }
        if !seen_entrypoints.insert(entrypoint_name.clone()) {
            return Err(NormalizeError::DuplicateEntrypoint(entrypoint_name));
        }

        let entrypoint_kind = classify_entrypoint(&entrypoint_name);
        let selector = context
            .selector_overrides
            .get(&entrypoint_name)
            .copied()
            .unwrap_or_else(|| EntrypointSelector::from_canonical_name(&entrypoint_name));
        if let Some(first) =
            seen_selectors.insert((entrypoint_kind, selector), entrypoint_name.clone())
        {
            return Err(NormalizeError::SelectorCollision {
                kind: entrypoint_kind,
                first,
                second: entrypoint_name.clone(),
                selector: selector.0,
            });
        }

        let descriptor = ProfileDescriptor {
            runtime_id: context.runtime_id.clone(),
            contract_code_hash: context.contract_code_hash,
            entrypoint_kind,
            numeric_entrypoint_selector: selector,
            profile_schema_version: context.profile_schema_version,
        };
        let stable_key = descriptor.stable_key();
        let accesses = normalize_accesses(&entrypoint_name, raw_accesses, &known_resources)?;
        let delegates_to = raw_delegation.map(|delegation| Delegation {
            entrypoint: delegation.entrypoint,
            input_mapping: delegation
                .input_mapping
                .into_iter()
                .map(|(target, expression)| InputMapping { target, expression })
                .collect(),
        });

        profiles.push(ProfileDefinition {
            stable_key,
            descriptor,
            contract_name: document.contract.clone(),
            source: document.source.clone(),
            analyzer_schema_version: document.schema_version.clone(),
            entrypoint_name,
            input_parameters: normalize_strings(input_parameters),
            accesses,
            delegates_to,
            notes: normalize_strings(notes),
        });
    }

    if let Some(unknown) = context
        .selector_overrides
        .keys()
        .find(|entrypoint| !seen_entrypoints.contains(entrypoint.as_str()))
    {
        return Err(NormalizeError::UnknownSelectorOverride(unknown.clone()));
    }

    expand_delegations(&mut profiles)?;
    Ok(profiles)
}

fn expand_delegations(profiles: &mut [ProfileDefinition]) -> Result<(), NormalizeError> {
    let entrypoint_to_index = profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| (profile.entrypoint_name.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let entrypoint_names = profiles
        .iter()
        .map(|profile| profile.entrypoint_name.clone())
        .collect::<Vec<_>>();
    let direct_accesses = profiles
        .iter()
        .map(|profile| profile.accesses.clone())
        .collect::<Vec<_>>();
    let delegations = profiles
        .iter()
        .map(|profile| profile.delegates_to.clone())
        .collect::<Vec<_>>();
    let mut visit_state = vec![VisitState::Unvisited; profiles.len()];
    let mut memo = vec![None::<Vec<AccessDescriptor>>; profiles.len()];

    for (index, profile) in profiles.iter_mut().enumerate() {
        profile.accesses = resolve_effective_accesses(
            index,
            &entrypoint_names,
            &direct_accesses,
            &delegations,
            &entrypoint_to_index,
            &mut visit_state,
            &mut memo,
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisitState {
    Unvisited,
    Visiting,
    Complete,
}

fn resolve_effective_accesses(
    index: usize,
    entrypoint_names: &[String],
    direct_accesses: &[Vec<AccessDescriptor>],
    delegations: &[Option<Delegation>],
    entrypoint_to_index: &BTreeMap<String, usize>,
    visit_state: &mut [VisitState],
    memo: &mut [Option<Vec<AccessDescriptor>>],
) -> Result<Vec<AccessDescriptor>, NormalizeError> {
    if let Some(cached) = &memo[index] {
        return Ok(cached.clone());
    }
    if visit_state[index] == VisitState::Visiting {
        return Err(NormalizeError::DelegationCycle(
            entrypoint_names[index].clone(),
        ));
    }

    visit_state[index] = VisitState::Visiting;
    let mut accesses = direct_accesses[index].clone();
    if let Some(delegation) = &delegations[index] {
        let Some(&target_index) = entrypoint_to_index.get(&delegation.entrypoint) else {
            return Err(NormalizeError::MissingDelegationTarget {
                source_entrypoint: entrypoint_names[index].clone(),
                target_entrypoint: delegation.entrypoint.clone(),
            });
        };
        let inherited = resolve_effective_accesses(
            target_index,
            entrypoint_names,
            direct_accesses,
            delegations,
            entrypoint_to_index,
            visit_state,
            memo,
        )?;
        let frame = DelegationFrame {
            target_entrypoint: delegation.entrypoint.clone(),
            input_mapping: delegation.input_mapping.clone(),
        };
        accesses.extend(
            inherited
                .into_iter()
                .map(|access| inherit_access(access, &frame)),
        );
    }

    visit_state[index] = VisitState::Complete;
    memo[index] = Some(accesses.clone());
    Ok(accesses)
}

fn inherit_access(mut access: AccessDescriptor, frame: &DelegationFrame) -> AccessDescriptor {
    if let Some(origin_input) = access
        .key_dependency
        .as_mut()
        .and_then(|dependency| dependency.origin_input.as_mut())
    {
        if let Some(mapping) = frame
            .input_mapping
            .iter()
            .find(|mapping| mapping.target.as_str() == origin_input.as_str())
        {
            *origin_input = mapping.expression.clone();
        }
    }
    access.delegation_path.insert(0, frame.clone());
    access
}

fn classify_entrypoint(entrypoint: &str) -> EntrypointKind {
    if entrypoint == "instantiate" {
        EntrypointKind::Instantiate
    } else if entrypoint == "migrate" {
        EntrypointKind::Migrate
    } else if entrypoint.starts_with("execute::") {
        EntrypointKind::Execute
    } else if entrypoint.starts_with("query::") {
        EntrypointKind::Query
    } else if entrypoint.starts_with("reply::") {
        EntrypointKind::Reply
    } else {
        EntrypointKind::Other
    }
}

fn normalize_accesses(
    entrypoint: &str,
    accesses: Vec<RawAccess>,
    known_resources: &HashSet<String>,
) -> Result<Vec<AccessDescriptor>, NormalizeError> {
    accesses
        .into_iter()
        .map(|access| normalize_access(entrypoint, access, known_resources))
        .collect()
}

fn normalize_access(
    entrypoint: &str,
    access: RawAccess,
    known_resources: &HashSet<String>,
) -> Result<AccessDescriptor, NormalizeError> {
    let resource = access.resource.trim().to_owned();
    if resource.is_empty() {
        return Err(NormalizeError::EmptyResource {
            entrypoint: entrypoint.to_owned(),
        });
    }
    if !known_resources.is_empty() && !known_resources.contains(&resource) {
        return Err(NormalizeError::UnknownResource {
            entrypoint: entrypoint.to_owned(),
            resource,
        });
    }

    let mode = match access.kind.as_str() {
        "read" => AccessMode::Read,
        "write" => AccessMode::Write,
        kind => {
            return Err(NormalizeError::UnsupportedAccessKind {
                entrypoint: entrypoint.to_owned(),
                resource,
                kind: kind.to_owned(),
            })
        }
    };

    let semantic_key_kind = match &access.key.semantic_name {
        crate::raw::RawSemanticName::Scalar(_) => SemanticKeyKind::LogicalKey,
        crate::raw::RawSemanticName::List(_) => SemanticKeyKind::FieldSet,
    };
    let mut components = BTreeSet::new();
    for component in access.key.semantic_name.into_components() {
        let component = component.trim();
        if !component.is_empty() {
            components.insert(component.to_owned());
        }
    }
    if components.is_empty() {
        return Err(NormalizeError::EmptySemanticKey {
            entrypoint: entrypoint.to_owned(),
            resource,
        });
    }

    let key_dependency = access
        .key
        .depends_on
        .map(|dependency| normalize_key_dependency(entrypoint, &resource, dependency))
        .transpose()?;
    let guard_dependency =
        normalize_dependency_kind(entrypoint, &resource, &access.guard.dependency_kind)?;

    Ok(AccessDescriptor {
        mode,
        resource: ResourceFamily(resource),
        semantic_key_components: components.into_iter().collect(),
        semantic_key_kind,
        delegation_path: Vec::new(),
        key_dependency,
        guard: GuardExpression {
            expression: access.guard.expression.trim().to_owned(),
            dependency_kind: guard_dependency,
        },
        // The current analyzer schema describes contract-local storage resources.
        scope: AccessScope::ContractInstance,
        evidence: access.evidence.map(|evidence| Evidence {
            file: evidence.file,
            start_line: evidence.start_line,
            end_line: evidence.end_line,
            code: evidence.code,
        }),
        notes: normalize_strings(access.notes),
    })
}

fn normalize_key_dependency(
    entrypoint: &str,
    resource: &str,
    dependency: RawDependency,
) -> Result<KeyDependency, NormalizeError> {
    Ok(KeyDependency {
        dependency_kind: normalize_dependency_kind(
            entrypoint,
            resource,
            &dependency.dependency_kind,
        )?,
        origin_input: dependency
            .origin_input
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
    })
}

fn normalize_dependency_kind(
    entrypoint: &str,
    resource: &str,
    kind: &str,
) -> Result<DependencyKind, NormalizeError> {
    match kind {
        "input" => Ok(DependencyKind::Input),
        "state" => Ok(DependencyKind::State),
        "input_and_state" => Ok(DependencyKind::InputAndState),
        "none" => Ok(DependencyKind::None),
        "unknown" => Ok(DependencyKind::Unknown),
        _ => Err(NormalizeError::UnsupportedDependencyKind {
            entrypoint: entrypoint.to_owned(),
            resource: resource.to_owned(),
            kind: kind.to_owned(),
        }),
    }
}

fn normalize_strings(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}
