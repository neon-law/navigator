use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use store::postgres_schema::{
    assemble_migrations, parse_schema, render_erd, seed_schema_differences,
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
        "assemble" => assemble_schema(&root)?,
        "schema" => print!("{schema}"),
        "erd" => {
            let output = render_erd()?;
            let path = root.join("store/postgres/postgres-erd.mmd");
            fs::write(&path, output)?;
            println!("wrote {}", path.display());
        }
        "check" => {
            let postgres = parse_schema(schema)?;
            let seeds = read_seed_files(&root.join("store/seeds"))?;
            let seed_refs = seeds
                .iter()
                .map(|(path, yaml)| (path.as_str(), yaml.as_str()))
                .collect::<Vec<_>>();
            let seed_differences = seed_schema_differences(&seed_refs, &postgres)?;
            let seeds_are_covered = seed_differences.is_empty();
            println!("PostgreSQL tables: {}; seed differences: {}", postgres.len(), seed_differences.len());
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
            if seeds_are_covered {
                return Ok(ExitCode::SUCCESS);
            }
            return Ok(ExitCode::from(1));
        }
        _ => anyhow::bail!("usage: cargo run -p store --example postgres_schema -- [assemble|check|erd|schema] [repository-root]"),
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

fn assemble_schema(root: &Path) -> anyhow::Result<()> {
    let mut migrations = Vec::new();
    for entry in fs::read_dir(root.join("store/postgres/migrations"))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "sql") {
            continue;
        }
        let filename = entry.file_name();
        let name = filename
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("migration filename is not UTF-8"))?
            .to_owned();
        migrations.push((name, fs::read_to_string(path)?));
    }
    anyhow::ensure!(!migrations.is_empty(), "no SQL migrations to assemble");
    let borrowed = migrations
        .iter()
        .map(|(name, sql)| (name.as_str(), sql.as_str()))
        .collect::<Vec<_>>();
    let schema = assemble_migrations(&borrowed)?;
    let path = root.join("store/postgres/schema.sql");
    fs::write(&path, schema)?;
    println!("wrote {}", path.display());
    Ok(())
}
