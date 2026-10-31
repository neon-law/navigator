//! Guard that `docs/gate.md` documents every rule code that actually ships.
//!
//! ENG-381 found 54 of 84 codes documented nowhere: the whole M-family, plus C003, E002, N112,
//! N116, S102, and Y001. This test enumerates every code the `rules` crate and `cli`'s own
//! seed-document pass emit and fails, naming each offender, when one has no entry in
//! `docs/gate.md` — so a new rule cannot merge without its row in the table, which is how the
//! original 54 accumulated (N120 and Y001 both shipped in the two days before this test existed,
//! neither with a doc change).

use std::collections::BTreeSet;
use std::fs;

/// `Y001` is the seed-document pass's rule code. It lives in `cli/src/main.rs`, not the `rules`
/// crate, because the pass it guards runs outside `ClassifiedRuleEngine` entirely.
const SEED_DOCUMENT_CODE: &str = "Y001";
const LOCALE_DOCUMENT_CODE: &str = "Y002";
const DOCUMENT_POINTER_CODE: &str = "Y003";
const MANIFEST_HOST_CODE: &str = "Y004";
const MANIFEST_PROJECT_CODE: &str = "Y005";
const MANIFEST_UNKNOWN_KEY_CODE: &str = "Y006";
const MANIFEST_ROWLESS_CODE: &str = "Y007";
const MANIFEST_RENAME_CODE: &str = "Y008";
/// `Y010` is the Project-template entity check in `cli/src/projects/repository.rs`.
const ENTITY_CODE: &str = "Y010";
/// `Y017` restricts trade-name wording in onboarding/offboarding templates.
const FIRM_TRADE_NAME_CODE: &str = "Y017";
/// `Y018` restricts California law-firm entity wording in onboarding/offboarding templates.
const CALIFORNIA_LAW_FIRM_ENTITY_CODE: &str = "Y018";
const MANIFEST_COMMENT_CODE: &str = "Y011";
const MANIFEST_VERSION_CODE: &str = "Y012";
const MANIFEST_DEPRECATED_CODE: &str = "Y013";
const DOCUMENT_GITIGNORE_CODE: &str = "Y014";
/// `Y015` is the Project-manifest `skills:` entry shape check, in
/// `cli/src/projects/manifest.rs`.
const MANIFEST_SKILLS_CODE: &str = "Y015";
/// `Y021` is the Project-manifest `pages:` shape check, in
/// `cli/src/projects/manifest.rs`; the D-family that checks each entry's
/// composition arrives through `navigator_pages_rules` below.
const MANIFEST_PAGES_CODE: &str = "Y021";
/// `Y023` is the live gate's warning for a declared section whose data source
/// has no rows on the live matter, in `cli/src/projects/gate.rs`.
const SECTION_DATA_CODE: &str = "Y023";
/// `Y024` refuses every tracked Project-repository file that is not Markdown or
/// YAML or a canonical file at its release bytes, in `cli/src/projects/repository.rs`.
const TRACKED_FILE_CODE: &str = "Y024";
/// `F001` is the formatting pass's code: under `--ci` the gate withholds a fix
/// it would otherwise have written, and reports the file instead.
const UNFORMATTED_CODE: &str = "F001";
/// `P001` is the public snapshot audit over every tracked file, in
/// `cli/src/snapshot_audit.rs`.
const PUBLIC_SNAPSHOT_CODE: &str = "P001";

fn all_shipped_codes() -> BTreeSet<&'static str> {
    let mut codes = BTreeSet::new();
    let rule_sets: Vec<Vec<Box<dyn rules::Rule>>> = vec![
        rules::engine::navigator_default_rules(),
        rules::engine::navigator_markdown_only_rules(),
        rules::engine::navigator_event_rules(),
        rules::engine::navigator_blog_rules(),
        rules::engine::navigator_workshop_rules(),
        rules::engine::navigator_github_rules(),
        rules::engine::navigator_pages_rules(),
    ];
    for rules in rule_sets {
        for rule in rules {
            codes.insert(rule.code());
        }
    }
    // N111, N124, N126, and N127 are cross-file checks, not `Rule` impls, so no rule-set above
    // carries them.
    codes.insert("N111");
    codes.insert("N124");
    codes.insert("N126");
    codes.insert("N127");
    codes.insert(SEED_DOCUMENT_CODE);
    codes.insert(LOCALE_DOCUMENT_CODE);
    codes.insert(DOCUMENT_POINTER_CODE);
    codes.insert(MANIFEST_HOST_CODE);
    codes.insert(MANIFEST_PROJECT_CODE);
    codes.insert(MANIFEST_UNKNOWN_KEY_CODE);
    codes.insert(MANIFEST_ROWLESS_CODE);
    codes.insert(MANIFEST_RENAME_CODE);
    codes.insert(ENTITY_CODE);
    codes.insert(FIRM_TRADE_NAME_CODE);
    codes.insert(CALIFORNIA_LAW_FIRM_ENTITY_CODE);
    codes.insert(MANIFEST_COMMENT_CODE);
    codes.insert(MANIFEST_VERSION_CODE);
    codes.insert(MANIFEST_DEPRECATED_CODE);
    codes.insert(DOCUMENT_GITIGNORE_CODE);
    codes.insert(MANIFEST_SKILLS_CODE);
    codes.insert(MANIFEST_PAGES_CODE);
    codes.insert(SECTION_DATA_CODE);
    codes.insert(TRACKED_FILE_CODE);
    codes.insert(UNFORMATTED_CODE);
    codes.insert(PUBLIC_SNAPSHOT_CODE);
    codes.insert(cli::pii_audit::CODE);
    codes
}

#[test]
fn every_shipped_code_has_an_entry_in_the_gate_docs() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let gate_md = fs::read_to_string(root.join("docs/gate.md")).expect("read docs/gate.md");

    let missing: Vec<&str> = all_shipped_codes()
        .into_iter()
        .filter(|code| !gate_md.contains(code))
        .collect();

    assert!(
        missing.is_empty(),
        "docs/gate.md has no entry for: {}. Add a row to the matching family table.",
        missing.join(", ")
    );
}

/// Pin the exhaustive count so a rule addition or removal is a visible diff here, not a silent
/// change to how many codes the doc is supposed to cover.
#[test]
fn the_shipped_code_count_is_one_hundred_and_twenty() {
    assert_eq!(all_shipped_codes().len(), 120);
}
