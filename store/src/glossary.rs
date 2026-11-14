//! The firm glossary as **reference data** — authored in the repository,
//! materialized into rows.
//!
//! Both, not either. `docs/glossary/` — one Markdown file per term —
//! stays the source of truth because
//! the glossary is where load-bearing distinctions live — that Lawyer
//! includes attorneys, that Participation is not the disclosures table —
//! and those get reviewed in pull requests. A controlled vocabulary that
//! changed through an admin form with no code review would be a real loss
//! for a legal product.
//!
//! Materializing it anyway buys the thing a file cannot: a dashboard
//! section, a notation template, or a questionnaire prompt can
//! **reference a term by slug** instead of restating it, so the
//! definition has exactly one home and every surface that shows it agrees.
//!
//! # Where this table lives
//!
//! PostgreSQL holds it wherever it is configured: every read and write
//! here goes to PostgreSQL
//! ([`crate::postgres::catalog`]).
//!
//! `glossary_terms` is a leaf reference table nothing links to and that
//! links to nothing. The unique `glossary_term_slug` index is what
//! enforces one row per slug; a violation has no typed detail, so
//! [`crate::surreal::retry::unique_violation`] discriminates on that index
//! name. PostgreSQL's `slug` key lets [`materialize`] upsert instead.
//!
//! # The distinction that must not be collapsed
//!
//! A contract's **defined terms** — the capitalized definitions scoped to
//! one document — are *matter data*: per-project, never seeded, covered by
//! the no-client-data rule. The **firm glossary** is reference data:
//! universal, identical in every deployment.
//!
//! They must never share a table. If they did, firm vocabulary would get
//! polluted with client content, and that is very hard to unwind. This
//! table makes the mistake unrepresentable rather than merely discouraged:
//! `glossary_term` has no `project_id`, no owner, and no link to any
//! matter-scoped table, so there is no field in which a client's defined
//! term could be written — `store/tests/glossary_terms.rs` pins that
//! shape against the applied schema. A component that renders both reads
//! two sources.

use chrono::{DateTime, Utc};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

pub use model::glossary::{
    find, github_source_url, link_target, parse_entry, preamble, slugify, terms, valid_description,
    EntryError, LinkTarget, Term, GLOSSARY, GLOSSARY_LABEL, README,
};

use crate::postgres::catalog;
use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

/// The table these rows live in.
const TABLE: &str = "glossary_term";

/// One materialized glossary term row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GlossaryTerm {
    pub id: Uuid,
    /// The published anchor (`lawyer-review`) — the stable reference key.
    /// Unique.
    pub slug: String,
    /// The heading text as authored (`Lawyer Review`).
    pub title: String,
    /// The Markdown body beneath the heading.
    pub body: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as the engine reads and writes it — the seam between
