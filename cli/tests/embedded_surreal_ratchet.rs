//! The embedded SurrealDB test harness is deprecated; its call sites only go
//! down.
//!
//! `store::surreal::test_support::mem` (and its wrappers `mem_surreal` and
//! `features::shared_surreal`) starts a SurrealDB engine inside the test
//! process. It costs about a second per test and is most of the workspace
//! suite's time, and the embedded SurrealDB 4 engine intermittently hangs. New
//! tests use the PostgreSQL fixture (`NAVIGATOR_TEST_POSTGRES_PORT`) instead;
//! ENG-1356 moves the rest crate by crate.
//!
//! [`BASELINE`] holds each crate's call-site count. A count above its baseline
//! fails: write the test against PostgreSQL. A count below it also fails, so
//! the port that removed the call sites lowers the number in the same change
//! and the ground it gained cannot be given back.
#![allow(clippy::doc_markdown)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Embedded-engine call sites per workspace crate.
const BASELINE: &[(&str, usize)] = &[
    ("archives", 0),
    ("billing-workflows", 16),
    ("cli", 2),
    ("deploy", 19),
    ("features", 96),
    ("import", 5),
    ("mcp", 0),
    ("neon", 3),
    ("portal", 312),
    ("server", 153),
    ("store", 924),
    ("webapp", 28),
    ("workflows", 14),
    ("workflows-service", 75),
];

/// Calls that name the embedded engine wherever they appear.
const QUALIFIED_CALLS: &[&str] = &["mem_surreal(", "shared_surreal(", "test_support::mem("];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root is cli/'s parent")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.')
                && !matches!(name.as_ref(), "target" | "vendor" | "node_modules")
            {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// Whether the file imports `mem` itself, so a bare `mem(` is the engine.
fn imports_bare_mem(source: &str) -> bool {
    source.lines().any(|line| {
        let line = line.trim();
        line.starts_with("use ")
            && line.contains("surreal::test_support::")
            && (line.contains("test_support::mem;")
                || line
                    .split(['{', '}', ',', ' ', ';'])
                    .any(|part| part == "mem"))
    })
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Embedded-engine call sites in one file's source.
fn call_sites(source: &str) -> usize {
    let bare_mem = imports_bare_mem(source);
    source
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//") && !line.contains("fn "))
        .map(|line| {
            let qualified: usize = QUALIFIED_CALLS
                .iter()
                .map(|call| line.matches(call).count())
                .sum();
            let bare = if bare_mem {
                line.match_indices("mem(")
                    .filter(|(at, _)| {
                        *at == 0 || {
                            let before = line.as_bytes()[at - 1];
                            !is_ident(before) && before != b':' && before != b'.'
                        }
                    })
                    .count()
            } else {
                0
            };
            qualified + bare
        })
        .sum()
}

fn counts() -> BTreeMap<String, usize> {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut counts = BTreeMap::new();
    for path in files {
        let relative = path.strip_prefix(&root).expect("file under the repo root");
        if relative == Path::new("store/src/surreal/test_support.rs")
            || relative == Path::new("cli/tests/embedded_surreal_ratchet.rs")
            || relative == Path::new("deploy/src/surreal_free_tests.rs")
        {
            continue;
        }
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", relative.display()));
        let sites = call_sites(&source);
        if sites > 0 {
            let krate = relative
                .components()
                .next()
                .expect("a crate directory")
                .as_os_str()
                .to_string_lossy()
                .into_owned();
            *counts.entry(krate).or_default() += sites;
        }
    }
    counts
}

#[test]
fn embedded_surreal_call_sites_match_the_baseline() {
    let actual = counts();
    let baseline: BTreeMap<String, usize> = BASELINE
        .iter()
        .map(|(krate, count)| ((*krate).to_string(), *count))
        .collect();
    let mut drift = Vec::new();
    for krate in actual.keys().chain(baseline.keys()) {
        let (now, allowed) = (
            actual.get(krate).copied().unwrap_or(0),
            baseline.get(krate).copied().unwrap_or(0),
        );
        if now > allowed {
            drift.push(format!(
                "{krate}: {now} embedded SurrealDB call sites, baseline {allowed}. \
                 Write the new test against the PostgreSQL fixture."
            ));
        } else if now < allowed {
            drift.push(format!(
                "{krate}: {now} embedded SurrealDB call sites, baseline {allowed}. \
                 Lower its BASELINE entry to {now}."
            ));
        }
    }
    drift.dedup();
    let table: Vec<String> = actual
        .iter()
        .map(|(krate, count)| format!("    (\"{krate}\", {count}),"))
        .collect();
    assert!(
        drift.is_empty(),
        "{}\n\nCurrent counts for cli/tests/embedded_surreal_ratchet.rs BASELINE:\n{}",
        drift.join("\n"),
        table.join("\n")
    );
}

#[test]
fn call_sites_count_qualified_and_imported_calls_only() {
    let qualified = "let db = store::surreal::test_support::mem().await;\n\
                     let other = mem_surreal().await;\n\
                     // let commented = mem_surreal().await;\n\
                     async fn mem_surreal() {}\n";
    assert_eq!(call_sites(qualified), 2);

    let imported = "use store::surreal::test_support::{mem, unmigrated};\n\
                    let db = mem().await;\n\
                    let x = self.mem(); let y = remem(); let z = cache::mem();\n";
    assert_eq!(call_sites(imported), 1);

    let not_imported = "let db = mem().await;\n";
    assert_eq!(call_sites(not_imported), 0);
}
