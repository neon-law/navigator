//! The Firm assumption keys a Kit may declare — the dotted paths in
//! `assumptions/schema.yaml`, embedded once for the whole workspace.
//!
//! The Firm Assumptions catalog (`store::firm_assumptions`) owns the keys'
//! types, defaults, and descriptions. That crate cannot be read from `rules`
//! (`rules` carries no `store` dependency by design — the same engine backs
//! the LSP, `cli validate`, and CI, none of which open a database), so this
//! module embeds the same schema file and exposes only what the Project Skill
//! gate (`N128`) needs: the set of keys a catalog entry may declare. The
//! embed stays single-source: `store::firm_assumptions::SCHEMA_YAML` re-exports
//! this static rather than including the file a second time.

use std::collections::BTreeSet;

/// The raw `assumptions/schema.yaml` text — the one embed of the file.
pub const SCHEMA_YAML: &str = include_str!("../../assumptions/schema.yaml");

/// Every dotted key the Firm Assumptions catalog defines, sorted. A Project
/// Skill may declare only these keys in its `assumptions:` frontmatter; the
/// gate refuses anything else rather than letting a typo'd key render blank
/// quietly at run time.
#[must_use]
pub fn catalog_keys() -> BTreeSet<String> {
    let raw: serde_yaml::Mapping =
        serde_yaml::from_str(SCHEMA_YAML).expect("assumptions/schema.yaml parses as a mapping");
    raw.keys()
        .filter_map(|key| key.as_str().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{catalog_keys, SCHEMA_YAML};

    #[test]
    fn the_embed_reads_the_repository_schema_file() {
        assert!(
            SCHEMA_YAML.contains("plan.target_minutes.document_review"),
            "the embed carries the seeded document-review key"
        );
        assert!(
            SCHEMA_YAML.contains("plan.target_minutes.severance_review"),
            "the embed carries the severance-review key the layoff kits declare"
        );
    }

    #[test]
    fn the_key_set_is_dotted_and_sorted() {
        let keys = catalog_keys();
        assert!(!keys.is_empty(), "the catalog defines keys");
        for key in &keys {
            assert!(
                key.contains('.') && !key.contains(char::is_whitespace),
                "{key} is a dotted path without whitespace"
            );
        }
        let last = keys.iter().last().expect("non-empty");
        let first = keys.iter().next().expect("non-empty");
        assert!(first <= last, "the set sorts its keys");
    }
}
