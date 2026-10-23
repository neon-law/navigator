use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use store::postgres_schema::{
    compare_to_surreal, parse_schema, render_erd, render_parity_manifest,
    render_surreal_source_erd, seed_schema_differences,
};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("postgres-schema: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<ExitCode> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "check".to_string());
    let root = PathBuf::from(arguments.next().unwrap_or_else(|| ".".to_string()));
    let schema = store::postgres_schema::canonical_schema();

    match command.as_str() {
        "schema" => print!("{schema}"),
        "erd" => {
            let output = render_erd()?;
            let path = root.join("store/postgres/postgres-erd.mmd");
            fs::write(&path, output)?;
            println!("wrote {}", path.display());
        }
        "source-erd" => {
            let output = render_surreal_source_erd();
            let path = root.join("store/postgres/surreal-source-erd.mmd");
            fs::write(&path, output)?;
            println!("wrote {}", path.display());
        }
        "manifest" => {
            let output = render_parity_manifest()?;
            let path = root.join("store/postgres/parity-manifest.json");
            fs::write(&path, output)?;
            println!("wrote {}", path.display());
        }
        "check" => {
            let postgres = parse_schema(schema)?;
            let table_names = store::schema::table_names();
            let mut differences = compare_to_surreal(
                &postgres,
                &table_names,
                store::schema::table_columns,
            );
            let seeds = read_seed_files(&root.join("store/seeds"))?;
            let seed_refs = seeds
                .iter()
                .map(|(path, yaml)| (path.as_str(), yaml.as_str()))
                .collect::<Vec<_>>();
            let seed_differences = seed_schema_differences(&seed_refs, &postgres)?;
            let schema_is_parity = differences.is_empty();
            let seeds_are_covered = seed_differences.is_empty();
            println!(
                "Surreal tables: {}; PostgreSQL tables: {}; schema differences: {}; seed differences: {}",
                table_names.len(),
                postgres.len(),
                differences.len(),
                seed_differences.len()
            );
            differences.sort_by_key(|difference| {
                (
                    difference.missing_table.clone(),
                    difference.extra_table.clone(),
                    difference.table.clone(),
                    difference.missing_column.clone(),
                    difference.extra_column.clone(),
                )
            });
            for difference in &differences {
                if let Some(table) = &difference.missing_table {
                    println!("missing PostgreSQL table: {table}");
                } else if let Some(table) = &difference.extra_table {
                    println!("extra PostgreSQL table: {table}");
                } else if let Some(table) = &difference.table {
                    if let Some(column) = &difference.missing_column {
                        println!("missing PostgreSQL column: {table}.{column}");
                    }
                    if let Some(column) = &difference.extra_column {
                        println!("extra PostgreSQL column: {table}.{column}");
                    }
                }
            }
            let pending_seeds = seed_differences
                .iter()
                .filter_map(|difference| difference.split_once(':').map(|(path, _)| path))
                .collect::<std::collections::BTreeSet<_>>();
            for (path, _) in &seeds {
                let status = if pending_seeds.contains(path.as_str()) {
                    "pending target coverage"
                } else {
                    "target schema-covered"
                };
                println!("seed: {path}: {status}");
            }
            for difference in seed_differences {
                println!("seed coverage: {difference}");
            }
            if table_names.len() == postgres.len() && schema_is_parity && seeds_are_covered {
                return Ok(ExitCode::SUCCESS);
            }
            return Ok(ExitCode::from(1));
        }
        _ => anyhow::bail!("usage: cargo run -p store --example postgres_schema -- [check|erd|source-erd|manifest|schema] [repository-root]"),
    }
    Ok(ExitCode::SUCCESS)
}

fn read_seed_files(directory: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let mut paths = Vec::new();
    collect_yaml_paths(directory, directory, &mut paths)?;
    paths.sort();
    paths
        .into_iter()
        .map(|(relative, path)| Ok((relative, fs::read_to_string(path)?)))
        .collect()
}

fn collect_yaml_paths(
    root: &Path,
    directory: &Path,
    paths: &mut Vec<(String, PathBuf)>,
) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_yaml_paths(root, &path, paths)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "yaml")
        {
            paths.push((
                path.strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/"),
                path,
            ));
        }
    }
    Ok(())
}
