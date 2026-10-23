use super::*;
use crate::authorities::{AuthorityLookup, AuthorityPatch, NewAuthority};
use crate::glossary::Term;
use crate::notation_packages::NewNotationPackage;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::questions::NewQuestion;
use crate::test_support::mem_surreal;

async fn mirrored(surreal: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) -> Mirrored {
    super::super::mirror(surreal, pg, key).await.unwrap()
}

fn rows(report: &ParityReport) -> Vec<usize> {
    report
        .tables
        .iter()
        .map(|table| table.target_rows)
        .collect()
}

fn new_authority(citation: &str) -> NewAuthority<'_> {
    NewAuthority {
        class: rules::citation::AuthorityClass::CaseLaw,
        citation,
        short_cite: None,
        title: "Synthetic v. Fixture",
        publisher: None,
        issued_on: Some("2026-01-02"),
        canonical_url: Some("https://example.com/synthetic"),
        checked_on: None,
        archived_asset_id: None,
    }
}

fn term(slug: &str, body: &str) -> Term {
    Term {
        slug: slug.into(),
        title: "Synthetic Term".into(),
        description: "A synthetic term.".into(),
        body: body.into(),
    }
}

async fn rate(surreal: &SurrealDb, code: &str) -> Uuid {
    let id = Uuid::now_v7();
    surreal
        .query(
            "CREATE $id SET code = $code, label = 'Synthetic rate', unit = 'day', \
             unit_amount_cents = 1000",
        )
        .bind(("id", record_id("notation_rate", id)))
        .bind(("code", code.to_string()))
        .await
        .unwrap()
        .check()
        .unwrap();
    id
}

