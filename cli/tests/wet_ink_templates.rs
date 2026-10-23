//! Integration tests for LAW-84: a wet-ink signable instrument may omit
//! `questionnaire:` and `workflow:` the same way any other prose-only,
//! attorney-drafted blueprint can. These drive the compiled binary through
//! `assert_cmd`, exercising the real `navigator project gate` path rather
//! than the `rules` crate directly.
//!
//! What must keep failing is covered here too: a partially declared machine
//! pair, an explicitly declared but malformed machine, missing
//! `lawyer_review`, and an electronic signature placeholder that still
//! needs its `sent_for_signature` workflow state (N107) — none of which the
//! omission bypass touches.
//!
//! Fixtures use `kind: letter`, which N123 does not hold to a Harvard
//! outline, so the body can carry pure wet-ink prose without also having to
//! satisfy the outline rule that `agreement`/`will`/`pleading`/`offboarding`
//! templates do.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::str;
use tempfile::TempDir;

fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn navigator() -> Command {
    let mut command = Command::cargo_bin("navigator").unwrap();
    command.env_remove("GITHUB_REPOSITORY");
    command
}

/// Arrange `root` as a repository root (a `README` and `.git` beside it,
/// which is how the gate identifies one) and invoke `project gate` there.
fn gate(root: &Path) -> Command {
    let has_readme = fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("README"));
    if !has_readme {
        fs::write(root.join("README"), "fixture\n").unwrap();
    }
    if !root.join(".git").exists() {
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git init failed in {}", root.display());
    }
    let mut command = navigator();
    command.current_dir(root).args(["project", "gate"]);
    command
}

/// The six keys every notation template needs regardless of whether it
/// declares `questionnaire:`/`workflow:`.
const REQUIRED_FRONTMATTER: &str = "kind: letter\n\
title: Wet Ink Instrument\n\
code: wet_ink__test_instrument\n\
respondent_type: person\n\
jurisdiction: NV\n\
confidential: true\n";

/// An attorney-drafted instrument with manual signature, witness, and
/// notary acknowledgment lines — none of it a `{{signer.field}}`
/// placeholder, and none of it collected or run by Navigator.
const WET_INK_BODY: &str = "\nThe undersigned executes this instrument as follows.\n\n\
Signature: ____________________\n\n\
Witness Signature: ____________________\n\n\
State of Nevada\nCounty of Washoe\n\n\
Subscribed and sworn before me this ____ day of __________, 20__.\n\n\
Notary Signature: ____________________\n\
My Commission Expires: ____________________\n";

#[test]
fn gate_accepts_a_wet_ink_instrument_with_no_questionnaire_or_workflow() {
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "templates/notations/neon_law/wet_ink_instrument.md",
        &format!("---\n{REQUIRED_FRONTMATTER}---\n{WET_INK_BODY}"),
    );
    gate(dir.path())
        .assert()
        .success()
        .stdout(str::contains("found 0 error(s)"));
}

#[test]
fn gate_still_flags_a_wet_ink_document_that_declares_only_one_machine() {
    // The pairing guardrail holds under LAW-84: a document that declares one
    // of `questionnaire:`/`workflow:` must declare the other, even though a
    // manual signing line forces neither.
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "templates/half_declared.md",
        &format!(
            "---\n{REQUIRED_FRONTMATTER}questionnaire:\n  BEGIN:\n    _: END\n  END: {{}}\n---\n{WET_INK_BODY}"
        ),
    );
    gate(dir.path())
        .assert()
        .failure()
        .code(1)
        .stdout(str::contains("N104"))
        .stdout(str::contains("Missing required `workflow`"));
}

#[test]
fn gate_still_flags_a_malformed_declared_workflow_on_a_wet_ink_document() {
    // Explicitly declared machines are still validated: an invalid
    // workflow step prefix fails N104 regardless of the body being
    // otherwise wet-ink signable.
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "templates/malformed_workflow.md",
        &format!(
            "---\n{REQUIRED_FRONTMATTER}questionnaire:\n  BEGIN:\n    _: END\n  END: {{}}\n\
             workflow:\n  BEGIN:\n    created: lawyer_review\n  lawyer_review:\n    \
             approved: bespoke_magic\n  bespoke_magic:\n    done: END\n  END: {{}}\n---\n{WET_INK_BODY}"
        ),
    );
    gate(dir.path())
        .assert()
        .failure()
        .code(1)
        .stdout(str::contains("N104"))
        .stdout(str::contains(
            "Invalid workflow step prefix: `bespoke_magic`",
        ));
}

#[test]
fn gate_still_requires_lawyer_review_when_a_wet_ink_document_declares_a_workflow() {
    // N106 is untouched: a template that opts into declaring a workflow
    // at all must still route it through `lawyer_review`.
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "templates/no_lawyer_review.md",
        &format!(
            "---\n{REQUIRED_FRONTMATTER}questionnaire:\n  BEGIN:\n    _: END\n  END: {{}}\n\
             workflow:\n  BEGIN:\n    created: END\n  END: {{}}\n---\n{WET_INK_BODY}"
        ),
    );
    gate(dir.path())
        .assert()
        .failure()
        .code(1)
        .stdout(str::contains("N106"))
        .stdout(str::contains("lawyer_review"));
}

#[test]
fn gate_still_requires_a_signing_workflow_state_for_an_electronic_signature_placeholder() {
    // N107's forward check is untouched: a body that draws a real
    // `{{signer.field}}` placeholder is not wet-ink prose, so it still
    // needs a `sent_for_signature[__*]` workflow state to collect it — the
    // omission bypass never rescues a template whose body actually carries
    // an electronic signature tab.
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "templates/electronic_signature.md",
        &format!("---\n{REQUIRED_FRONTMATTER}---\n\n{{{{client.signature}}}}\n"),
    );
    gate(dir.path())
        .assert()
        .failure()
        .code(1)
        .stdout(str::contains("N107"))
        .stdout(str::contains("no `sent_for_signature`"));
}

#[test]
fn gate_still_requires_a_body_anchor_for_a_declared_signing_workflow_state() {
    // N107's reverse check is untouched: a declared `sent_for_signature`
    // state with no body placeholder for the tab to land on still fails,
    // even though the same document could have omitted the workflow
    // entirely and passed clean.
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "templates/signing_state_no_anchor.md",
        &format!(
            "---\n{REQUIRED_FRONTMATTER}questionnaire:\n  BEGIN:\n    _: END\n  END: {{}}\n\
             workflow:\n  BEGIN:\n    created: lawyer_review\n  lawyer_review:\n    \
             approved: sent_for_signature__pending\n  \
             sent_for_signature__pending:\n    signature_received: END\n  END: {{}}\n---\n{WET_INK_BODY}"
        ),
    );
    gate(dir.path())
        .assert()
        .failure()
        .code(1)
        .stdout(str::contains("N107"))
        .stdout(str::contains("no signature anchor"));
}
