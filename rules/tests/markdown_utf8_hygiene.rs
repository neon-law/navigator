//! The encoding-hygiene gate end to end: an engine walk over a real
//! directory must reject a file whose bytes are not valid UTF-8 — the
//! PR #142 failure mode, where a recommit declared raw text as base64
//! and the committed bytes were binary soup that rendered as CJK
//! mojibake — with a message that names the file.
//!
//! `S106`'s per-character checks live as unit tests beside the rule
//! (`rules/src/s106.rs`); what only an engine walk can prove is that a
//! file no `&str` could ever hold still produces a violation, on the
//! classified path CI actually runs (`navigator project gate`).

use rules::{severity_for_code, ClassifiedRuleEngine, Severity, Violation};
use std::path::PathBuf;

fn classified_s106_violations(dir: &std::path::Path) -> Vec<Violation> {
    ClassifiedRuleEngine::new()
        .lint_directory(dir)
        .expect("engine walk must not fail on a readable temp directory")
        .violations
        .into_iter()
        .filter(|v| v.code == "S106")
        .collect()
}

#[test]
fn a_clean_markdown_file_walks_clean() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("clean.md"),
        "# Title\n\nPlain English prose.\n",
    )
    .unwrap();
    let dir_path = dir.path().to_path_buf();
    assert!(classified_s106_violations(&dir_path).is_empty());
}

#[test]
fn raw_binary_garbage_is_rejected_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("will.md");
    // Raw base64-decoded binary, the PR #142 failure mode: the bytes do
    // not form a valid UTF-8 sequence, so no `&str` can ever hold them.
    let garbage: Vec<u8> = [0x8f, 0xc3, 0x28, 0xe2, 0x82, 0x28, 0xf0, 0x28, 0x8c, 0x28].to_vec();
    std::fs::write(&path, &garbage).unwrap();
    let dir_path = dir.path().to_path_buf();

    let violations = classified_s106_violations(&dir_path);
    assert!(!violations.is_empty(), "corrupt bytes must trip S106");
    let violation = &violations[0];
    assert!(
        violation.message.contains("not valid UTF-8"),
        "the message must name the failure: {}",
        violation.message
    );
    assert_eq!(violation.line, 0);
    assert_eq!(violation.range.start, 0);
    let reported = violation.path.to_string_lossy().replace('\\', "/");
    assert!(
        reported.ends_with("will.md"),
        "the violation must name the file: {reported}"
    );
    // And it fails the gate: S106 is an error severity.
    assert_eq!(severity_for_code("S106"), Severity::Error);
}

#[test]
fn cjk_mojibake_in_a_walked_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // Valid UTF-8, but the CJK ideographs the #142 rendering showed are
    // outside the allowed character set.
    std::fs::write(
        dir.path().join("mojibake.md"),
        "\u{4E2D}\u{6587} \u{6587}\u{5B57}\n",
    )
    .unwrap();
    let dir_path = dir.path().to_path_buf();

    let violations = classified_s106_violations(&dir_path);
    assert_eq!(violations.len(), 4, "one per ideograph");
    let reported = violations[0].path.to_string_lossy().replace('\\', "/");
    assert!(
        reported.ends_with("mojibake.md"),
        "must name the file: {reported}"
    );
    assert!(violations[0]
        .message
        .contains("outside the allowed Markdown character set"));
}

#[test]
fn a_clean_directory_walk_reports_the_file_as_scanned() {
    // Guards the decode refactor against silently skipping files: the
    // byte-level decode must still count a scanned file even when it
    // also produces a decode violation.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "fine\n").unwrap();
    std::fs::write(dir.path().join("b.md"), [0xff, 0xfe]).unwrap();
    let dir_path = dir.path().to_path_buf();
    let report = ClassifiedRuleEngine::new()
        .lint_directory(&dir_path)
        .expect("walk must succeed");
    assert_eq!(report.files_scanned, 2);
    // The 0xFF/0xFE bytes lossy-decode to two U+FFFD characters, so the
    // corrupt file trips the rule three ways: one synthetic decode
    // violation plus one per replacement character. The decode violation
    // is the one that names the raw-byte failure.
    let s106: Vec<&Violation> = report
        .violations
        .iter()
        .filter(|v| v.code == "S106")
        .collect();
    assert_eq!(s106.len(), 3);
    assert_eq!(
        s106.iter()
            .filter(|v| v.message.contains("not valid UTF-8"))
            .count(),
        1,
        "exactly one decode violation names the raw-byte failure"
    );
}

/// Silence the unused-import lint on a path type used only in asserts.
#[allow(dead_code)]
fn _type_witness() -> PathBuf {
    PathBuf::new()
}
