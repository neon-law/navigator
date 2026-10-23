//! A Firm's slug: the stable, URL-safe name its Forgejo organization carries.
//!
//! [`derive`] turns a Firm's name into a slug once, when the Firm is created
//! (or, for a Firm that predates the field, when `store::schema::apply`
//! backfills it). After that the slug never changes: renaming a Firm edits
//! its displayed name only, so its organization never moves.
//!
//! The shape is a Project code's shape ([`cloud::workspace::is_valid_slug`]),
//! capped at [`MAX_LEN`] because that is Forgejo's limit on an organization
//! name, and refusing [`RESERVED`]. A name that derives no valid slug, or the
//! slug another Firm already holds, is refused; nothing appends a suffix to
//! make one fit.

use std::collections::BTreeMap;

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

/// The longest slug: Forgejo's limit on a user or organization name.
pub const MAX_LEN: usize = 40;

/// Names a Firm slug may not take.
///
/// Forgejo's reserved user and organization names that fit the slug shape
/// (its route segments, such as `admin`, `api`, `explore`, and `user`), plus
/// `navigator`, which names Navigator's own source.
pub const RESERVED: &[&str] = &[
    "admin",
    "api",
    "assets",
    "attachments",
    "avatar",
    "avatars",
    "captcha",
    "commits",
    "debug",
    "devtest",
    "error",
    "explore",
    "forgejo-actions",
    "ghost",
    "gitea-actions",
    "issues",
    "login",
    "metrics",
    "milestones",
    "navigator",
    "new",
    "notifications",
    "org",
    "pulls",
    "raw",
    "repo",
    "repo-avatars",
    "search",
    "user",
    "v2",
];

/// Apostrophes are dropped rather than read as separators, so `O'Neil`
/// becomes `oneil`, not `o-neil`.
const APOSTROPHES: &[char] = &['\'', '\u{2019}', '\u{02BC}'];

/// Derive a slug from a Firm name.
///
/// The name is decomposed (Unicode NFKD) and its combining marks dropped, so
/// `É` becomes `e` and the `ﬁ` ligature becomes `fi`. Apostrophes are
/// dropped. ASCII letters are lowercased and kept with ASCII digits; every
/// other run of characters becomes one hyphen. Leading and trailing hyphens
/// are trimmed, and the result is cut to [`MAX_LEN`] at a hyphen-free end.
///
/// The result may still be invalid (empty, or reserved); [`is_valid`] is the
/// check. The PostgreSQL migration's `firm_slug_from_name` function performs
/// the same steps for the rows it backfills.
#[must_use]
pub fn derive(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut separator = false;
    for character in name.nfkd() {
        if is_combining_mark(character) || APOSTROPHES.contains(&character) {
            continue;
        }
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            separator = false;
            slug.push(character.to_ascii_lowercase());
        } else {
            separator = true;
        }
    }
    slug.truncate(MAX_LEN);
    slug.trim_end_matches('-').to_string()
}

/// Whether `slug` is a valid Firm slug: a Project code's shape, at most
/// [`MAX_LEN`] characters, and not [`RESERVED`].
#[must_use]
pub fn is_valid(slug: &str) -> bool {
    slug.len() <= MAX_LEN && cloud::workspace::is_valid_slug(slug) && !RESERVED.contains(&slug)
}

/// Why the slug backfill refuses to run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackfillRefusal {
    /// The Firm's name derives no valid slug (empty, or reserved).
    #[error("firm {firm_id} derives the invalid slug {slug:?}")]
    Invalid { firm_id: Uuid, slug: String },
    /// Two Firms would hold the same slug.
    #[error("firms {first} and {second} both derive the slug {slug:?}")]
    Collision {
        slug: String,
        first: Uuid,
        second: Uuid,
    },
}

/// One `firm` row as the backfill sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillRow {
    pub id: Uuid,
    pub name: String,
    pub slug: Option<String>,
}

