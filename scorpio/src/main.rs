//! `scorpio`: the Ephemeris CLI. Acquire authorized raw DE440 sources,
//! generate reference tables (Parquet, CSV, `DuckDB`, Iceberg), and promote
//! verified bundles. Kept apart from `navigator` so the Arrow, Parquet, and
//! Iceberg stack never ships in the Projects CLI.

use std::process::ExitCode;

use clap::Parser;

mod ephemeris_download;
mod ephemeris_duckdb;
mod ephemeris_iceberg;
mod ephemeris_release;
mod ephemeris_tables;

use cli::{atomic_directory, remote};

#[derive(Parser)]
#[command(
    name = "scorpio",
    version = cli::cli_version(),
    about = "Scorpio, the Navigator Ephemeris CLI.",
    long_about = "Scorpio, the Navigator Ephemeris CLI."
)]
struct Scorpio {
    #[command(subcommand)]
    action: ephemeris_download::EphemerisCmd,
}

fn main() -> ExitCode {
    let scorpio = Scorpio::parse();
    tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(ephemeris_download::run(scorpio.action))
}