async fn remove(surreal: &SurrealDb, table: &str, id: Uuid) {
    surreal
        .query("DELETE $id")
        .bind(("id", record_id(table, id)))
        .await
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn catalog_keys_name_rows_without_values() {
    let id = Uuid::nil();
    for (key, table) in [
        (MirrorKey::Authority(id), "authority"),
        (MirrorKey::GlossaryTerm(id), "glossary_term"),
        (MirrorKey::Question(id), "question"),
        (MirrorKey::NotationPackage(id), "notation_package"),
        (
            MirrorKey::NotationPackageTemplate(id),
            "notation_package_template",
        ),
        (MirrorKey::NotationRate(id), "notation_rate"),
    ] {
        assert_eq!(key.table(), table);
        assert_eq!(key.to_string(), format!("{table}:{id}"));
    }
}

#[test]
fn every_catalog_table_is_mirrored_after_the_tables_it_may_reference() {
    let names: Vec<_> = super::super::MIRRORS
        .iter()
        .map(|mirror| mirror.name)
        .collect();
    let position = |name: &str| names.iter().position(|mirror| *mirror == name).unwrap();
    // An Authority names its archived asset; a Notation and an answer name
    // a Notation Package and a Question.
    assert!(position("catalog") > position("documents"));
    assert!(position("catalog") < position("workflow"));
    assert!(AuthorityImage::upsert_statement().contains("archived_asset_id = EXCLUDED"));
    assert_eq!(
        PackageTemplateImage::delete_statement(),
        "DELETE FROM notation_package_template WHERE id = $1"
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one lifecycle covers each shared catalog and its retained parents"
)]
async fn catalog_rows_converge_through_their_lifecycle() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;

    // Authority: created, replayed, then patched.
    let authority = crate::authorities::record(&surreal, &new_authority("1 Synth. 1 (2026)"))
        .await
        .unwrap();
    let key = MirrorKey::Authority(authority.id);
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Unchanged);
    crate::authorities::update(
        &surreal,
        AuthorityLookup::Id(authority.id),
        &AuthorityPatch {
            short_cite: Some("Synthetic"),
            ..AuthorityPatch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);

    // Glossary Term: materialized, then edited in place.
    crate::glossary::materialize(&surreal, &[term("synthetic-term", "First.")])
        .await
        .unwrap();
    let glossary = crate::glossary::by_slug(&surreal, "synthetic-term")
        .await
        .unwrap()
        .unwrap();
    let key = MirrorKey::GlossaryTerm(glossary.id);
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);
    crate::glossary::materialize(&surreal, &[term("synthetic-term", "Second.")])
        .await
        .unwrap();
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Unchanged);

    // Question: created, then narrowed to the lawyer.
    let question = crate::questions::create(
        &surreal,
        &NewQuestion::new("synthetic_question", "Synthetic?", "string"),
    )
    .await
    .unwrap();
    let key = MirrorKey::Question(question.id);
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);
    crate::questions::set_audience(&surreal, "synthetic_question", "lawyer")
        .await
        .unwrap();
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);
    let audience: String = sqlx::query_scalar("SELECT audience FROM question WHERE id = $1")
        .bind(question.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(audience, "lawyer");

    // A package template brings its package with it.
    let package = crate::notation_packages::create(
        &surreal,
        &NewNotationPackage::new("synthetic-package", "Synthetic package", "simple", 10_000),
    )
    .await
    .unwrap();
    let template = crate::templates::save_version(
        &surreal,
        None,
        "onboarding__synthetic_catalog",
        crate::templates::Version {
            title: "Synthetic Draft".into(),
            respondent_type: "person".into(),
            asset_id: None,
            form_code: None,
            kind: None,
            source_commit_sha: None,
        },
    )
    .await
    .unwrap()
    .into_model()
    .id;
    crate::notation_packages::add_template(&surreal, package.id, template, 0, true)
        .await
        .unwrap();
    let mut response = surreal
        .query("SELECT VALUE id FROM notation_package_template")
        .await
        .unwrap();
    let members: Vec<RecordId> = response.take(0).unwrap();
    let member = record_uuid(&members[0]).unwrap();
    let member_key = MirrorKey::NotationPackageTemplate(member);
    assert_eq!(
        mirrored(&surreal, &pg, &member_key).await,
        Mirrored::Written
    );
    assert_eq!(
        mirrored(&surreal, &pg, &MirrorKey::NotationPackage(package.id)).await,
        Mirrored::Unchanged
    );

    let pinned: Uuid =
        sqlx::query_scalar("SELECT template_id FROM notation_package_template WHERE id = $1")
            .bind(member)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(pinned, template);
    let error = sqlx::query("DELETE FROM template WHERE id = $1")
        .bind(template)
        .execute(&pg.0)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );

    // Notation rate: no Surreal writer, so a raw row stands in.
    let rate = rate(&surreal, "synthetic_rate").await;
    let rate_key = MirrorKey::NotationRate(rate);
    assert_eq!(mirrored(&surreal, &pg, &rate_key).await, Mirrored::Written);

    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(rows(&report), [1, 1, 1, 1, 1, 1]);

    // Removal: an absent source row is deleted, and a replay is a no-op.
    remove(&surreal, "notation_package_template", member).await;
    assert_eq!(
        mirrored(&surreal, &pg, &member_key).await,
        Mirrored::Deleted
    );
    remove(&surreal, "notation_rate", rate).await;
    assert_eq!(mirrored(&surreal, &pg, &rate_key).await, Mirrored::Deleted);
    assert_eq!(
        mirrored(&surreal, &pg, &rate_key).await,
        Mirrored::Unchanged
    );
    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(rows(&report), [1, 1, 1, 1, 0, 0]);
}

