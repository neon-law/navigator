//! The MCP server and the deploy examples speak of Navigator, never the retired
//! assistant names. Two compatibility identifiers stay: the `aida_` tool-name
//! prefix and the `nick+aida@neonlaw.com` mailbox alias.

use std::path::{Path, PathBuf};

const ALLOWED: [&str; 2] = ["nick+aida@neonlaw.com", "aida_"];
const RETIRED: [&str; 2] = ["aida", "aidi"];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn offending_lines(text: &str) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let mut scrubbed = line.to_ascii_lowercase();
        for allowed in ALLOWED {
            scrubbed = scrubbed.replace(allowed, " ");
        }
        let mut words = scrubbed.split(|c: char| !c.is_ascii_alphanumeric());
        if words.any(|word| RETIRED.contains(&word)) {
            hits.push((index + 1, line.trim().to_owned()));
        }
    }
    hits
}

#[test]
fn allowlisted_identifiers_are_not_flagged() {
    assert!(offending_lines("aida_list_projects and nick+aida@neonlaw.com").is_empty());
    assert_eq!(offending_lines("where AIDA reads out").len(), 1);
    assert_eq!(offending_lines("ask Aidi.").len(), 1);
}

#[test]
fn mcp_and_deploy_examples_use_navigator_not_aida_or_aidi() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    walk(&root.join("mcp/src"), &mut files);
    walk(&root.join("examples/deploy"), &mut files);

    let mut failures = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (line, content) in offending_lines(&text) {
            failures.push(format!("{}:{line}: {content}", file.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "retired names found:\n{}",
        failures.join("\n")
    );
}
