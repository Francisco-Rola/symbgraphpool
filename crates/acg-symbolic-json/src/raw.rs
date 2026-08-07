use std::{collections::BTreeMap, io::Read};

use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize)]
pub struct RawAnalyzerDocument {
    pub schema_version: String,
    pub contract: String,
    pub source: String,
    #[serde(default)]
    pub storage_resources: BTreeMap<String, RawStorageResource>,
    #[serde(default)]
    pub dependency_kinds: BTreeMap<String, String>,
    pub profiles: Vec<RawProfile>,
    #[serde(default)]
    pub semantic_key_rules: BTreeMap<String, String>,
}

impl RawAnalyzerDocument {
    pub fn validate_layout_version(&self) -> Result<(), RawParseError> {
        let numeric = self
            .schema_version
            .split_once('-')
            .map(|(numeric, _)| numeric)
            .unwrap_or(self.schema_version.as_str());
        if numeric != "3.1" {
            return Err(RawParseError::UnsupportedSchemaVersion(
                self.schema_version.clone(),
            ));
        }
        if self.contract.trim().is_empty() {
            return Err(RawParseError::InvalidDocument(
                "contract name cannot be empty".to_owned(),
            ));
        }
        if self.profiles.is_empty() {
            return Err(RawParseError::InvalidDocument(
                "document must contain at least one profile".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct RawStorageResource {
    pub key_semantic_name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RawProfile {
    pub entrypoint: String,
    #[serde(default)]
    pub input_parameters: Vec<String>,
    #[serde(default)]
    pub accesses: Vec<RawAccess>,
    #[serde(default)]
    pub delegates_to: Option<RawDelegation>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RawAccess {
    pub kind: String,
    pub resource: String,
    pub key: RawKey,
    pub guard: RawGuard,
    #[serde(default)]
    pub evidence: Option<RawEvidence>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RawKey {
    pub semantic_name: RawSemanticName,
    #[serde(default)]
    pub depends_on: Option<RawDependency>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum RawSemanticName {
    Scalar(String),
    List(Vec<String>),
}

impl RawSemanticName {
    pub fn into_components(self) -> Vec<String> {
        match self {
            Self::Scalar(value) => vec![value],
            Self::List(values) => values,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct RawDependency {
    pub dependency_kind: String,
    #[serde(default)]
    pub origin_input: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RawGuard {
    pub expression: String,
    pub dependency_kind: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RawEvidence {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub code: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RawDelegation {
    pub entrypoint: String,
    #[serde(default)]
    pub input_mapping: BTreeMap<String, String>,
}

#[derive(Debug, Error)]
pub enum RawParseError {
    #[error("failed to parse symbolic analyzer JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported analyzer schema version {0:?}; this parser supports the 3.1 layout")]
    UnsupportedSchemaVersion(String),
    #[error("invalid analyzer document: {0}")]
    InvalidDocument(String),
}

pub fn parse_reader(reader: impl Read) -> Result<RawAnalyzerDocument, RawParseError> {
    let document: RawAnalyzerDocument = serde_json::from_reader(reader)?;
    document.validate_layout_version()?;
    Ok(document)
}

pub fn parse_slice(bytes: &[u8]) -> Result<RawAnalyzerDocument, RawParseError> {
    let document: RawAnalyzerDocument = serde_json::from_slice(bytes)?;
    document.validate_layout_version()?;
    Ok(document)
}
