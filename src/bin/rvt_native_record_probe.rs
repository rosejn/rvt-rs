//! Bounded physical-frame diagnostic for one current native record.
use anyhow::{Context, Result};
use clap::Parser;
use rvt::native_document::{self, Options};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Parser)]
#[command(
    name = "rvt-native-record-probe",
    about = "Report one validated current native record frame without decoding its graph"
)]
struct Args {
    file: PathBuf,
    #[arg(long)]
    id: u64,
    #[arg(long, default_value_t = 102, value_parser = channel)]
    channel: u64,
    /// New JSON output path; refuses overwrite.
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 512 * 1024 * 1024)]
    max_stream_bytes: u64,
    #[arg(long, default_value_t = 256 * 1024 * 1024)]
    max_group_bytes: usize,
}

fn channel(value: &str) -> Result<u64, String> {
    match value {
        "101" => Ok(101),
        "102" => Ok(102),
        "103" => Ok(103),
        _ => Err("channel must be 101, 102, or 103".into()),
    }
}

fn source_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path).context("open source for hashing")?;
    let mut hash = Sha256::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        hash.update(&chunk[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[derive(Serialize)]
struct Report<'a> {
    format: &'static str,
    source: &'a Path,
    source_sha256: String,
    probe: rvt::native_document::PhysicalRecordProbe,
}

fn run(args: Args) -> Result<()> {
    anyhow::ensure!(args.max_group_bytes > 0, "group byte budget must be positive");
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.output)
        .with_context(|| format!("create new output {}", args.output.display()))?;
    let mut file = rvt::RevitFile::open(&args.file)?;
    let options = Options {
        max_stream_bytes: args.max_stream_bytes,
        max_group_bytes: args.max_group_bytes,
        ..Default::default()
    };
    let index = native_document::build_physical_index(&mut file, &options)?;
    let probe = native_document::probe_current_record(&mut file, &options, &index, args.channel, args.id)?;
    let report = Report {
        format: "rvt-native-record-probe/v1",
        source: &args.file,
        source_sha256: source_hash(&args.file)?,
        probe,
    };
    let mut output = BufWriter::new(output);
    serde_json::to_writer_pretty(&mut output, &report)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rvt-native-record-probe: {error:#}");
            ExitCode::FAILURE
        }
    }
}
