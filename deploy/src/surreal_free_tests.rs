//! `hudson ops surreal-free-tests` — select the workspace tests that start no
//! `SurrealDB` engine, for the PR gate.
//!
//! `SurrealDB` is being retired (ENG-1356..1358). The application may still talk
//! to it, but CI no longer tests it: the embedded engine costs about a second
//! per test, was ~96% of the suite's run time, and made the gate cost ~6x more
//! under `SurrealDB` 4. Every test defined in a file that starts an engine is
//! left out of the run. The coverage filter omits files naming the Source engine,
//! except integration tests that run the native PostgreSQL fixture. Those tests
//! can pass a disconnected Source handle through the application API.
//!
//! Writes three files into `--out`:
//!
//! - `nextest-filter` — a nextest filterset excluding those tests (`-E`)
//! - `coverage-ignore` — a regex for `cargo llvm-cov --ignore-filename-regex`
//! - `features-args` — the `cargo test -p features` targets that reach no engine
//!
//! `store::surreal::test_support` refuses to start an engine under
//! `NAVIGATOR_FORBID_SURREAL=1`, so a test this selection misses fails loudly
//! instead of quietly bringing `SurrealDB` back into CI.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};

/// Engine-starting helpers, a direct `mem://` connection, and the verified
/// Source-export fixture. The export fixture uses an engine created by another
/// helper, so its callers still require the compatibility lane. A bare `mem()`
/// is matched separately: `self.mem()` and `cache::mem()` are not the engine.
const ENGINE_CALLS: &[&str] = &[
    "mem_surreal(",
    "shared_surreal(",
    "embedded_surreal(",
    "server_surreal(",
    "unmigrated(",
    "test_support::mem(",
    "verified_family_capture(",
    "\"mem://\"",
];

const SKIP_DIRS: &[&str] = &["target", "vendor", "node_modules"];

