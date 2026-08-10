use std::{fmt, str::FromStr};

use blake3::Hasher;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

const PROFILE_KEY_DOMAIN: &[u8] = b"acg.profile-key.v1\0";
const SELECTOR_DOMAIN: &[u8] = b"acg.entrypoint-selector.v1\0";

/// Dense validator-local profile identifier.
///
/// This value is assigned when a graph artifact is loaded. It is not a stable persistence key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileId(pub u32);

/// Dense validator-local contract-instance identifier.
///
/// This identifier is assigned by a runtime adapter and is intentionally separate from
/// profile identity: multiple deployed instances can share one profile while maintaining
/// disjoint contract-local state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(pub u32);

/// Dense index of a concrete transaction inside one candidate block.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxIndex(pub u32);

/// Runtime-independent logical transaction identifier.
///
/// Candidate graph topology uses [`TxIndex`] for uniqueness; the logical identifier is kept
/// for tracing and correlation with the execution runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxId(pub u64);

/// Index of an edge in the immutable in-memory edge arrays.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileEdgeIndex(pub u32);

/// Stable 32-byte key used to persist and exchange profile references.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StableProfileKey(pub [u8; 32]);

impl StableProfileKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for StableProfileKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("StableProfileKey")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for StableProfileKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl FromStr for StableProfileKey {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = decode_fixed_hex::<32>("stable profile key", value)?;
        Ok(Self(bytes))
    }
}

impl Serialize for StableProfileKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for StableProfileKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

/// Canonical contract code hash used in profile identity.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContractCodeHash(pub [u8; 32]);

impl ContractCodeHash {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for ContractCodeHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ContractCodeHash")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for ContractCodeHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl FromStr for ContractCodeHash {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = decode_fixed_hex::<32>("contract code hash", value)?;
        Ok(Self(bytes))
    }
}

impl Serialize for ContractCodeHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ContractCodeHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

/// Stable, human-readable runtime namespace used only outside the hot path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuntimeId(String);

impl RuntimeId {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
        let value = value.into();
        let canonical = value.trim().to_ascii_lowercase();
        if canonical.is_empty() {
            return Err(IdentityError::EmptyRuntimeId);
        }
        if !canonical.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        }) {
            return Err(IdentityError::InvalidRuntimeId(value));
        }
        Ok(Self(canonical))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RuntimeId {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for RuntimeId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RuntimeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl fmt::Display for RuntimeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Entrypoint class encoded into the stable profile descriptor.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntrypointKind {
    Instantiate,
    Execute,
    Query,
    Reply,
    Migrate,
    Other,
}

impl EntrypointKind {
    pub fn numeric_tag(self) -> u16 {
        match self {
            Self::Instantiate => 1,
            Self::Execute => 2,
            Self::Query => 3,
            Self::Reply => 4,
            Self::Migrate => 5,
            Self::Other => u16::MAX,
        }
    }
}

/// Numeric selector used in the canonical descriptor.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntrypointSelector(pub u64);

impl EntrypointSelector {
    /// Stable fallback for runtimes whose analyzer emits names instead of native numeric selectors.
    pub fn from_canonical_name(name: &str) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(SELECTOR_DOMAIN);
        put_len_prefixed(&mut hasher, name.as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&digest.as_bytes()[..8]);
        Self(u64::from_be_bytes(bytes))
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ProfileDescriptor {
    pub runtime_id: RuntimeId,
    pub contract_code_hash: ContractCodeHash,
    pub entrypoint_kind: EntrypointKind,
    pub numeric_entrypoint_selector: EntrypointSelector,
    pub profile_schema_version: u16,
}

impl ProfileDescriptor {
    pub fn stable_key(&self) -> StableProfileKey {
        let mut hasher = Hasher::new();
        hasher.update(PROFILE_KEY_DOMAIN);
        put_len_prefixed(&mut hasher, self.runtime_id.as_str().as_bytes());
        hasher.update(self.contract_code_hash.as_bytes());
        hasher.update(&self.entrypoint_kind.numeric_tag().to_be_bytes());
        hasher.update(&self.numeric_entrypoint_selector.0.to_be_bytes());
        hasher.update(&self.profile_schema_version.to_be_bytes());
        StableProfileKey(*hasher.finalize().as_bytes())
    }
}

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("runtime id cannot be empty")]
    EmptyRuntimeId,
    #[error("runtime id contains unsupported characters: {0:?}")]
    InvalidRuntimeId(String),
    #[error("invalid {label}: expected {expected} hex characters, got {actual}")]
    InvalidHexLength {
        label: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("invalid {label} hex: {source}")]
    InvalidHex {
        label: &'static str,
        #[source]
        source: hex::FromHexError,
    },
}

fn put_len_prefixed(hasher: &mut Hasher, value: &[u8]) {
    let length = u32::try_from(value.len()).expect("identity fields must fit into u32");
    hasher.update(&length.to_be_bytes());
    hasher.update(value);
}

fn decode_fixed_hex<const N: usize>(
    label: &'static str,
    value: &str,
) -> Result<[u8; N], IdentityError> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    if value.len() != N * 2 {
        return Err(IdentityError::InvalidHexLength {
            label,
            expected: N * 2,
            actual: value.len(),
        });
    }
    let bytes = hex::decode(value).map_err(|source| IdentityError::InvalidHex { label, source })?;
    let mut result = [0_u8; N];
    result.copy_from_slice(&bytes);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> ProfileDescriptor {
        ProfileDescriptor {
            runtime_id: RuntimeId::new("CosmWasm").unwrap(),
            contract_code_hash: ContractCodeHash([7; 32]),
            entrypoint_kind: EntrypointKind::Execute,
            numeric_entrypoint_selector: EntrypointSelector::from_canonical_name("execute::Swap"),
            profile_schema_version: 1,
        }
    }

    #[test]
    fn stable_key_is_deterministic() {
        assert_eq!(descriptor().stable_key(), descriptor().stable_key());
    }

    #[test]
    fn descriptor_fields_are_domain_separated() {
        let mut changed = descriptor();
        changed.profile_schema_version += 1;
        assert_ne!(descriptor().stable_key(), changed.stable_key());
    }

    #[test]
    fn fixed_keys_round_trip_as_hex() {
        let key = descriptor().stable_key();
        assert_eq!(key, key.to_string().parse().unwrap());
    }
}