/// [`GlossaryTerm`] and the SDK's own `RecordId` and `Datetime`.
#[derive(SurrealValue)]
struct GlossaryTermRow {
    id: surrealdb::types::RecordId,
    slug: String,
    title: String,
    body: String,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl GlossaryTermRow {
    /// `None` when the record id is not a native UUID key — a row
    /// written by something that bypassed [`crate::surreal::record_id`].
    fn into_term(self) -> Option<GlossaryTerm> {
        Some(GlossaryTerm {
            id: record_uuid(&self.id)?,
            slug: self.slug,
            title: self.title,
            body: self.body,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`GlossaryTermRow`] from only one query.
const SELECT: &str = "id, slug, title, body, inserted_at, updated_at";

/// Errors reading or writing a glossary term.
#[derive(Debug, thiserror::Error)]
pub enum GlossaryError {
    /// A database operation failed.
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// The write collided with `glossary_term_slug` — another row already
    /// holds this slug.
    #[error("that glossary slug is already in use")]
    SlugTaken,
    /// A write reported success but returned no row, or returned one
    /// this module could not read back — see [`GlossaryTermRow::into_term`].
    #[error("writing a glossary term returned no usable row")]
    WriteReturnedNothing,
    /// The PostgreSQL catalog authority could not serve the call.
    #[error(transparent)]
    Postgres(#[from] catalog::CatalogError),
}

impl From<sqlx::Error> for GlossaryError {
    fn from(error: sqlx::Error) -> Self {
        Self::Postgres(error.into())
    }
}

const PG_BY_SLUG: &str =
    "SELECT id, slug, title, body, inserted_at, updated_at FROM glossary_term WHERE slug = $1";
const PG_ALL: &str =
    "SELECT id, slug, title, body, inserted_at, updated_at FROM glossary_term ORDER BY slug ASC";
/// One authored term, written once and rewritten only when its text moved.
const PG_UPSERT: &str =
    "INSERT INTO glossary_term (id, slug, title, body) VALUES ($1, $2, $3, $4) \
     ON CONFLICT (slug) DO UPDATE SET title = EXCLUDED.title, body = EXCLUDED.body, \
     updated_at = now() \
     WHERE glossary_term.title <> EXCLUDED.title OR glossary_term.body <> EXCLUDED.body";

fn from_pg(row: &sqlx::postgres::PgRow) -> Result<GlossaryTerm, sqlx::Error> {
    use sqlx::Row;
    Ok(GlossaryTerm {
        id: row.try_get("id")?,
        slug: row.try_get("slug")?,
        title: row.try_get("title")?,
        body: row.try_get("body")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Turn a write failure into the caller-correctable conflict it names,
/// or leave it as a database fault. A unique violation carries **no
/// typed detail** — the index name in the message is the only
/// discriminator through the shared classifier in [`crate::surreal::retry`].
fn classify_write(error: surrealdb::Error) -> GlossaryError {
    if crate::surreal::retry::unique_violation(&error) == Some("glossary_term_slug") {
        GlossaryError::SlugTaken
    } else {
        GlossaryError::Db(error)
    }
}

/// Run a write under the shared retry policy
/// ([`crate::surreal::retry`]), mapping whatever finally comes back to
/// this module's error.
///
/// Only the mapping lives here. How long a lost race is re-run, and
/// which engine conditions count as a lost race, are one policy for the
/// whole crate.
async fn writing<F, Q>(attempt: F) -> Result<surrealdb::IndexedResults, GlossaryError>
where
    F: FnMut() -> Q,
    Q: std::future::IntoFuture<Output = Result<surrealdb::IndexedResults, surrealdb::Error>>,
{
    retry::writing(attempt).await.map_err(classify_write)
}

/// Materialize glossary terms — [`terms`] in the canonical seed — into
/// `glossary_term` rows, keyed by slug.
///
/// Idempotent by find-or-create plus update: re-running after an edit
/// updates the title and body in place rather than appending a second row
/// for the same term, so every boot converges on exactly the authored
/// vocabulary. Race-safe without a lock: a concurrent boot that wins the
/// `glossary_term_slug` unique index turns this call's insert into
/// [`GlossaryError::SlugTaken`], which is re-read as the winner's row and
/// updated in place. Returns the number of terms written.
///
/// # Errors
///
/// Propagates any database error.
pub async fn materialize(db: &SurrealDb, terms: &[Term]) -> Result<usize, GlossaryError> {
    if let Some(pg) = catalog::authority().await? {
        for t in terms {
            catalog::execute(
                sqlx::query(PG_UPSERT)
                    .bind(Uuid::now_v7())
                    .bind(t.slug.clone())
                    .bind(t.title.clone())
                    .bind(t.body.clone()),
                &pg,
            )
            .await?;
        }
        return Ok(terms.len());
    }
    for t in terms {
        match by_slug(db, &t.slug).await? {
            Some(existing) => {
                if existing.title != t.title || existing.body != t.body {
                    update(db, existing.id, t).await?;
                }
            }
            None => match create(db, t).await {
                Ok(()) => {}
                // A concurrent boot won the slug index between the check
                // and the insert; converge on the winner's row.
                Err(GlossaryError::SlugTaken) => {
                    let existing = by_slug(db, &t.slug)
                        .await?
                        .ok_or(GlossaryError::WriteReturnedNothing)?;
                    update(db, existing.id, t).await?;
                }
                Err(error) => return Err(error),
            },
        }
    }
    Ok(terms.len())
}

/// Insert one term row under a fresh v7 UUID record id.
async fn create(db: &SurrealDb, t: &Term) -> Result<(), GlossaryError> {
    let id = Uuid::now_v7();
    writing(|| {
        db.query("CREATE $id SET slug = $slug, title = $title, body = $body".to_string())
            .bind(("id", record_id(TABLE, id)))
            .bind(("slug", t.slug.clone()))
            .bind(("title", t.title.clone()))
            .bind(("body", t.body.clone()))
    })
    .await?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::GlossaryTerm(id))
        .await;
    Ok(())
}

/// Bring an existing term row up to the authored text.
async fn update(db: &SurrealDb, id: Uuid, t: &Term) -> Result<(), GlossaryError> {
    writing(|| {
        db.query(
            "UPDATE $id SET title = $title, body = $body, updated_at = time::now()".to_string(),
        )
        .bind(("id", record_id(TABLE, id)))
        .bind(("title", t.title.clone()))
        .bind(("body", t.body.clone()))
    })
    .await?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::GlossaryTerm(id))
        .await;
    Ok(())
}

/// Resolve one term by its slug — the lookup a composition performs when
/// it references a definition instead of restating it.
///
/// # Errors
///
/// Propagates any database error.
pub async fn by_slug(db: &SurrealDb, slug: &str) -> Result<Option<GlossaryTerm>, GlossaryError> {
    if let Some(pg) = catalog::authority().await? {
        return Ok(catalog::one(&pg, PG_BY_SLUG, slug.to_string(), from_pg).await?);
    }
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} WHERE slug = $slug LIMIT 1"
        ))
        .bind(("slug", slug.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<GlossaryTermRow> = response.take(0)?;
    Ok(row.and_then(GlossaryTermRow::into_term))
}

/// Every materialized term, alphabetical by slug.
///
/// # Errors
///
/// Propagates any database error.
pub async fn all(db: &SurrealDb) -> Result<Vec<GlossaryTerm>, GlossaryError> {
    if let Some(pg) = catalog::authority().await? {
        return Ok(catalog::all(&pg, PG_ALL, None::<String>, from_pg).await?);
    }
    let mut response = db
        .query(format!("SELECT {SELECT} FROM {TABLE} ORDER BY slug ASC"))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<GlossaryTermRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(GlossaryTermRow::into_term)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        find, github_source_url, parse_entry, preamble, slugify, terms, valid_description,
        EntryError, GLOSSARY, GLOSSARY_LABEL, README,
    };

    /// Every embedded entry file, as `(stem, raw)`, `README.md` excluded.
    fn entry_files() -> Vec<(String, &'static str)> {
        GLOSSARY
            .files()
            .filter_map(|file| {
                let name = file.path().file_name()?.to_str()?;
                let stem = name.strip_suffix(".md").filter(|_| name != README)?;
                Some((stem.to_string(), file.contents_utf8()?))
            })
            .collect()
    }

    #[test]
    fn entries_explain_terms_without_generated_schema_boxes() {
        for (stem, raw) in entry_files() {
            assert!(
                !raw.lines().any(|line| line.starts_with("┌─ ")),
                "{GLOSSARY_LABEL}/{stem}.md must not contain a generated table block"
            );
        }
    }

    #[test]
    fn slug_matches_the_published_anchor_shape() {
        assert_eq!(slugify("Lawyer Review"), "lawyer-review");
        assert_eq!(slugify("Engagement / Retainer"), "engagement--retainer");
        assert_eq!(slugify("`ctx.run`"), "ctxrun");
    }

    #[test]
    fn the_authored_glossary_reads_into_terms() {
        let terms = terms();
        assert!(
            terms.len() > 25,
            "expected the full authored vocabulary, got {}",
            terms.len()
        );
        assert!(terms.iter().any(|t| t.title == "Lawyer Review"));
        assert!(terms.iter().any(|t| t.title == "Template"));
        assert!(
            terms.iter().all(|t| !t.body.is_empty()),
            "every term needs a definition"
        );
        assert!(
            terms.windows(2).all(|w| w[0].slug < w[1].slug),
            "terms are alphabetical by slug, and a slug names one term"
        );
    }

    /// Every entry file parses: a bad file panics [`terms`] at boot, so
    /// this names the file instead.
    #[test]
    fn every_entry_file_is_a_well_formed_term() {
        for (stem, raw) in entry_files() {
            if let Err(error) = parse_entry(&stem, raw) {
                panic!("{GLOSSARY_LABEL}/{stem}.md: {error}");
            }
        }
    }

    #[test]
    fn an_entry_needs_a_title_that_slugs_to_its_file_name() {
        assert_eq!(
            parse_entry("x", "no frontmatter\n"),
            Err(EntryError::NoFrontmatter)
        );
        assert_eq!(
            parse_entry("x", "---\nother: 1\n---\n\nBody.\n"),
            Err(EntryError::NoTitle)
        );
        assert_eq!(
            parse_entry(
                "lawyer",
                "---\ntitle: Lawyer Review\ndescription: A lawyer reviews the work.\n---\n\nBody.\n",
            ),
            Err(EntryError::MisnamedFile {
                stem: "lawyer".to_string(),
                slug: "lawyer-review".to_string()
            })
        );
        let term = parse_entry(
            "lawyer-review",
            "---\ntitle: Lawyer Review\ndescription: A lawyer reviews the work.\n---\n\nBody.\n",
        )
        .expect("well-formed");
        assert_eq!(term.title, "Lawyer Review");
        assert_eq!(term.description, "A lawyer reviews the work.");
        assert_eq!(term.body, "Body.");
        assert!(parse_entry("lawyer-review", "---\ntitle: Lawyer Review\n---\n\nBody.\n").is_err());
        assert!(parse_entry(
            "lawyer-review",
            "---\ntitle: Lawyer Review\ndescription: \"First sentence. Second sentence.\"\n---\n\nBody.\n"
        ).is_err());
        assert!(parse_entry(
            "lawyer-review",
            "---\ntitle: Lawyer Review\ndescription: \"See [Matter](matter.md).\"\n---\n\nBody.\n"
        )
        .is_err());
    }

    #[test]
    fn find_matches_a_title_but_not_its_slug() {
        assert_eq!(
            find("lawyer review").map(|t| t.slug.as_str()),
            Some("lawyer-review")
        );
        assert_eq!(
            find("`CTX.RUN`").map(|t| t.title.as_str()),
            Some("`ctx.run`")
        );
        assert!(find("ctxrun").is_none());
        assert!(find("not a real term").is_none());
    }

    /// The assistant's chat names its own terms: `Conversation` and
    /// `Message` already mean an exchange with an external account.
    #[test]
    fn the_assistant_chat_has_its_own_terms() {
        for title in ["Assistant Chat", "Assistant Turn"] {
            let term = find(title).unwrap_or_else(|| panic!("{title} is a glossary term"));
            assert!(
                term.body.contains("conversation.md"),
                "{title} must name how it differs from a Conversation"
            );
        }
    }

    #[test]
    fn every_term_has_a_plain_one_sentence_description() {
        assert!(terms().iter().all(|term| {
            valid_description(&term.description)
                && !term.description.contains('[')
                && !term.description.contains('`')
                && !term.description.contains('*')
        }));
    }

    #[test]
    fn source_links_open_the_release_tag() {
        let version = env!("CARGO_PKG_VERSION");
        let repo = cloud::workspace::NAVIGATOR_REPOSITORY_URL;
        assert_eq!(
            github_source_url("store/src/persons.rs", None),
            format!("{repo}/blob/{version}/store/src/persons.rs")
        );
        assert_eq!(
            github_source_url("store/", None),
            format!("{repo}/tree/{version}/store")
        );
        assert_eq!(
            github_source_url("docs/notation.md", Some("template")),
            format!("{repo}/blob/{version}/docs/notation.md#template")
        );
        assert_eq!(
            github_source_url("LICENSE", None),
            format!("{repo}/blob/{version}/LICENSE")
        );
        assert!(
            !github_source_url("store/src/persons.rs", None).contains("/main/"),
            "a source link names the release tag, not main"
        );
    }

    /// A `#1093` or `ENG-20` names a pull request or a Linear issue. The
    /// release-tag source link is the citation that stays.
    #[test]
    fn entries_cite_source_not_an_issue() {
        for file in GLOSSARY.files() {
            let name = file.path().display();
            let raw = file.contents_utf8().unwrap_or("");
            if let Some(hit) = issue_citation(raw) {
                panic!("{GLOSSARY_LABEL}/{name} cites `{hit}`; link the source at the release tag");
            }
        }
    }

    fn issue_citation(text: &str) -> Option<&str> {
        let mut rest = text;
        while let Some(at) = rest.find("ENG-") {
            let tail = &rest[at + 4..];
            if tail.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
                let end = citation_end(tail);
                return Some(&rest[at..at + 4 + end]);
            }
            rest = &rest[at + 4..];
        }
        let mut rest = text;
        while let Some(at) = rest.find('#') {
            let tail = &rest[at + 1..];
            if tail.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
                let end = citation_end(tail);
                return Some(&rest[at..at + 1 + end]);
            }
            rest = &rest[at + 1..];
        }
        None
    }

    fn citation_end(tail: &str) -> usize {
        tail.char_indices()
            .find(|(_, ch)| !ch.is_ascii_digit())
            .map_or(tail.len(), |(index, _)| index)
    }

    #[test]
    fn links_resolve_from_the_glossary_directory() {
        use super::{link_target, LinkTarget};
        assert_eq!(link_target("matter.md"), LinkTarget::Term("matter".into()));
        assert_eq!(
            link_target("../../store/src/persons.rs"),
            LinkTarget::Repository {
                path: "store/src/persons.rs".into(),
                anchor: None
            }
        );
        assert_eq!(
            link_target("../../store/"),
            LinkTarget::Repository {
                path: "store/".into(),
                anchor: None
            }
        );
        assert_eq!(
            link_target("../notation.md#template"),
            LinkTarget::Repository {
                path: "docs/notation.md".into(),
                anchor: Some("template")
            }
        );
        assert_eq!(
            link_target("README.md"),
            LinkTarget::Repository {
                path: "docs/glossary/README.md".into(),
                anchor: None
            }
        );
        for verbatim in [
            "https://restate.dev",
            "mailto:a@b.c",
            "/glossary",
            "#x",
            "../../../x",
        ] {
            assert_eq!(link_target(verbatim), LinkTarget::Verbatim, "{verbatim}");
        }
    }

    #[test]
    fn every_sibling_link_names_a_real_term() {
        let slugs: Vec<&str> = terms().iter().map(|t| t.slug.as_str()).collect();
        for term in terms() {
            let mut rest = term.body.as_str();
            while let Some(at) = rest.find("](") {
                rest = &rest[at + 2..];
                let Some(close) = rest.find(')') else { break };
                if let super::LinkTarget::Term(slug) = super::link_target(&rest[..close]) {
                    assert!(
                        slugs.contains(&slug.as_str()),
                        "`{}` links `{slug}.md`, which is not a glossary term",
                        term.title
                    );
                }
            }
        }
    }

    #[test]
    fn the_preamble_is_the_readers_opening_only() {
        assert!(!preamble().is_empty());
        assert!(!preamble().starts_with('#'));
        assert!(
            !preamble().contains("## ") && !preamble().contains("To add a term"),
            "authoring notes stay in the README: {}",
            preamble()
        );
    }
}
