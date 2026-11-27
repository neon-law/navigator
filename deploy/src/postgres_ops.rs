//! PostgreSQL application checks and graph maintenance.

use std::process::ExitCode;

use anyhow::Context;
use clap::Subcommand;
use serde_json::json;
use store::postgres::PostgresDb;

/// `hudson ops postgres`.
#[derive(Debug, Subcommand)]
pub enum PostgresCmd {
    /// Generate or check the compiled native-origin release contract without connecting to any service.
    NativeOriginContract {
        /// Canonical restore artifact destination.
        #[arg(long)]
        out: std::path::PathBuf,
        /// Refuse when the existing artifact differs; write nothing.
        #[arg(long)]
        check: bool,
    },
    /// Verify completed application history using PostgreSQL alone; writes nothing.
    Ready {
        /// Print one content-free JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Verify or rebuild the Apache AGE projection from PostgreSQL.
    #[command(subcommand)]
    Graph(GraphCmd),
}

/// `hudson ops postgres graph`.
#[derive(Debug, Subcommand)]
pub enum GraphCmd {
    /// Compare the graph with its relational sources without writing.
    Parity,
    /// Rebuild the projection using the migration login, then verify it.
    Rebuild,
}

const DRIFT: u8 = 1;
const UNAVAILABLE: u8 = 2;

fn native_origin_contract(out: &std::path::Path, check: bool) -> Result<(), &'static str> {
    let mut bytes = serde_json::to_vec_pretty(&store::postgres::native_origin::release_contract())
        .map_err(|_| "could not encode native-origin release contract")?;
    bytes.push(b'\n');
    if check {
        let actual = std::fs::read(out).map_err(|_| "native-origin release contract is missing")?;
        if actual != bytes {
            return Err("native-origin release contract differs from the current binary");
        }
    } else {
        std::fs::write(out, bytes).map_err(|_| "could not write native-origin release contract")?;
    }
    Ok(())
}

/// Run one `ops postgres` command.
pub fn run(command: PostgresCmd) -> ExitCode {
    match command {
        PostgresCmd::NativeOriginContract { out, check } => {
            match native_origin_contract(&out, check) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("navigator: {error}");
                    ExitCode::from(UNAVAILABLE)
                }
            }
        }
        PostgresCmd::Ready { json } => crate::postgres_admission::run(json),
        PostgresCmd::Graph(command) => run_graph(&command),
    }
}

fn run_graph(command: &GraphCmd) -> ExitCode {
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("start PostgreSQL graph runtime")
        .and_then(|runtime| {
            runtime.block_on(async {
                let pg = PostgresDb::from_env().await?;
                if matches!(command, GraphCmd::Rebuild) {
                    store::postgres_graph::project_graph(&pg).await?;
                }
                Ok(store::postgres_graph::validate_graph(&pg).await?)
            })
        });
    match result {
        Ok(validation) => {
            println!(
                "{}",
                json!({
                    "clean": validation.is_clean(),
                    "vertices": validation.vertices,
                    "source_vertices": validation.source_vertices,
                    "edges": validation.edges,
                    "source_edges": validation.source_edges,
                    "broken_endpoints": validation.broken_endpoints
                })
            );
            if validation.is_clean() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(DRIFT)
            }
        }
        Err(error) => {
            eprintln!("navigator: graph operation failed: {error:#}");
            ExitCode::from(UNAVAILABLE)
        }
    }
}
