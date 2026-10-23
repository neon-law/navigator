//! Discover the versioned Clause library shipped with this binary or stored
//! in the configured PostgreSQL read model.

use std::process::ExitCode;

use store::postgres::{PostgresDb, PostgresError};

struct ListedClause {
    code: String,
    version: i32,
    kind: String,
    is_current: bool,
}

pub fn list() -> ExitCode {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())
        .and_then(|runtime| runtime.block_on(list_sources()))
    {
        Ok(clauses) => {
            for clause in clauses {
                println!(
                    "{}@{} — {} — {}",
                    clause.code,
                    clause.version,
                    clause.kind,
                    if clause.is_current {
                        "current"
                    } else {
                        "not current"
                    }
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator: {error}");
            ExitCode::from(1)
        }
    }
}

pub fn show(source: &str) -> ExitCode {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())
        .and_then(|runtime| runtime.block_on(show_source(source)))
    {
        Ok(yaml) => {
            print!("{yaml}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator: {error}");
            ExitCode::from(1)
        }
    }
}

async fn list_sources() -> Result<Vec<ListedClause>, String> {
    store::clauses::catalog()
        .await
        .map(|clauses| {
            clauses
                .into_iter()
                .map(|clause| ListedClause {
                    code: clause.code,
                    version: clause.version,
                    kind: clause.kind,
                    is_current: clause.is_current,
                })
                .collect()
        })
        .map_err(|error| error.to_string())
}

async fn show_source(source: &str) -> Result<String, String> {
    if std::env::var_os("NAVIGATOR_POSTGRES_HOST").is_some() {
        match PostgresDb::from_env().await {
            Ok(db) => {
                let (code, version) = parse_source(source)?;
                let clause = store::clauses::get(&db, code, version)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("unknown Clause version `{source}`"))?;
                return serde_yaml::to_string(&serde_json::json!({
                    "kind": clause.kind,
                    "inputs": clause.inputs,
                    "body": clause.body,
                }))
                .map_err(|error| error.to_string());
            }
            Err(PostgresError::Connection) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    rules::clauses::show(source)
}

fn parse_source(source: &str) -> Result<(&str, i32), String> {
    let (code, version) = source
        .rsplit_once('@')
        .and_then(|(code, version)| Some((code, version.parse::<i32>().ok()?)))
        .filter(|(code, version)| !code.is_empty() && *version > 0)
        .ok_or_else(|| format!("invalid Clause source `{source}`; use code@version"))?;
    Ok((code, version))
}
