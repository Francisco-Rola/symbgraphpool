//! Parsing and normalization for symbolic-analyzer JSON artifacts.

mod normalize;
pub mod raw;

pub use normalize::{normalize_document, IngestionContext, NormalizeError};
pub use raw::{parse_reader, parse_slice, RawAnalyzerDocument, RawParseError};
