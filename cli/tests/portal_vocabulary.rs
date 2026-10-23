//! "Portal" names one thing; this stops "client portal" blurring it.
//!
//! A Portal is the per-Project client application a Project publishes from
//! `apps/portal/`, served at `/app/projects/{code}/portal/` (see
//! `docs/glossary/portal.md`). Two neighbours sit close enough to be mistaken
//! for it: the **matter page**, `/app/projects/{code}`, which is Navigator's own
//! show page for a Project, and the **`portal` crate**, which serves `/app/` as
//! a whole and is "the web app" in prose. "Client portal" reads naturally for
//! all three, so a doc comment, an `OpenAPI` description, or a line of copy that
//! uses it says nothing about which surface it means.
//!
//! So prose says "Portal", "matter page", or "web app", and the phrase "client
//! portal" (or "client-portal") appears nowhere it is not allowed. What
//! survives is the link label that names the Portal's audience out loud, the
//! marketing copy that describes it, and a few identifiers — a `data-resource`
//! slug, a CSS modifier, a screenshot filename — that are names rather than
//! prose. Each is allowed explicitly rather than by exempting whole files, so a
//! file carrying an allowed phrase still fails if a loose one appears in it.

use std::fs;
use std::path::{Path, PathBuf};

/// Substrings that may legitimately contain "client portal" or
/// "client-portal", lowercased. A line is cleared of every one of these before
/// it is checked, so a line carrying both an allowed phrase and a loose use
/// still fails.
const ALLOWED: &[&str] = &[
    // `webapp/src/project_resources.rs` — `ResourceKind::label`, the Portal
    // link's text, which spends its words on the audience the mark cannot show.
    // Mirrored by `webapp/src/design.rs` and asserted as exact copy by
    // `server/tests/browser_e2e.rs`, `server/tests/dioxus_clerk_ssr.rs`, and
    // `features/src/portfolio_acceptance.rs`, and by the diagram fixture in
    // `webapp/src/marketing_page.rs`.
    "\"client portal\"",
    // `webapp/src/project_resources.rs` — `ResourceKind::name`, emitted as the
    // row's `data-resource` slug and asserted by
    // `server/tests/project_integrations_buttons.rs`.
    "\"client-portal\"",
    // `webapp/src/firm_footer.rs` — the matter page footer's CSS modifier, read
    // by `portal/src/dioxus_app.rs` and `webapp/src/portal_project_detail.rs`.
    "app-footer--client-portal",
];

/// The phrase, in both spellings, after lowercasing.
const FORBIDDEN: &[&str] = &["client portal", "client-portal"];

/// Directories the walk never descends into: build output, VCS metadata, and
/// the local dev-environment scratch dir.
///
/// `worktrees` (no dot) is the directory the harness creates linked checkouts
/// under — `.claude/worktrees/<branch>/` is a whole second copy of the tree.
/// Without it the walk reads another branch's files and reports them against
/// this one.
const SKIPPED_DIRS: &[&str] = &[
    "target",
    ".git",
    "node_modules",
    ".worktrees",
    "worktrees",
    ".devx",
];

/// Files exempt by provenance rather than by name: only this test, whose own
/// allowlist above is written in the thing it forbids.
const SKIPPED_FILES: &[&str] = &["portal_vocabulary.rs"];

/// The workspace root (this test crate is `cli`).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The line with every allowed phrase removed. Whatever phrase remains is a
/// genuine leftover.
fn strip_allowed(line: &str) -> String {
    let mut out = line.to_lowercase();
    for allowed in ALLOWED {
        out = out.replace(allowed, " ");
    }
    out
}

fn is_loose(text: &str) -> bool {
    let stripped = strip_allowed(text);
    FORBIDDEN.iter().any(|phrase| stripped.contains(phrase))
}

fn walk(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // `.git` is a directory in a normal checkout and a *file* holding a
        // `gitdir:` pointer inside a worktree, so it is skipped by name before
        // the directory test.
        if SKIPPED_DIRS.contains(&name.as_ref()) {
            continue;
        }
        if path.is_dir() {
            walk(&path, out);
            continue;
        }
        if SKIPPED_FILES.contains(&name.as_ref()) {
            continue;
        }
        let relative = path.strip_prefix(repo_root()).unwrap_or(&path);
        let displayed = relative.to_string_lossy().to_string();
        if is_loose(&displayed) {
            out.push(format!("{displayed}: path says \"client portal\""));
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in body.lines().enumerate() {
            if is_loose(line) {
                out.push(format!("{displayed}:{}: {}", i + 1, line.trim()));
            }
        }
    }
}

#[test]
fn client_portal_appears_nowhere_it_is_not_allowed() {
    let mut hits = Vec::new();
    walk(&repo_root(), &mut hits);
    assert!(
        hits.is_empty(),
        "\"Portal\" means a Project's own application at \
         `/app/projects/{{code}}/portal/`. Write \"Portal\" for that, \"matter \
         page\" for `/app/projects/{{code}}`, and \"web app\" for the `portal` \
         crate as a whole (docs/glossary/portal.md). Found {} loose \
         \"client portal\" occurrence(s):\n  {}",
        hits.len(),
        hits.join("\n  ")
    );
}

/// The exclusions have to stay reachable, or the guard silently widens: a
/// typo'd entry in `ALLOWED` would match nothing and the test would keep
/// passing while permitting less than it claims.
#[test]
fn every_allowed_phrase_still_occurs_in_the_workspace() {
    let mut unused = Vec::new();
    for allowed in ALLOWED {
        let mut found = false;
        search(&repo_root(), allowed, &mut found);
        if !found {
            unused.push(*allowed);
        }
    }
    assert!(
        unused.is_empty(),
        "these exclusions match nothing in the workspace and should be dropped \
         (or are misspelled, which would let the loose phrase back in): \
         {unused:?}"
    );
}

fn search(dir: &Path, needle: &str, found: &mut bool) {
    if *found {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SKIPPED_DIRS.contains(&name.as_ref()) {
            continue;
        }
        if path.is_dir() {
            search(&path, needle, found);
            if *found {
                return;
            }
            continue;
        }
        if SKIPPED_FILES.contains(&name.as_ref()) {
            continue;
        }
        if let Ok(body) = fs::read_to_string(&path) {
            if body.to_lowercase().contains(needle) {
                *found = true;
                return;
            }
        }
    }
}

#[cfg(test)]
mod classifier {
    use super::*;

    #[test]
    fn the_loose_phrase_is_caught_in_either_spelling_and_any_case() {
        assert!(is_loose("drops out of the client portal"));
        assert!(is_loose("each Project's published client-portal bundle"));
        assert!(is_loose("Client Portal"));
    }

    #[test]
    fn an_allowed_phrase_does_not_shield_a_loose_one_on_the_same_line() {
        assert!(!is_loose(r#"Self::ClientPortal => "Client portal","#));
        assert!(is_loose(r#""Client portal" links to the client portal"#));
    }

    #[test]
    fn the_glossary_words_pass() {
        assert!(!is_loose("the Portal at /app/projects/{code}/portal/"));
        assert!(!is_loose("the matter page and the web app"));
        assert!(!is_loose("ResourceKind::ClientPortal"));
    }
}
