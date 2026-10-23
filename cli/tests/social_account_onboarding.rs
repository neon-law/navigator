//! `docs/social-account-onboarding.md` and its synthetic evidence example.
//!
//! The guide names existing Brand keys only, claims no account exists, and
//! lists the scopes `portal::social_oauth` actually verifies. The evidence
//! example carries only synthetic values.

use std::fs;
use std::path::PathBuf;

use views::brand::BrandKey;

const ONBOARDED: &[&str] = &["vesta", "cyber-injury-law", "neon"];
const PLATFORMS: &[&str] = &[
    "facebook",
    "instagram",
    "threads",
    "linkedin",
    "tiktok",
    "x",
];
const STATUSES: &[&str] = &["pending", "verified", "blocked"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|e| panic!("read {relative}: {e}"))
}

/// The cells of every Markdown table row under `heading`, header and
/// separator rows dropped.
fn table_rows(markdown: &str, heading: &str) -> Vec<Vec<String>> {
    let section = markdown
        .split_once(heading)
        .unwrap_or_else(|| panic!("the page has a {heading:?} section"))
        .1;
    section
        .lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .skip(2)
        .map(|line| {
            line.trim()
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().trim_matches('`').to_owned())
                .collect()
        })
        .collect()
}

#[test]
fn the_status_table_names_each_existing_brand_once_and_claims_nothing() {
    let guide = read("docs/social-account-onboarding.md");
    let rows = table_rows(&guide, "## Current status");
    let keys: Vec<&str> = rows.iter().map(|row| row[0].as_str()).collect();
    assert_eq!(keys, ONBOARDED);
    for row in &rows {
        assert!(
            BrandKey::parse(&row[0]).is_some(),
            "{} must be a compiled Brand key, never a new one",
            row[0]
        );
        assert_eq!(row.len(), 1 + PLATFORMS.len(), "{row:?}");
        for cell in &row[1..] {
            assert_eq!(
                cell, "pending",
                "only verified evidence may move a row off pending: {row:?}"
            );
        }
    }
}

#[test]
fn the_capability_matrix_lists_the_scopes_social_oauth_verifies() {
    let guide = read("docs/social-account-onboarding.md");
    let source = read("store/src/social_accounts.rs");
    let list = guide
        .split_once("Required publishing and account identity scopes verified before Navigator stores a connection:")
        .expect("the guide lists verified scopes")
        .1
        .split("\n\n")
        .nth(1)
        .expect("the scope list follows its lead-in");
    let listed: Vec<&str> = list.split('`').skip(1).step_by(2).collect();
    assert!(!listed.is_empty());
    for scope in listed {
        assert!(
            source.contains(&format!("\"{scope}\"")),
            "{scope} is not a scope portal::social_oauth verifies"
        );
    }
}

const FIELDS: &[&str] = &[
    "brand_key",
    "platform",
    "status",
    "owner",
    "connected_by",
    "provider_account_id",
    "scopes_verified",
    "callback_verified",
    "expires_at",
    "reconnect_checked",
    "reviewed_by",
    "notes",
];

#[test]
fn the_guide_documents_every_evidence_field() {
    let guide = read("docs/social-account-onboarding.md");
    let fields: Vec<String> = table_rows(&guide, "## The evidence record")
        .into_iter()
        .map(|row| row[0].clone())
        .collect();
    assert_eq!(fields, FIELDS);
}

#[test]
fn the_evidence_example_is_synthetic_and_well_formed() {
    let example: serde_yaml::Value = serde_yaml::from_str(&read(
        "cli/tests/fixtures/social-account-onboarding/evidence.example.yaml",
    ))
    .expect("the example parses");
    let records = example["records"]
        .as_sequence()
        .expect("the example lists records");
    assert_eq!(records.len(), ONBOARDED.len() * PLATFORMS.len());
    for record in records {
        let record = record.as_mapping().expect("a record is a mapping");
        let field = |name: &str| {
            record
                .get(name)
                .and_then(serde_yaml::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        for key in record.keys() {
            let key = key.as_str().unwrap_or_default();
            assert!(FIELDS.contains(&key), "{key} is not an evidence field");
        }
        let (brand, platform, status) = (field("brand_key"), field("platform"), field("status"));
        assert!(ONBOARDED.contains(&brand.as_str()), "{brand}");
        assert!(BrandKey::parse(&brand).is_some(), "{brand}");
        assert!(PLATFORMS.contains(&platform.as_str()), "{platform}");
        assert!(STATUSES.contains(&status.as_str()), "{status}");
        for person in ["owner", "connected_by", "reviewed_by"].map(field) {
            assert!(
                person.is_empty() || person == "n/a" || person.ends_with("@example.com"),
                "people in the example must use example.com: {person}"
            );
        }
        let account_id = field("provider_account_id");
        assert!(
            account_id.is_empty() || account_id.starts_with("synthetic-"),
            "account ids in the example must be visibly synthetic: {account_id}"
        );
        if status == "verified" {
            for name in &FIELDS[..FIELDS.len() - 1] {
                assert!(!field(name).is_empty(), "a verified record fills {name}");
            }
        }
    }
}