#[tokio::test]
async fn parity_names_divergent_catalog_keys_and_reconcile_repairs_them() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;

    let authority = crate::authorities::record(&surreal, &new_authority("2 Synth. 2 (2026)"))
        .await
        .unwrap();
    crate::glossary::materialize(&surreal, &[term("synthetic-reconciled", "Body.")])
        .await
        .unwrap();
    let glossary = crate::glossary::by_slug(&surreal, "synthetic-reconciled")
        .await
        .unwrap()
        .unwrap();
    let question = crate::questions::create(
        &surreal,
        &NewQuestion::new("synthetic_reconciled", "Synthetic?", "string"),
    )
    .await
    .unwrap();
    let rate = rate(&surreal, "synthetic_reconciled_rate").await;
    for key in [
        MirrorKey::Authority(authority.id),
        MirrorKey::Question(question.id),
    ] {
        mirrored(&surreal, &pg, &key).await;
    }

    // PostgreSQL drifts: a differing Authority, a stale Question SurrealDB
    // never held, and a stale Glossary Term holding the live slug under
    // another id. The live term and the rate never arrived.
    sqlx::query("UPDATE authority SET title = 'Drifted' WHERE id = $1")
        .bind(authority.id)
        .execute(&pg.0)
        .await
        .unwrap();
    let stale_question = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO question (id, code, prompt, answer_type) \
         VALUES ($1, 'synthetic_stale', 'Stale?', 'string')",
    )
    .bind(stale_question)
    .execute(&pg.0)
    .await
    .unwrap();
    let stale_term = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO glossary_term (id, slug, title, body) \
         VALUES ($1, 'synthetic-reconciled', 'Stale', 'Stale.')",
    )
    .bind(stale_term)
    .execute(&pg.0)
    .await
    .unwrap();

    // The live term collides with the stale slug until reconcile runs.
    let refused = super::super::mirror(&surreal, &pg, &MirrorKey::GlossaryTerm(glossary.id))
        .await
        .unwrap_err();
    assert!(matches!(refused, MirrorError::Target(ref code) if code == "23505"));

    let report = parity(&surreal, &pg).await.unwrap();
    let mut divergent: Vec<_> = report.divergent().cloned().collect();
    divergent.sort();
    let mut expected = vec![
        MirrorKey::Authority(authority.id),
        MirrorKey::GlossaryTerm(glossary.id),
        MirrorKey::GlossaryTerm(stale_term),
        MirrorKey::Question(stale_question),
        MirrorKey::NotationRate(rate),
    ];
    expected.sort();
    assert_eq!(divergent, expected);
    // The report carries keys and counts, never a title or a body.
    assert!(!format!("{report:?}").contains("Drifted"));

    let repaired = reconcile(&surreal, &pg).await.unwrap();
    assert!(repaired.is_clean(), "{repaired:?}");
    assert_eq!(rows(&repaired), [1, 1, 1, 0, 0, 1]);
    let replay = reconcile(&surreal, &pg).await.unwrap();
    assert_eq!(replay, repaired);
    let title: String = sqlx::query_scalar("SELECT title FROM authority WHERE id = $1")
        .bind(authority.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(title, "Synthetic v. Fixture");
}

#[tokio::test]
async fn the_canonical_seed_mirrors_once_and_a_reseed_changes_nothing() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
        cloud::FsStorage::new(
            std::env::temp_dir().join(format!("catalog-mirror-{}", Uuid::now_v7().simple())),
        )
        .await
        .unwrap(),
    );
    crate::seed::seed_canonical(&surreal, &storage)
        .await
        .unwrap();

    let seeded = reconcile(&surreal, &pg).await.unwrap();
    assert!(seeded.is_clean(), "{seeded:?}");
    let glossary_terms = seeded.tables[1].target_rows;
    let questions = seeded.tables[2].target_rows;
    assert!(glossary_terms > 0 && questions > 0, "{seeded:?}");

    // A second boot rewrites no catalog row, so PostgreSQL needs nothing.
    crate::seed::seed_canonical(&surreal, &storage)
        .await
        .unwrap();
    assert_eq!(parity(&surreal, &pg).await.unwrap(), seeded);
}

#[tokio::test]
async fn an_authority_naming_an_asset_postgres_lacks_is_refused_and_reported() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let authority = crate::authorities::record(
        &surreal,
        &NewAuthority {
            archived_asset_id: Some(Uuid::now_v7()),
            ..new_authority("3 Synth. 3 (2026)")
        },
    )
    .await
    .unwrap();
    let key = MirrorKey::Authority(authority.id);
    let error = super::super::mirror(&surreal, &pg, &key).await.unwrap_err();
    assert!(matches!(error, MirrorError::Target(ref code) if code == "23503"));
    let report = reconcile(&surreal, &pg).await.unwrap();
    assert_eq!(report.tables[0].divergent, [key]);
    assert_eq!(report.tables[0].target_rows, 0);
}
