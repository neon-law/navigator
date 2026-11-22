//! Guard `docs/oss-install.md` §0 against the drift that made the page
//! unusable: it is the first thing an outside operator reads, and every
//! prerequisite it omits is a command that fails on a clean machine with no
//! hint about what is missing.
//!
//! Derive required tools from `orchestrate.rs` and check that the install
//! page names them. Source citations must resolve to existing nonempty lines.

use std::fs;
use std::path::PathBuf;

const OSS_INSTALL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../docs/oss-install.md"
));

const ORCHESTRATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../deploy/src/devx/orchestrate.rs"
));

/// The workspace root, which every citation on the page is relative to.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// `## 0. Prerequisites` through the next `## ` heading.
fn prerequisites_section() -> &'static str {
    let (_, section) = OSS_INSTALL
        .split_once("## 0. Prerequisites")
        .expect("oss-install.md must carry a `## 0. Prerequisites` section");
    section
        .split_once("\n## ")
        .map_or(section, |(section, _)| section)
}

/// Every tool name passed to a `require_tools(&[…])` call in `orchestrate.rs`
/// whose list mentions `kind` — i.e. the calls that open the KIND cluster
/// lane, as opposed to the `kubectl`-only helpers that assume it is already up.
fn cluster_lane_tools() -> Vec<String> {
    let mut tools: Vec<String> = Vec::new();
    for (at, needle) in ORCHESTRATE.match_indices("require_tools(&[") {
        let rest = &ORCHESTRATE[at + needle.len()..];
        let list = rest
            .split_once("])")
            .expect("a require_tools call must close its slice literal")
            .0;
        let names: Vec<String> = list
            .split(',')
            .map(|name| name.trim().trim_matches('"').to_string())
            .filter(|name| !name.is_empty())
            .collect();
        if !names.iter().any(|name| name == "kind") {
            continue;
        }
        for name in names {
            if !tools.contains(&name) {
                tools.push(name);
            }
        }
    }
    assert!(
        !tools.is_empty(),
        "orchestrate.rs must open the cluster lane with require_tools"
    );
    tools
}

/// A reader who installs everything §0 lists must be able to start the local
/// cluster. `orchestrate.rs` decides that, so read the answer from there.
#[test]
fn prerequisites_name_every_tool_the_kind_cluster_lane_requires() {
    let section = prerequisites_section();
    for tool in cluster_lane_tools() {
        assert!(
            section.to_lowercase().contains(&tool.to_lowercase()),
            "`docs/oss-install.md` §0 must name `{tool}`: `deploy/src/devx/orchestrate.rs` refuses to \
             start the KIND cluster lane without it, so a reader who installs only what §0 lists \
             is turned away by a tool the page never mentioned"
        );
    }
}

/// Inline `path/to/file.ext:123` citations on the page, as (path, line).
///
/// Trims the surrounding markdown punctuation rather than parsing code spans:
/// the page mixes inline backticks with fenced blocks, so backtick parity is
/// not a reliable delimiter, while the extension-plus-digits shape is.
fn citations(markdown: &str) -> Vec<(&str, usize)> {
    markdown
        .split_whitespace()
        // Leading `.` is load-bearing — `.github/workflows/...` — so only the
        // trailing side sheds sentence punctuation.
        .map(|word| word.trim_start_matches(|c: char| "`*_([<\"'".contains(c)))
        .map(|word| word.trim_end_matches(|c: char| "`*_.,;)]>\"'".contains(c)))
        .filter_map(|token| {
            let (path, line) = token.rsplit_once(':')?;
            // 1-indexed, so a `:0` is a malformed citation rather than a line.
            let line: usize = line.parse().ok().filter(|n| *n > 0)?;
            let (_, extension) = path.rsplit_once('.')?;
            matches!(extension, "rs" | "md" | "yml" | "yaml" | "toml" | "surql")
                .then_some((path, line))
        })
        .collect()
}

/// §0's whole repair was to stop asserting prerequisites and start citing the
/// code that demands them. A citation nobody checks decays into the same
/// unfounded claim, so hold every one of them to an existing line.
#[test]
fn every_source_citation_on_the_page_resolves() {
    let found = citations(OSS_INSTALL);
    assert!(
        found
            .iter()
            .any(|(path, _)| *path == "deploy/src/devx/orchestrate.rs"),
        "the page must cite the code that gates the cluster lane"
    );

    let root = workspace_root();
    for (path, line) in found {
        let body = fs::read_to_string(root.join(path))
            .unwrap_or_else(|e| panic!("`docs/oss-install.md` cites `{path}:{line}`, but {e}"));
        let cited = body.lines().nth(line - 1).unwrap_or_else(|| {
            panic!(
                "`docs/oss-install.md` cites `{path}:{line}`, but that file has only {} lines",
                body.lines().count()
            )
        });
        assert!(
            !cited.trim().is_empty(),
            "`docs/oss-install.md` cites `{path}:{line}`, which is a blank line — the citation \
             has drifted off the code it was pointing at"
        );
    }
}
