//! CodeQL's `actions/unpinned-tag` query is filtered out in
//! `.github/codeql/codeql-config.yml` because the release flow pins this
//! repository's own actions to a tag that does not exist until `deploy.yml`
//! creates it. This test is the narrower rule that replaces it: every other
//! third-party action must be pinned to a full commit SHA.

use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli lives one level below the repository root")
        .to_path_buf()
}

fn workflow_files() -> Vec<PathBuf> {
    let github = repository_root().join(".github");
    let mut files = Vec::new();
    for dir in ["workflows"] {
        for entry in std::fs::read_dir(github.join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "yml") {
                files.push(path);
            }
        }
    }
    for entry in std::fs::read_dir(github.join("actions")).unwrap() {
        let action = entry.unwrap().path().join("action.yml");
        if action.exists() {
            files.push(action);
        }
    }
    files
}

/// The `uses:` value of a step line, or `None` for comments and other lines.
fn uses_value(line: &str) -> Option<&str> {
    let line = line.trim_start().trim_start_matches("- ").trim_start();
    let value = line.strip_prefix("uses:")?.trim();
    Some(value.split_whitespace().next().unwrap_or(""))
}

fn is_exempt(action: &str) -> bool {
    action.starts_with("./")
        // GitHub-owned actions and this repository's own actions, which the
        // release cut pins to its release tag.
        || action.starts_with("actions/")
        || action.starts_with("github/")
        || action.starts_with("neon-law/navigator/")
}

#[test]
fn every_third_party_action_is_pinned_to_a_commit_sha() {
    let mut unpinned = Vec::new();
    for file in workflow_files() {
        for (number, line) in std::fs::read_to_string(&file).unwrap().lines().enumerate() {
            let Some(action) = uses_value(line) else {
                continue;
            };
            if is_exempt(action) {
                continue;
            }
            let pinned = action.split_once('@').is_some_and(|(_, reference)| {
                reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit())
            });
            if !pinned {
                unpinned.push(format!("{}:{}: {action}", file.display(), number + 1));
            }
        }
    }
    assert!(
        unpinned.is_empty(),
        "pin to a commit SHA:\n{}",
        unpinned.join("\n")
    );
}

#[test]
fn codeql_filters_only_the_self_reference_tag_query() {
    let config =
        std::fs::read_to_string(repository_root().join(".github/codeql/codeql-config.yml"))
            .expect("the CodeQL config exists");
    let parsed: serde_yaml::Value = serde_yaml::from_str(&config).unwrap();
    let filters = parsed["query-filters"]
        .as_sequence()
        .expect("query-filters");
    assert_eq!(filters.len(), 1, "one filter, no more");
    assert_eq!(
        filters[0]["exclude"]["id"].as_str(),
        Some("actions/unpinned-tag")
    );
    let workflow =
        std::fs::read_to_string(repository_root().join(".github/workflows/codeql.yml")).unwrap();
    assert!(
        workflow.contains("config-file: ./.github/codeql/codeql-config.yml"),
        "codeql.yml must load the config"
    );
}
