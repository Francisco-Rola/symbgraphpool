use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    io::{BufRead, BufReader},
    path::Path,
    time::{Duration, Instant},
};

use acg_cosmwasm_engine::deterministic_compute;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComputeMetric {
    None,
    Steps,
    GasUsed,
}

impl ComputeMetric {
    pub fn parse(value: &str) -> Result<Self, CalibrationError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Ok(Self::None),
            "steps" | "opcode-steps" => Ok(Self::Steps),
            "gas" | "gas-used" | "gasused" => Ok(Self::GasUsed),
            other => Err(CalibrationError(format!(
                "unknown compute calibration metric {other:?}; expected none, steps, or gas"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Steps => "steps",
            Self::GasUsed => "gas-used",
        }
    }
}

#[derive(Debug)]
pub struct CalibrationError(String);

impl fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl Error for CalibrationError {}

#[derive(Clone, Debug, Deserialize)]
struct ComputeWeightRow {
    block_number: u64,
    tx_index: usize,
    tx_hash: String,
    source_trace_present: bool,
    source_opcode_steps: Option<u64>,
    source_gas_used: Option<u64>,
}

#[derive(Clone, Debug)]
struct TxWeight {
    tx_hash: String,
    units: u64,
    source_trace_present: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ComputeCalibrationMetadata {
    pub metric: &'static str,
    pub scale: f64,
    pub base_total_nanos: u64,
    pub iterations_per_nano: f64,
    pub source_units_total: u128,
    pub weighted_transactions: usize,
    pub missing_source_transactions: usize,
}

/// Deterministic source-cost supplement used only by Vegeta S3 workload-fidelity experiments.
///
/// `scale=1` adds approximately `base_total_nanos` of *single-core* CPU work over the full S3
/// range, distributed across transactions in proportion to frozen source opcode steps or gasUsed.
/// The actual supplement is a deterministic integer iteration count; wall-clock time is used only
/// once at process startup to translate the requested single-core budget into iterations.
#[derive(Clone, Debug)]
pub struct ComputeCalibration {
    metric: ComputeMetric,
    scale: f64,
    base_total_nanos: u64,
    iterations_per_nano: f64,
    total_units: u128,
    weights: BTreeMap<(u64, usize), TxWeight>,
    weighted_transactions: usize,
    missing_source_transactions: usize,
}

impl ComputeCalibration {
    pub fn disabled() -> Self {
        Self {
            metric: ComputeMetric::None,
            scale: 0.0,
            base_total_nanos: 0,
            iterations_per_nano: 0.0,
            total_units: 0,
            weights: BTreeMap::new(),
            weighted_transactions: 0,
            missing_source_transactions: 0,
        }
    }

    pub fn load(
        path: &Path,
        metric: ComputeMetric,
        scale: f64,
        base_total_nanos: u64,
        iterations_per_nano_override: Option<f64>,
    ) -> Result<Self, CalibrationError> {
        if !scale.is_finite() || scale < 0.0 {
            return Err(CalibrationError(
                "compute scale must be finite and non-negative".to_owned(),
            ));
        }
        if metric == ComputeMetric::None || scale == 0.0 {
            return Ok(Self {
                metric,
                scale,
                base_total_nanos,
                ..Self::disabled()
            });
        }
        if base_total_nanos == 0 {
            return Err(CalibrationError(
                "compute base total must be greater than zero when calibration is enabled"
                    .to_owned(),
            ));
        }
        let file = fs::File::open(path).map_err(|error| {
            CalibrationError(format!(
                "failed to open compute weights {}: {error}",
                path.display()
            ))
        })?;
        let mut weights = BTreeMap::new();
        let mut total_units = 0_u128;
        let mut weighted_transactions = 0_usize;
        let mut missing_source_transactions = 0_usize;
        for (line_number, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|error| CalibrationError(error.to_string()))?;
            if line.trim().is_empty() {
                continue;
            }
            let row: ComputeWeightRow = serde_json::from_str(&line).map_err(|error| {
                CalibrationError(format!(
                    "invalid compute weight line {}: {error}",
                    line_number + 1
                ))
            })?;
            let units = match metric {
                ComputeMetric::None => 0,
                ComputeMetric::Steps => row.source_opcode_steps.unwrap_or(0),
                ComputeMetric::GasUsed => row.source_gas_used.unwrap_or(0),
            };
            if !row.source_trace_present {
                missing_source_transactions += 1;
            } else if units > 0 {
                weighted_transactions += 1;
            }
            total_units = total_units.saturating_add(u128::from(units));
            let key = (row.block_number, row.tx_index);
            if weights
                .insert(
                    key,
                    TxWeight {
                        tx_hash: row.tx_hash.to_ascii_lowercase(),
                        units,
                        source_trace_present: row.source_trace_present,
                    },
                )
                .is_some()
            {
                return Err(CalibrationError(format!(
                    "duplicate compute weight for block {} tx {}",
                    row.block_number, row.tx_index
                )));
            }
        }
        if total_units == 0 {
            return Err(CalibrationError(format!(
                "compute weights contain no positive {} units",
                metric.as_str()
            )));
        }
        let iterations_per_nano = match iterations_per_nano_override {
            Some(value) if value.is_finite() && value > 0.0 => value,
            Some(_) => {
                return Err(CalibrationError(
                    "compute iterations-per-nano override must be finite and positive".to_owned(),
                ))
            }
            None => measure_iterations_per_nano(),
        };
        Ok(Self {
            metric,
            scale,
            base_total_nanos,
            iterations_per_nano,
            total_units,
            weights,
            weighted_transactions,
            missing_source_transactions,
        })
    }

