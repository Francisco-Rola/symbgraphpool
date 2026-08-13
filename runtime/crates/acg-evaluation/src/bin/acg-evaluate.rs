use std::{env, fs, process::ExitCode};

use acg_evaluation::{acceptance::read_records_jsonl, ExperimentManifest};

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
            eprintln!("acg-evaluate: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if !(2..=3).contains(&args.len()) {
        return Err(
            "usage: acg-evaluate <manifest.json> <records.jsonl> [acceptance-report.json]"
                .to_owned(),
        );
    }

    let manifest_bytes = fs::read(&args[0]).map_err(|error| error.to_string())?;
    let manifest =
        ExperimentManifest::from_json(&manifest_bytes).map_err(|error| error.to_string())?;
    let records = read_records_jsonl(&args[1]).map_err(|error| error.to_string())?;
    let report = manifest.evaluate(&records);
    let bytes = report.to_pretty_json().map_err(|error| error.to_string())?;

    if let Some(output) = args.get(2) {
        fs::write(output, &bytes).map_err(|error| error.to_string())?;
    }
    println!("{}", String::from_utf8_lossy(&bytes));
    Ok(report.accepted())
}
