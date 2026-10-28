//! `navigator-migrate`: the operator's PostgreSQL migration entrypoint.
//!
//! Applies the checked-in `store/postgres/migrations` compiled into this
//! binary through `SQLx`'s transactional, advisory-locked runner on a dedicated
//! connection, then exits. It reads the same discrete `NAVIGATOR_POSTGRES_*`
//! values as the application and runs before any service accepts requests;
//! no web process applies DDL. A complete run then seeds the `clause` read
//! model from the Clause library compiled into the same release. See
//! `docs/postgres-tooling.md`.
//!
//! ```text
//! navigator-migrate                   apply every pending migration, then seed Clauses
//! navigator-migrate --through <V>     apply pending migrations up to version V
//! navigator-migrate --dir <PATH>      apply the migrations in PATH instead
//! ```

use std::process::ExitCode;

use store::postgres::PostgresDb;

const USAGE: &str = "usage: navigator-migrate [--through <version>] [--dir <path>]";

#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    through: Option<i64>,
    dir: Option<std::path::PathBuf>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| USAGE.to_string())?;
        match flag.as_str() {
            "--through" if options.through.is_none() => {
                options.through = Some(value.parse().map_err(|_| USAGE.to_string())?);
            }
            "--dir" if options.dir.is_none() => options.dir = Some(value.into()),
            _ => return Err(USAGE.to_string()),
        }
    }
    if options.through.is_some() && options.dir.is_some() {
        return Err(USAGE.to_string());
    }
    Ok(options)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    match run(options).await {
        Ok(applied) => {
            println!(
                "PostgreSQL migrations are current through {}",
                applied.last().map_or_else(|| "none".into(), i64::to_string)
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator-migrate: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(options: Options) -> Result<Vec<i64>, store::clauses::ClauseError> {
    let db = PostgresDb::from_env().await?;
    match &options.dir {
        Some(dir) => db.migrate_directory(dir).await?,
        None => db.migrate_embedded(options.through).await?,
    }
    // A complete run of this release's migrations also refreshes the Clause
    // read model from the library compiled into the same release.
    if options == Options::default() {
        if let Some(report) = store::clauses::seed_library_from_env().await? {
            println!(
                "Clause library seeded: {} versions added, {} current versions moved, {} versions retired",
                report.inserted, report.current_changed, report.retired
            );
        }
    }
    Ok(db.applied_migrations().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> impl Iterator<Item = String> {
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn parses_the_operator_flags_and_refuses_anything_else() {
        assert_eq!(parse(args(&[])), Ok(Options::default()));
        assert_eq!(
            parse(args(&["--through", "20261011200000"])),
            Ok(Options {
                through: Some(20_261_011_200_000),
                dir: None
            })
        );
        assert_eq!(
            parse(args(&["--dir", "store/postgres/migrations"])),
            Ok(Options {
                through: None,
                dir: Some("store/postgres/migrations".into())
            })
        );
        for refused in [
            &["--through"][..],
            &["--through", "latest"],
            &["--force", "1"],
            &["--dir", "a", "--dir", "b"],
            &["--dir", "a", "--through", "1"],
        ] {
            assert_eq!(parse(args(refused)), Err(USAGE.to_string()), "{refused:?}");
        }
    }
}
