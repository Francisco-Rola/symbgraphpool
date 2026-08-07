use serde::{Deserialize, Serialize};

use crate::{ProfileDescriptor, StableProfileKey};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProfileDefinition {
    pub stable_key: StableProfileKey,
    pub descriptor: ProfileDescriptor,
    pub contract_name: String,
    pub source: String,
    pub analyzer_schema_version: String,
    pub entrypoint_name: String,
    pub input_parameters: Vec<String>,
    pub accesses: Vec<AccessDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegates_to: Option<Delegation>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceFamily(pub String);

impl ResourceFamily {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Input,
    State,
    InputAndState,
    None,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticKeyKind {
    /// Analyzer emitted a list of independently addressable fields.
    FieldSet,
    /// Analyzer emitted one logical storage-key family, potentially requiring input substitution.
    LogicalKey,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessScope {
    ContractInstance,
    Global,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccessDescriptor {
    pub mode: AccessMode,
    pub resource: ResourceFamily,
    /// Canonical sorted semantic components. A scalar analyzer key becomes a one-element vector.
    pub semantic_key_components: Vec<String>,
    pub semantic_key_kind: SemanticKeyKind,
    /// Empty for direct accesses; outermost delegation frame first for inherited accesses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegation_path: Vec<DelegationFrame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_dependency: Option<KeyDependency>,
    pub guard: GuardExpression,
    pub scope: AccessScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeyDependency {
    pub dependency_kind: DependencyKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_input: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GuardExpression {
    pub expression: String,
    pub dependency_kind: DependencyKind,
}

impl GuardExpression {
    pub fn is_unconditional(&self) -> bool {
        self.dependency_kind == DependencyKind::None && self.expression.trim() == "true"
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub code: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Delegation {
    pub entrypoint: String,
    pub input_mapping: Vec<InputMapping>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct InputMapping {
    pub target: String,
    pub expression: String,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DelegationFrame {
    pub target_entrypoint: String,
    pub input_mapping: Vec<InputMapping>,
}
