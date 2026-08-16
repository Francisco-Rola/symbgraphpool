//! Helpers for self-identifying Phase 5F experiment records.

use std::{
    fs,
    path::Path,
    process::Command,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use crate::{CorrectnessRecord, ExperimentMetadata};

impl ExperimentMetadata {
    /// Best-effort capture of stable host/build metadata. Missing values remain missing so the
    /// Phase 5F publication gate can reject an incomplete record instead of silently inventing
    /// provenance.
    pub fn capture_standard_environment(mut self, repo_root: impl AsRef<Path>) -> Self {
        let repo_root = repo_root.as_ref();
        if self.started_at_utc.is_none() {
            self.started_at_utc = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|duration| format_unix_timestamp_utc(duration.as_secs()));
        }
        if self.git_revision.is_none() {
            let repo_root_text = repo_root.to_string_lossy().into_owned();
            self.git_revision =
                command_stdout("git", &["-C", repo_root_text.as_str(), "rev-parse", "HEAD"]);
        }
        if self.rustc_version.is_none() {
            self.rustc_version = command_stdout("rustc", &["--version"]);
        }
        if self.build_profile.is_none() {
            self.build_profile = std::env::var("ACG_BUILD_PROFILE")
                .ok()
                .or_else(|| std::env::var("PROFILE").ok())
                .filter(|value| !value.trim().is_empty());
        }

        self.environment
            .entry("os".to_owned())
            .or_insert_with(|| std::env::consts::OS.to_owned());
        self.environment
            .entry("arch".to_owned())
            .or_insert_with(|| std::env::consts::ARCH.to_owned());
        if let Ok(logical) = thread::available_parallelism() {
            self.environment
                .entry("logical_cores".to_owned())
                .or_insert_with(|| logical.get().to_string());
        }
        if let Some(kernel) = command_stdout("uname", &["-r"]) {
            self.environment
                .entry("kernel".to_owned())
                .or_insert(kernel);
        }
        if let Some(model) = linux_cpu_model() {
            self.environment
                .entry("cpu_model".to_owned())
                .or_insert(model);
        }
        if let Some(memory) = linux_memory_total_kib() {
            self.environment
                .entry("memory_total_kib".to_owned())
                .or_insert(memory);
        }
        if let Some(dirty) = git_dirty(repo_root) {
            self.environment
                .entry("git_dirty".to_owned())
                .or_insert_with(|| dirty.to_string());
        }
        self
    }
}

impl CorrectnessRecord {
    /// Build a correctness record from deterministic canonical-state encodings.
    pub fn from_state_bytes(canonical_state: &[u8], serial_reference: &[u8]) -> Self {
        let canonical_state_digest = sha256_hex(canonical_state);
        let serial_reference_digest = sha256_hex(serial_reference);
        Self {
            serial_equivalent: Some(canonical_state_digest == serial_reference_digest),
            canonical_state_digest: Some(canonical_state_digest),
            serial_reference_digest: Some(serial_reference_digest),
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn git_dirty(repo_root: &Path) -> Option<bool> {
    let repo_root_text = repo_root.to_string_lossy().into_owned();
    let output = Command::new("git")
        .args([
            "-C",
            repo_root_text.as_str(),
            "status",
            "--porcelain",
            "--untracked-files=normal",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        None
    } else {
        Some(!output.stdout.is_empty())
    }
}

fn linux_cpu_model() -> Option<String> {
    let contents = fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in contents.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if matches!(key.trim(), "model name" | "Hardware" | "Processor") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn linux_memory_total_kib() -> Option<String> {
    let contents = fs::read_to_string("/proc/meminfo").ok()?;
    let line = contents
        .lines()
        .find(|line| line.starts_with("MemTotal:"))?;
    line.split_whitespace().nth(1).map(str::to_owned)
}

fn format_unix_timestamp_utc(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let seconds_of_day = seconds % 86_400;
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

// Howard Hinnant's civil-from-days algorithm, with day zero = 1970-01-01.
fn civil_from_days(days_since_unix_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_unix_epoch.saturating_add(719_468);
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_timestamp_formatter_handles_epoch_and_known_leap_day() {
        assert_eq!(format_unix_timestamp_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_unix_timestamp_utc(1_582_934_400),
            "2020-02-29T00:00:00Z"
        );
    }
}
