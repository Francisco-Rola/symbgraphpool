use bitflags::bitflags;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

use crate::{DelegationFrame, DependencyKind, ResourceFamily, StableProfileKey};

bitflags! {
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub struct ConflictKinds: u8 {
        const READ_WRITE  = 0b0000_0001;
        const WRITE_READ  = 0b0000_0010;
        const WRITE_WRITE = 0b0000_0100;
        const CALL        = 0b0000_1000;
        const BALANCE     = 0b0001_0000;
    }
}

impl Serialize for ConflictKinds {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.bits())
    }
}

impl<'de> Deserialize<'de> for ConflictKinds {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bits = u8::deserialize(deserializer)?;
        Self::from_bits(bits)
            .ok_or_else(|| D::Error::custom(format!("unknown conflict-kind bits: {bits:#010b}")))
    }
}

/// Coarse summary of a profile edge.
///
/// Online materialization must evaluate the predicate clauses; this value is metadata used for
/// inspection, priors, and fast-path selection. In particular, `Unknown` means at least one
/// alternative clause is unresolved, not that every concrete transaction pair is unknown.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRelation {
    Conditional,
    Unconditional,
    Unknown,
}

impl EdgeRelation {
    pub fn summarize_clauses(clauses: &[PredicateClause]) -> Self {
        if clauses
            .iter()
            .any(|clause| clause.resolution == ClauseResolution::Unconditional)
        {
            return Self::Unconditional;
        }
        if clauses
            .iter()
            .any(|clause| clause.resolution == ClauseResolution::Unknown)
        {
            return Self::Unknown;
        }
        Self::Conditional
    }
}

/// Offline resolution class for one alternative overlap clause.
///
/// This classification describes whether the key/scope portion can be decided from runtime
/// instance identity and transaction bindings. Runtime guard evaluation can still produce
/// `Unknown` when a guard depends on contract state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClauseResolution {
    Conditional,
    Unconditional,
    Unknown,
}

/// Why a clause cannot be fully resolved from the symbolic artifact or concrete bindings.
///
/// The first three variants can be attached statically to a persisted clause. The remaining
/// variants are also used by detailed runtime predicate diagnostics.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    StateDerivedKey,
    UnresolvedKey,
    UnknownScope,
    MissingInputBinding,
    UnsupportedExpression,
    StateDependentGuard,
    UnsupportedGuard,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileEdgeDefinition {
    pub source: StableProfileKey,
    pub target: StableProfileKey,
    pub relation: EdgeRelation,
    pub conflict_kinds: ConflictKinds,
    pub symbolic_score: f32,
    pub predicate: PredicateTemplate,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PredicateTemplate {
    /// Alternative overlap conditions. The edge can materialize when any clause evaluates true.
    pub clauses: Vec<PredicateClause>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PredicateClause {
    pub resource: ResourceFamily,
    pub semantic_key_component: String,
    #[serde(default = "legacy_clause_resolution")]
    pub resolution: ClauseResolution,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_reasons: Vec<UnknownReason>,
    pub require_same_contract_instance: bool,
    pub key_match: KeyMatch,
    pub left_guard: GuardRef,
    pub right_guard: GuardRef,
}

fn legacy_clause_resolution() -> ClauseResolution {
    ClauseResolution::Conditional
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyMatch {
    /// The semantic component is the complete logical key within one contract instance.
    WholeResource,
    /// Both accesses expose bound input expressions that can be compared online.
    InputEquality {
        left: BoundExpression,
        right: BoundExpression,
    },
    /// The analyzer identifies the key family but not enough concrete bindings to compare it.
    Unresolved,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct BoundExpression {
    pub expression: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegation_path: Vec<DelegationFrame>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct GuardRef {
    pub expression: String,
    pub dependency_kind: DependencyKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegation_path: Vec<DelegationFrame>,
}
