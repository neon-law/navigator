//! `navigator drafts …` and `navigator templates …` are visible clap aliases
//! of `navigator notation …` (ENG-699): the same command variants, dispatched
//! through the same match arms, just reachable under two more names an
//! operator reaching for "draft" or "template" vocabulary would type first.
//!
//! There is no surviving `--template` flag anywhere in the CLI to alias with
//! `--draft` — the group's `render`, `scaffold`, and `transcribe` verbs, and
//! `transcribe`'s `--template <PATH>`, were retired before this change; the
//! live group is exactly `preview`, `pdf`, and `word`, none of which takes a
//! `--template` option. So this file covers only the group aliases the
//! current tree actually supports.

use std::fs;
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

/// A minimal notation template that passes the full validation gate, shared
/// by the pdf/word render-path checks below.
const VALID: &str = "\
---
kind: letter
title: Test Demand
respondent_type: entity
confidential: true
output: letter
questionnaire:
  BEGIN:
    _: END
  END: {}
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: END
    rejected: END
  END: {}
---

# Demand

Pay the sum of `{{amount}}` to **NEON LAW** without delay.
";

/// The three spellings that must dispatch identically.
const GROUP_SPELLINGS: [&str; 3] = ["notation", "drafts", "templates"];

fn navigator(args: &[&str]) -> std::process::Output {
    Command::new(cargo_bin("navigator"))
        .args(args)
        .output()
        .expect("run navigator")
}

fn write(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, body).expect("write fixture");
    path
}

#[test]
fn top_level_help_shows_both_visible_group_aliases() {
    let output = navigator(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    let notation_row = help
        .lines()
        .find(|line| line.trim_start().starts_with("notation"))
        .unwrap_or_else(|| panic!("no `notation` row in:\n{help}"));
    assert!(
        notation_row.contains("drafts") && notation_row.contains("templates"),
        "the `notation` row must advertise both visible aliases: {notation_row:?}"
    );
}

#[test]
fn preview_help_is_identical_across_all_three_spellings() {
    let outputs: Vec<String> = GROUP_SPELLINGS
        .iter()
        .map(|group| {
            let output = navigator(&[group, "preview", "--help"]);
            assert!(output.status.success(), "`{group} preview --help` failed");
            String::from_utf8_lossy(&output.stdout).to_string()
        })
        .collect();

    assert_eq!(
        outputs[0], outputs[1],
        "`notation preview --help` and `drafts preview --help` must describe \
         the same arguments and behavior"
    );
    assert_eq!(
        outputs[0], outputs[2],
        "`notation preview --help` and `templates preview --help` must describe \
         the same arguments and behavior"
    );
}

#[test]
fn pdf_renders_the_same_document_through_every_group_spelling() {
    let mut renders = Vec::new();
    for group in GROUP_SPELLINGS {
        let work = TempDir::new().unwrap();
        let src = write(&work, "demand.md", VALID);
        let out = work.path().join("demand.pdf");
        let output = navigator(&[
            group,
            "pdf",
            src.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--preview",
        ]);
        assert!(
            output.status.success(),
            "`{group} pdf` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let bytes = fs::read(&out).expect("pdf written");
        assert_eq!(&bytes[..4], b"%PDF", "`{group} pdf` did not write a PDF");
        renders.push(bytes);
    }

    assert_eq!(
        renders[0], renders[1],
        "`notation pdf` and `drafts pdf` must render byte-identical output"
    );
    assert_eq!(
        renders[0], renders[2],
        "`notation pdf` and `templates pdf` must render byte-identical output"
    );
}

#[test]
fn word_renders_the_same_document_through_every_group_spelling() {
    let mut renders = Vec::new();
    for group in GROUP_SPELLINGS {
        let work = TempDir::new().unwrap();
        let src = write(&work, "demand.md", VALID);
        let out = work.path().join("demand.docx");
        let output = navigator(&[
            group,
            "word",
            src.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--preview",
        ]);
        assert!(
            output.status.success(),
            "`{group} word` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let bytes = fs::read(&out).expect("docx written");
        assert_eq!(
            &bytes[..4],
            b"PK\x03\x04",
            "`{group} word` did not write an Open XML package"
        );
        renders.push(bytes);
    }

    assert_eq!(
        renders[0], renders[1],
        "`notation word` and `drafts word` must render byte-identical output"
    );
    assert_eq!(
        renders[0], renders[2],
        "`notation word` and `templates word` must render byte-identical output"
    );
}

#[test]
fn an_alias_still_refuses_a_verb_the_group_does_not_have() {
    // `render`, `scaffold`, and `transcribe` were retired from this group
    // before this change; an alias must not resurrect them.
    for group in GROUP_SPELLINGS {
        let output = navigator(&[group, "scaffold"]);
        assert!(
            !output.status.success(),
            "`{group} scaffold` must not resolve to anything"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("unrecognized subcommand"),
            "`{group} scaffold` must fail as an unknown subcommand: {stderr}"
        );
    }
}
