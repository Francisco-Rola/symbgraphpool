use acg_core::{ProfileDefinition, ProfileEdgeDefinition};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{derive_profile_edges, EdgeBuildConfig, EdgeDerivationError};

pub const PROFILE_GRAPH_ARTIFACT_VERSION: u16 = 2;

/// Portable graph artifact. Endpoints use stable keys; dense IDs are assigned only at load time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileGraphArtifact {
    pub format_version: u16,
    pub profiles: Vec<ProfileDefinition>,
    pub edges: Vec<ProfileEdgeDefinition>,
}

impl ProfileGraphArtifact {
    pub fn compile(
        mut profiles: Vec<ProfileDefinition>,
        config: &EdgeBuildConfig,
    ) -> Result<Self, ArtifactBuildError> {
        profiles.sort_by_key(|profile| profile.stable_key);
        let edges = derive_profile_edges(&profiles, config)?;
        Ok(Self {
            format_version: PROFILE_GRAPH_ARTIFACT_VERSION,
            profiles,
            edges,
        })
    }

    pub fn to_pretty_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, ArtifactReadError> {
        let artifact: Self = serde_json::from_slice(bytes)?;
        if artifact.format_version != PROFILE_GRAPH_ARTIFACT_VERSION {
            return Err(ArtifactReadError::UnsupportedFormatVersion {
                actual: artifact.format_version,
                supported: PROFILE_GRAPH_ARTIFACT_VERSION,
            });
        }
        Ok(artifact)
    }
}

#[derive(Debug, Error)]
pub enum ArtifactBuildError {
    #[error(transparent)]
    EdgeDerivation(#[from] EdgeDerivationError),
}

#[derive(Debug, Error)]
pub enum ArtifactReadError {
    #[error("invalid profile graph artifact JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported profile graph format version {actual}; supported version is {supported}")]
    UnsupportedFormatVersion { actual: u16, supported: u16 },
}