    pub fn iterations_for(
        &self,
        block_number: u64,
        tx_index: usize,
        tx_hash: &str,
    ) -> Result<u64, CalibrationError> {
        if self.metric == ComputeMetric::None || self.scale == 0.0 {
            return Ok(0);
        }
        let Some(weight) = self.weights.get(&(block_number, tx_index)) else {
            return Err(CalibrationError(format!(
                "missing compute weight for block {block_number} tx {tx_index}"
            )));
        };
        if !weight.tx_hash.is_empty()
            && !tx_hash.is_empty()
            && weight.tx_hash != tx_hash.to_ascii_lowercase()
        {
            return Err(CalibrationError(format!(
                "compute weight hash mismatch at block {block_number} tx {tx_index}: {} != {}",
                weight.tx_hash, tx_hash
            )));
        }
        if !weight.source_trace_present || weight.units == 0 {
            return Ok(0);
        }
        let share = weight.units as f64 / self.total_units as f64;
        let iterations =
            self.base_total_nanos as f64 * self.scale * share * self.iterations_per_nano;
        if !iterations.is_finite() || iterations <= 0.0 {
            return Ok(0);
        }
        Ok(iterations.round().clamp(1.0, u64::MAX as f64) as u64)
    }

    pub fn metadata(&self) -> ComputeCalibrationMetadata {
        ComputeCalibrationMetadata {
            metric: self.metric.as_str(),
            scale: self.scale,
            base_total_nanos: self.base_total_nanos,
            iterations_per_nano: self.iterations_per_nano,
            source_units_total: self.total_units,
            weighted_transactions: self.weighted_transactions,
            missing_source_transactions: self.missing_source_transactions,
        }
    }

    pub fn metric(&self) -> ComputeMetric {
        self.metric
    }
}

pub fn measure_iterations_per_nano() -> f64 {
    let mut iterations = 50_000_u64;
    loop {
        let started = Instant::now();
        std::hint::black_box(deterministic_compute(iterations, 0xC011_BA5E));
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_millis(12) || iterations >= 100_000_000 {
            let nanos = elapsed.as_nanos().max(1) as f64;
            return iterations as f64 / nanos;
        }
        iterations = iterations.saturating_mul(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn source_units_drive_deterministic_iteration_ratios() {
        let path = std::env::temp_dir().join(format!(
            "acg-compute-weights-{}-{}.jsonl",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"block_number":1,"tx_index":0,"tx_hash":"0xaaa","source_trace_present":true,"source_opcode_steps":10,"source_gas_used":100}}"#).unwrap();
        writeln!(file, r#"{{"block_number":1,"tx_index":1,"tx_hash":"0xbbb","source_trace_present":true,"source_opcode_steps":30,"source_gas_used":200}}"#).unwrap();
        let calibration =
            ComputeCalibration::load(&path, ComputeMetric::Steps, 1.0, 4_000, Some(1.0)).unwrap();
        let a = calibration.iterations_for(1, 0, "0xaaa").unwrap();
        let b = calibration.iterations_for(1, 1, "0xbbb").unwrap();
        assert_eq!(a, 1_000);
        assert_eq!(b, 3_000);
        fs::remove_file(path).ok();
    }
}