pub fn run(out: &Path) -> ExitCode {
    match write(out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hudson: surreal-free-tests: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn write(out: &Path) -> Result<()> {
    let root = std::env::current_dir().context("read the working directory")?;
    let selection = select(&root)?;
    fs::create_dir_all(out).with_context(|| format!("create {}", out.display()))?;
    fs::write(out.join("nextest-filter"), &selection.nextest_filter)?;
    fs::write(out.join("coverage-ignore"), &selection.coverage_ignore)?;
    fs::write(
        out.join("features-args"),
        selection.features_args.join("\n"),
    )?;
    println!(
        "{} files that name `SurrealDB` left out of the coverage measurement",
        selection.ignored_files
    );
    println!(
        "cargo test -p features {}",
        selection.features_args.join(" ")
    );
    Ok(())
}

struct Selection {
    nextest_filter: String,
    coverage_ignore: String,
    features_args: Vec<String>,
    ignored_files: usize,
}

fn code_lines(source: &str) -> impl Iterator<Item = &str> {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
}

fn names_surreal(source: &str) -> bool {
    code_lines(source).any(|line| line.to_ascii_lowercase().contains("surreal"))
}

fn starts_engine(source: &str) -> bool {
    code_lines(source).any(|line| {
        ENGINE_CALLS.iter().any(|call| line.contains(call))
            || line.match_indices("mem()").any(|(at, _)| {
                at == 0 || {
                    let before = line.as_bytes()[at - 1];
                    !(before.is_ascii_alphanumeric() || matches!(before, b'_' | b'.' | b':'))
                }
            })
    })
}

/// `a/b.rs` or `a/b/mod.rs` → `a::b`; `lib.rs` or `main.rs` → the empty path.
fn module_path(src_relative: &Path) -> String {
    let mut parts: Vec<String> = src_relative
        .with_extension("")
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.last().is_some_and(|last| last == "mod") {
        parts.pop();
    }
    if parts == ["lib"] || parts == ["main"] {
        return String::new();
    }
    parts.join("::")
}

/// Every `#[path = "file"] mod name;` in `source`, as `(file, name)`.
fn path_attributes(source: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut pending: Option<String> = None;
    for line in source.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("#[path") {
            pending = rest.split('"').nth(1).map(ToString::to_string);
            continue;
        }
        if line.starts_with("#[") {
            continue;
        }
        if let Some(file) = pending.take() {
            let declaration = line
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ");
            if let Some(name) = declaration
                .strip_prefix("mod ")
                .and_then(|rest| rest.strip_suffix(';'))
            {
                found.push((file, name.trim().to_string()));
            }
        }
    }
    found
}

/// Files mounted with `#[path]` → their real module path.
fn path_attribute_modules(src: &Path) -> BTreeMap<PathBuf, String> {
    let mut mounted = BTreeMap::new();
    for parent in rust_files(src) {
        let Ok(source) = fs::read_to_string(&parent) else {
            continue;
        };
        for (target, name) in path_attributes(&source) {
            let relative = parent.strip_prefix(src).unwrap_or(&parent);
            let parent_module = module_path(relative);
            let directory = parent.parent().unwrap_or(src);
            let nested = directory.join(parent.file_stem().unwrap_or_default());
            let is_root = matches!(
                parent.file_name().and_then(|n| n.to_str()),
                Some("mod.rs" | "lib.rs" | "main.rs")
            );
            let base = if !is_root && nested.is_dir() {
                nested
            } else {
                directory.to_path_buf()
            };
            let resolved = [base.join(&target), directory.join(&target)]
                .into_iter()
                .find(|path| path.exists())
                .and_then(|path| path.canonicalize().ok());
            if let Some(resolved) = resolved {
                let module = if parent_module.is_empty() {
                    name
                } else {
                    format!("{parent_module}::{name}")
                };
                mounted.insert(resolved, module);
            }
        }
    }
    mounted
}

/// Integration-test source file → nextest binary name.
fn test_targets(manifest: &toml::Value, crate_dir: &Path) -> BTreeMap<PathBuf, String> {
    let mut targets = BTreeMap::new();
    for test in manifest
        .get("test")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = test.get("name").and_then(toml::Value::as_str) else {
            continue;
        };
        let path = test
            .get("path")
            .and_then(toml::Value::as_str)
            .map_or_else(|| format!("tests/{name}.rs"), ToString::to_string);
        if let Ok(path) = crate_dir.join(path).canonicalize() {
            targets.insert(path, name.to_string());
        }
    }
    let autotests = manifest
        .get("package")
        .and_then(|package| package.get("autotests"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(true);
    let tests = crate_dir.join("tests");
    if autotests {
        for entry in fs::read_dir(&tests).into_iter().flatten().flatten() {
            let path = entry.path();
            let candidate = if path.is_dir() {
                path.join("main.rs")
            } else {
                path.clone()
            };
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            if candidate.extension().is_some_and(|ext| ext == "rs") && candidate.is_file() {
                if let Ok(canonical) = candidate.canonicalize() {
                    targets
                        .entry(canonical)
                        .or_insert_with(|| name.into_owned());
                }
            }
        }
    }
    targets
}

/// Whether a test binary rooted at `root` reaches an engine through the
/// modules it declares (`mod x;` or `#[path = "..."] mod x;`), at any depth.
fn binary_starts_engine(root: &Path) -> bool {
    // This test names the engine helpers to count them; it never starts one.
    if root.ends_with("cli/tests/embedded_surreal_ratchet.rs") {
        return false;
    }
    let mut seen = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(file) = stack.pop() {
        if !seen.insert(file.clone()) {
            continue;
        }
        let source = fs::read_to_string(&file).unwrap_or_default();
        if starts_engine(&source) {
            return true;
        }
        let directory = file.parent().unwrap_or(Path::new("."));
        let is_root = file == root
            || matches!(
                file.file_name().and_then(|n| n.to_str()),
                Some("mod.rs" | "main.rs" | "lib.rs")
            );
        let nested = if is_root {
            directory.to_path_buf()
        } else {
            directory.join(file.file_stem().unwrap_or_default())
        };
        let mounted: BTreeSet<String> = path_attributes(&source)
            .into_iter()
            .map(|(target, _)| target)
            .collect();
        for (target, _) in path_attributes(&source) {
            stack.push(directory.join(target));
        }
        for line in code_lines(&source).map(str::trim) {
            let declaration = line
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ");
            if let Some(name) = declaration
                .strip_prefix("mod ")
                .and_then(|rest| rest.strip_suffix(';'))
            {
                let name = name.trim();
                if mounted
                    .iter()
                    .any(|target| target.ends_with(&format!("{name}.rs")))
                {
                    continue;
                }
                for candidate in [
                    nested.join(format!("{name}.rs")),
                    nested.join(name).join("mod.rs"),
                ] {
                    if candidate.is_file() {
                        stack.push(candidate);
                    }
                }
            }
        }
    }
    false
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_ref()) {
                    stack.push(path);
                }
            } else if name.ends_with(".rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[allow(clippy::too_many_lines)] // one pass per crate; splitting it scatters the cases
fn select(root: &Path) -> Result<Selection> {
    let workspace: toml::Value = toml::from_str(
        &fs::read_to_string(root.join("Cargo.toml")).context("read the workspace manifest")?,
    )
    .context("parse the workspace manifest")?;
    let members = workspace
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .context("the workspace manifest lists no members")?;
    let mut exclusions = Vec::new();
    let mut ignored = Vec::new();
    let mut features = Vec::new();
    for member in members.iter().filter_map(toml::Value::as_str) {
        let crate_dir = root.join(member);
        let manifest: toml::Value = toml::from_str(
            &fs::read_to_string(crate_dir.join("Cargo.toml"))
                .with_context(|| format!("read {member}/Cargo.toml"))?,
        )
        .with_context(|| format!("parse {member}/Cargo.toml"))?;
        let package = manifest
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
            .with_context(|| format!("{member}/Cargo.toml names no package"))?
            .to_string();
        let targets = test_targets(&manifest, &crate_dir);
        let mounted = path_attribute_modules(&crate_dir.join("src"));
        let mut surreal_targets = BTreeSet::new();
        for path in rust_files(&crate_dir) {
            // These scanners name engine helpers without starting an engine.
            if path.ends_with("deploy/src/surreal_free_tests.rs")
                || path.ends_with("cli/tests/embedded_surreal_ratchet.rs")
            {
                continue;
            }
            let source = fs::read_to_string(&path).unwrap_or_default();
            if !names_surreal(&source) {
                continue;
            }
            let relative_to_root = path.strip_prefix(root).unwrap_or(&path);
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            let native_test = targets.contains_key(&canonical)
                && code_lines(&source).any(|line| line.contains("with_required_postgres("))
                && !binary_starts_engine(&canonical);
            if !native_test {
                ignored.push(relative_to_root.to_string_lossy().replace('.', "\\."));
            }
            if package != "features" && !starts_engine(&source) {
                continue;
            }
            let relative = path.strip_prefix(&crate_dir).unwrap_or(&path);
            let mut components = relative.components();
            let top = components
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned());
            match top.as_deref() {
                Some("src") => {
                    if package == "features" {
                        continue;
                    }
                    let module = mounted
                        .get(&canonical)
                        .cloned()
                        .unwrap_or_else(|| module_path(components.as_path()));
                    let prefix = if module.is_empty() {
                        "tests::".to_string()
                    } else {
                        format!("{module}::")
                    };
                    exclusions.push(format!(
                        "(package({package}) & not kind(test) & test(/^{prefix}/))"
                    ));
                }
                _ => {
                    if let Some(name) = targets.get(&canonical) {
                        surreal_targets.insert(name.clone());
                    } else if top.as_deref() == Some("tests") && relative.components().count() > 2 {
                        // A helper module under tests/<dir>/: the binary that
                        // owns the directory, when there is one.
                        let owner = relative
                            .components()
                            .nth(1)
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if targets.values().any(|name| *name == owner) {
                            surreal_targets.insert(owner);
                        }
                    }
                }
            }
        }
        for (path, name) in &targets {
            if binary_starts_engine(path) {
                surreal_targets.insert(name.clone());
            }
        }
        if package == "features" {
            let engine_modules: Vec<String> = rust_files(&crate_dir.join("src"))
                .into_iter()
                .filter(|path| path.file_stem().is_some_and(|stem| stem != "lib"))
                .filter(|path| starts_engine(&fs::read_to_string(path).unwrap_or_default()))
                .filter_map(|path| Some(path.file_stem()?.to_string_lossy().into_owned()))
                .collect();
            for (path, name) in &targets {
                let runner = fs::read_to_string(path).unwrap_or_default();
                // `features::app_state` builds its world on `shared_surreal()`.
                if runner.contains("app_state")
                    || engine_modules
                        .iter()
                        .any(|module| runner.contains(&format!("features::{module}::")))
                {
                    surreal_targets.insert(name.clone());
                }
            }
            let mut clean: Vec<&String> = targets
                .values()
                .filter(|name| !surreal_targets.contains(*name))
                .collect();
            clean.sort();
            clean.dedup();
            features = std::iter::once("--lib".to_string())
                .chain(clean.into_iter().map(|name| format!("--test={name}")))
                .collect();
        } else {
            for name in surreal_targets {
                exclusions.push(format!(
                    "(package({package}) & kind(test) & binary(={name}))"
                ));
            }
        }
    }
    Ok(Selection {
        nextest_filter: if exclusions.is_empty() {
            "all()".to_string()
        } else {
            format!("not ({})", exclusions.join(" | "))
        },
        coverage_ignore: format!("({})$", ignored.join("|")),
        ignored_files: ignored.len(),
        features_args: features,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_paths_follow_the_file_layout() {
        assert_eq!(module_path(Path::new("lib.rs")), "");
        assert_eq!(module_path(Path::new("a/b.rs")), "a::b");
        assert_eq!(module_path(Path::new("a/b/mod.rs")), "a::b");
    }

    #[test]
    fn engine_detection_counts_the_helpers_not_lookalikes() {
        assert!(starts_engine(
            "let db = store::surreal::test_support::mem().await;"
        ));
        assert!(starts_engine("let db = mem().await;"));
        assert!(!starts_engine(
            "fn f(_db: &store::surreal::SurrealDb) {}\nlet m = self.mem();"
        ));
        assert!(!starts_engine("// let db = mem_surreal().await;"));
    }

    #[test]
    fn captured_exports_require_source_but_native_receipts_do_not() {
        for call in [
            "verified_family_capture(&source).await",
            "crate::postgres_mirror::tests::verified_family_capture(&source).await",
            // A module alias preserves the called function's existing name.
            "capture::verified_family_capture(&source).await",
        ] {
            assert!(starts_engine(call), "actual Source export: {call}");
        }
        assert!(!starts_engine("receipt.verify_export(&bytes)"));
        assert!(!starts_engine("PostgresFixture::create(options).await"));
        assert!(!starts_engine(
            "use crate::postgres_mirror::tests::verified_family_capture;"
        ));
    }

    #[test]
    fn captured_application_modules_preserve_native_postgres_siblings() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        for module in [
            "postgres_backfill::application_family_tests",
            "postgres_backfill::application_native_boundary_tests",
        ] {
            assert!(
                selection.nextest_filter.contains(&format!(
                    "(package(store) & not kind(test) & test(/^{module}::/))"
                )),
                "captured nonempty Source history must stay in the compatibility lane: {module}"
            );
        }
        for native in [
            "postgres_backfill",
            "postgres_backfill::family::tests",
            "postgres_backfill::family::journal_component_tests",
            "postgres::application_authority_tests",
        ] {
            assert!(
                !selection
                    .nextest_filter
                    .contains(&format!("test(/^{native}::/)")),
                "native PostgreSQL boundary must remain selected: {native}"
            );
        }
    }

    #[test]
    fn naming_surreal_ignores_comments() {
        assert!(names_surreal("fn f(_db: &store::surreal::SurrealDb) {}"));
        assert!(!names_surreal("// `SurrealDB` is retired\nlet x = 1;"));
    }

    #[test]
    fn path_attributes_name_the_mounted_module() {
        assert_eq!(
            path_attributes("#[cfg(test)]\n#[path = \"x_tests.rs\"]\nmod tests;"),
            vec![("x_tests.rs".to_string(), "tests".to_string())]
        );
    }

    #[test]
    fn the_workspace_selection_leaves_out_the_engine_tests() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the workspace root");
        let selection = select(root).expect("select over the workspace");
        assert!(selection
            .nextest_filter
            .contains("(package(store) & not kind(test) & test(/^persons::/))"));
        assert!(selection
            .coverage_ignore
            .contains("store/src/surreal/mod\\.rs"));
        assert_eq!(
            selection.features_args.first().map(String::as_str),
            Some("--lib")
        );
        assert!(!selection
            .features_args
            .iter()
            .any(|arg| arg == "--test=client_intake"));
        // Reaches the engine only through `#[path = "common/notation_fixture.rs"]`.
        assert!(selection
            .nextest_filter
            .contains("(package(server) & kind(test) & binary(=api_project_notations))"));
        for (package, binary) in [("server", "twilio_summary"), ("store", "person_phones")] {
            assert!(!selection
                .nextest_filter
                .contains(&format!("binary(={binary})")));
            assert!(!selection
                .coverage_ignore
                .contains(&format!("{package}/tests/{binary}\\.rs")));
        }
        assert!(!selection.nextest_filter.contains("surreal_free_tests::"));
    }
    #[test]
    fn embedded_start_ratchet_runs_in_the_surreal_free_lane() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        assert!(!selection
            .nextest_filter
            .contains("(package(cli) & kind(test) & binary(=embedded_surreal_ratchet))"));
    }

    #[test]
    fn oauth_compatibility_does_not_suppress_native_identity_tests() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        assert!(selection
            .nextest_filter
            .contains("test(/^oauth::legacy_tests::/)"));
        assert!(!selection.nextest_filter.contains("test(/^oauth::/)"));
        assert!(!selection.nextest_filter.contains("test(/^oauth::tests::/)"));
    }

    #[test]
    fn native_email_acknowledgments_are_selected() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        assert!(!selection
            .nextest_filter
            .contains("(package(portal) & not kind(test) & test(/^email_acknowledgment::/))"));
    }

    #[test]
    fn native_credential_tests_are_selected() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        assert!(!selection.nextest_filter.contains("test(/^credentials::/)"));
        assert!(selection
            .nextest_filter
            .contains("test(/^credentials::compatibility_tests::/)"));
    }

    #[test]
    fn native_people_and_clerk_ssr_tests_are_selected() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        for binary in ["dioxus_people_ssr", "dioxus_clerk_ssr"] {
            assert!(!selection.nextest_filter.contains(&format!(
                "(package(server) & kind(test) & binary(={binary}))"
            )));
            assert!(!selection
                .coverage_ignore
                .contains(&format!("server/tests/{binary}\\.rs")));
        }
    }

    #[test]
    fn native_graph_api_tests_are_selected() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        assert!(!selection
            .nextest_filter
            .contains("(package(portal) & kind(test) & binary(=graph_api))"));
        assert!(!selection
            .coverage_ignore
            .contains("portal/tests/graph_api\\.rs"));
    }

    #[test]
    fn inbox_compatibility_does_not_suppress_native_tests() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        assert!(selection
            .nextest_filter
            .contains("(package(store) & not kind(test) & test(/^inbox::legacy_tests::/))"));
        assert!(!selection
            .nextest_filter
            .contains("(package(store) & not kind(test) & test(/^inbox::/))"));
        assert!(!selection
            .nextest_filter
            .contains("(package(store) & not kind(test) & test(/^inbox::tests::/))"));
    }

    #[test]
    fn compatibility_tests_do_not_suppress_mock_http_tiktok_tests() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        assert!(selection
            .nextest_filter
            .contains("test(/^social_oauth::tests::/)"));
        assert!(!selection.nextest_filter.contains("test(/^social_oauth::/)"));
    }

    #[test]
    fn compatibility_tests_do_not_suppress_engine_free_firm_slug_tests() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        assert!(selection.nextest_filter.contains("test(/^firms::tests::/)"));
        assert!(!selection.nextest_filter.contains("test(/^firms::/)"));
    }

    #[test]
    fn engine_orchestration_does_not_suppress_engine_free_scraper_tests() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        assert!(
            selection
                .nextest_filter
                .contains("test(/^scraper::tests::/)"),
            "engine-backed orchestration stays excluded"
        );
        assert!(
            !selection.nextest_filter.contains("test(/^scraper::/)"),
            "engine-free adapter, retry, session, and filing tests remain selected"
        );
    }
    #[test]
    fn project_compatibility_does_not_suppress_native_postgres_journeys() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let selection = select(root).unwrap();
        assert!(selection
            .nextest_filter
            .contains("test(/^postgres::projects_tests::compatibility::/)"));
        assert!(!selection
            .nextest_filter
            .contains("test(/^postgres::projects_tests::/)"));
    }
}
