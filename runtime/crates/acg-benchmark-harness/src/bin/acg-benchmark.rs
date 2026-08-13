use std::{env, fs, path::PathBuf, process::ExitCode};

use acg_benchmark_harness::BenchmarkHarness;
use acg_evaluation::ExperimentManifest;

fn main() -> ExitCode {
    match run() {
        Ok(accepted) => {
            if accepted {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(message) => {
            eprintln!("acg-benchmark: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if !(3..=4).contains(&args.len()) {
        return Err(
            "usage: acg-benchmark <manifest.json> <records.jsonl> <acceptance.json> [repo-root]"
                .to_owned(),
        );
    }
    let manifest =
        ExperimentManifest::from_json(&fs::read(&args[0]).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let repo_root = args
        .get(3)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let harness = BenchmarkHarness::with_builtin_workloads(repo_root);
    let outcome = harness
        .run_manifest_to_files(&manifest, &args[1], &args[2])
        .map_err(|error| error.to_string())?;
    let report = outcome
        .acceptance
        .to_pretty_json()
        .map_err(|error| error.to_string())?;
    println!("{}", String::from_utf8_lossy(&report));
    Ok(outcome.acceptance.accepted())
}
