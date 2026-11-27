//! The firm glossary materializes through the PostgreSQL catalog authority:
//! every read and write here resolves to the disposable fixture database,
//! and the SurrealDB handle stays uninitialized because the authority path
//! never touches it. The engine-shape tests — what the Surreal
//! `glossary_term` table may hold — remain in `store/tests/glossary_terms.rs`.

use super::{all, by_slug, materialize, terms, Term};
use crate::application_store::ApplicationStore;
use crate::postgres::catalog;
use crate::postgres::fixtures::{connect, fresh_schema};
use crate::postgres::PostgresDb;

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

/// The canonical seed materializes the authored vocabulary, and running it
/// again converges instead of appending — an edited definition updates its
/// row rather than creating a rival one under the same slug.
#[tokio::test]
async fn materialize_is_idempotent_and_updates_in_place() {
    let Some(pg) = database().await else {
        return;
    };
    let application_store = ApplicationStore::uninitialized();
    catalog::with_test_db(pg, async move {
        let written = materialize(&application_store, terms())
            .await
            .expect("first materialize");
        assert!(written > 25, "expected the full vocabulary, got {written}");
        let after_first = all(&application_store).await.expect("all").len();
        assert_eq!(after_first, written);

        materialize(&application_store, terms())
            .await
            .expect("second materialize");
        assert_eq!(
            all(&application_store).await.expect("all").len(),
            after_first,
            "a re-run must converge, not append a second row per term"
        );

        // An edited definition updates the existing row.
        let edited = Term {
            slug: "lawyer-review".to_string(),
            title: "Lawyer Review".to_string(),
            description: "A lawyer reviews the work.".to_string(),
            body: "Rewritten body.".to_string(),
        };
        materialize(&application_store, &[edited])
            .await
            .expect("edit");
        let row = by_slug(&application_store, "lawyer-review")
            .await
            .expect("lookup")
            .expect("lawyer-review row");
        assert_eq!(row.body, "Rewritten body.");
        assert_eq!(
            all(&application_store).await.expect("all").len(),
            after_first,
            "editing a term must not create a rival row under the same slug"
        );
    })
    .await;
}

/// The reference a composition performs: a dashboard section, notation
/// template, or questionnaire prompt names a term by slug instead of
/// restating the definition, and that slug resolves.
#[tokio::test]
async fn a_term_referenced_by_slug_resolves_to_its_definition() {
    let Some(pg) = database().await else {
        return;
    };
    let application_store = ApplicationStore::uninitialized();
    catalog::with_test_db(pg, async move {
        materialize(&application_store, terms())
            .await
            .expect("materialize");

        let referenced = by_slug(&application_store, "lawyer-review")
            .await
            .expect("lookup")
            .expect("a composition referencing `lawyer-review` must resolve");
        assert_eq!(referenced.title, "Lawyer Review");
        assert!(
            !referenced.body.trim().is_empty(),
            "a resolved reference must carry the definition, not just a title"
        );

        // #894 also adds Template, absent until now despite #850 existing to
        // document its four-part anatomy.
        let template = by_slug(&application_store, "template")
            .await
            .expect("lookup")
            .expect("Template must be a real term");
        assert!(
            template.body.contains("not a literal nested YAML key"),
            "the metadata caveat is the load-bearing half of the Template entry"
        );

        assert!(
            by_slug(&application_store, "not-a-real-term")
                .await
                .expect("lookup")
                .is_none(),
            "an unknown slug resolves to nothing rather than a wrong definition"
        );
    })
    .await;
}