/// The slugs to write for every row that has none, derived from its name.
///
/// Refuses the whole batch when any derived slug is invalid, or when two
/// rows, whether both new or one already stored, would share a slug. A row
/// that already carries a slug keeps it.
pub fn plan_backfill(rows: &[BackfillRow]) -> Result<Vec<(Uuid, String)>, BackfillRefusal> {
    let mut holders: BTreeMap<String, Uuid> = rows
        .iter()
        .filter_map(|row| row.slug.clone().map(|slug| (slug, row.id)))
        .collect();
    let mut planned = Vec::new();
    for row in rows.iter().filter(|row| row.slug.is_none()) {
        let slug = derive(&row.name);
        if !is_valid(&slug) {
            return Err(BackfillRefusal::Invalid {
                firm_id: row.id,
                slug,
            });
        }
        if let Some(first) = holders.get(&slug) {
            return Err(BackfillRefusal::Collision {
                slug,
                first: *first,
                second: row.id,
            });
        }
        holders.insert(slug.clone(), row.id);
        planned.push((row.id, slug));
    }
    Ok(planned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_lowercases_and_hyphenates() {
        assert_eq!(derive("Example Law PC"), "example-law-pc");
        assert_eq!(derive("EXAMPLE law pc"), "example-law-pc");
        assert_eq!(derive("Practice 42"), "practice-42");
    }

    #[test]
    fn derivation_turns_punctuation_into_one_separator() {
        assert_eq!(derive("Smith & Jones, LLP"), "smith-jones-llp");
        assert_eq!(derive("Example Law, P.C."), "example-law-p-c");
        assert_eq!(derive("Under_score/Slash"), "under-score-slash");
        assert_eq!(derive("O'Neil Legal"), "oneil-legal");
        assert_eq!(derive("O\u{2019}Neil Legal"), "oneil-legal");
    }

    #[test]
    fn derivation_collapses_and_trims_separator_runs() {
        assert_eq!(derive("  Example --- Law  "), "example-law");
        assert_eq!(derive("--Example__Law--"), "example-law");
        assert_eq!(derive("Example \t\n Law"), "example-law");
    }

    #[test]
    fn derivation_folds_unicode_to_ascii() {
        assert_eq!(derive("Éxample Låw"), "example-law");
        // Precomposed and decomposed forms derive the same slug.
        assert_eq!(derive("Caf\u{00E9}"), derive("Cafe\u{0301}"));
        assert_eq!(derive("Caf\u{00E9}"), "cafe");
        // Compatibility forms fold too: the `ﬁ` ligature and full-width letters.
        assert_eq!(derive("\u{FB01}rm"), "firm");
        assert_eq!(derive("\u{FF25}xample"), "example");
        // A character with no ASCII decomposition is a separator.
        assert_eq!(derive("Example \u{00DF} Law"), "example-law");
        assert_eq!(derive("事務所"), "");
    }

    #[test]
    fn derivation_cuts_to_the_limit_without_a_trailing_hyphen() {
        let long = derive("Example Synthetic Practice Of Several Long Words");
        assert!(long.len() <= MAX_LEN);
        assert_eq!(long, "example-synthetic-practice-of-several-lo");
        let at_boundary = derive("Example Synthetic Practice Of Several L Words");
        assert_eq!(at_boundary, "example-synthetic-practice-of-several-l");
        let cut_at_hyphen = derive("Example Synthetic Practice Of Several Xy");
        assert_eq!(cut_at_hyphen, "example-synthetic-practice-of-several-xy");
        let trailing = derive("Example Synthetic Practice Of Severalxy Z");
        assert_eq!(trailing, "example-synthetic-practice-of-severalxy");
    }

    #[test]
    fn invalid_and_reserved_slugs_are_refused() {
        assert!(is_valid("example-law-pc"));
        assert!(is_valid(&"a".repeat(MAX_LEN)));
        assert!(!is_valid(&"a".repeat(MAX_LEN + 1)));
        for invalid in [
            "",
            "-example",
            "example-",
            "exa--mple",
            "Example",
            "ex_ample",
            "ex ample",
        ] {
            assert!(!is_valid(invalid), "{invalid:?}");
        }
        for reserved in RESERVED {
            assert!(
                cloud::workspace::is_valid_slug(reserved),
                "{reserved} is slug-shaped"
            );
            assert!(!is_valid(reserved), "{reserved} is reserved");
        }
        assert!(!is_valid(&derive("Admin")));
        assert!(!is_valid(&derive("Navigator")));
        assert!(!is_valid(&derive("!!!")));
    }

    fn row(name: &str, slug: Option<&str>) -> BackfillRow {
        BackfillRow {
            id: Uuid::now_v7(),
            name: name.to_string(),
            slug: slug.map(str::to_string),
        }
    }

    #[test]
    fn backfill_derives_only_missing_slugs() {
        let kept = row("Renamed Practice", Some("original-practice"));
        let missing = row("Example Law PC", None);
        let planned = plan_backfill(&[kept, missing.clone()]).unwrap();
        assert_eq!(planned, vec![(missing.id, "example-law-pc".to_string())]);
    }

    #[test]
    fn backfill_refuses_a_collision_instead_of_suffixing() {
        let first = row("Example Law PC", None);
        let second = row("EXAMPLE Law -- PC", None);
        assert_eq!(
            plan_backfill(&[first.clone(), second.clone()]),
            Err(BackfillRefusal::Collision {
                slug: "example-law-pc".into(),
                first: first.id,
                second: second.id,
            })
        );
        let stored = row("Other Name", Some("example-law-pc"));
        assert!(matches!(
            plan_backfill(&[stored, first]),
            Err(BackfillRefusal::Collision { .. })
        ));
    }

    #[test]
    fn backfill_refuses_an_invalid_derived_slug() {
        let reserved = row("Admin", None);
        assert_eq!(
            plan_backfill(std::slice::from_ref(&reserved)),
            Err(BackfillRefusal::Invalid {
                firm_id: reserved.id,
                slug: "admin".into(),
            })
        );
        assert!(matches!(
            plan_backfill(&[row("事務所", None)]),
            Err(BackfillRefusal::Invalid { .. })
        ));
    }
}
