use super::*;
use crate::authorities::{self, AuthorityLookup, AuthorityPatch, NewAuthority};
use crate::glossary::{self, Term};
use crate::jurisdictions::{self, JurisdictionError, NewJurisdiction};
use crate::notation_packages::{self, NewNotationPackage};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::postgres_mirror::{self, MirrorKey, Mirrored};
use crate::questions::{self, NewQuestion, QuestionError};
use crate::test_support::mem_surreal;
use crate::{entity_types, entity_types::EntityTypeError};
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one synthetic scenario covers both reference tables and the mirror guard"
)]
async fn reference_tables_are_written_and_read_in_postgres_only() {
    let Some(pg) = database().await else { return };
    let surreal = mem_surreal().await;
    let nevada = NewJurisdiction::new("Synthetic State", "SY", "state");
    let (jurisdiction, entity_type) = with_test_db(pg.clone(), async {
        let created = jurisdictions::find_or_create(&surreal, &nevada)
            .await
            .unwrap();
        assert_eq!(
            jurisdictions::find_or_create(&surreal, &nevada)
                .await
                .unwrap(),
            created
        );
        assert!(matches!(
            jurisdictions::create(
                &surreal,
                &NewJurisdiction::new("Synthetic State", "SZ", "state")
            )
            .await,
            Err(JurisdictionError::NameTaken)
        ));
        assert!(matches!(
            jurisdictions::create(&surreal, &NewJurisdiction::new("Typo", "TY", "province")).await,
            Err(JurisdictionError::Postgres(_))
        ));
        jurisdictions::create(
            &surreal,
            &NewJurisdiction::new("Synthetic Realm", "SR", "country"),
        )
        .await
        .unwrap();
        assert_eq!(
            jurisdictions::find_by_name(&surreal, " Synthetic State ")
                .await
                .unwrap(),
            Some(created.clone())
        );
        assert_eq!(
            jurisdictions::find_by_id(&surreal, created.id)
                .await
                .unwrap(),
            Some(created.clone())
        );
        let names = |rows: Vec<jurisdictions::Jurisdiction>| {
            rows.into_iter().map(|row| row.code).collect::<Vec<_>>()
        };
        assert_eq!(
            names(jurisdictions::list_all(&surreal).await.unwrap()),
            ["SR", "SY"]
        );
        assert_eq!(
            names(
                jurisdictions::list_by_type(&surreal, "country")
                    .await
                    .unwrap()
            ),
            ["SR"]
        );

        let llc = entity_types::find_or_create(&surreal, "Synthetic LLC")
            .await
            .unwrap();
        assert_eq!(
            entity_types::find_or_create(&surreal, "Synthetic LLC")
                .await
                .unwrap(),
            llc
        );
        assert!(matches!(
            entity_types::create(&surreal, "Synthetic LLC").await,
            Err(EntityTypeError::NameTaken)
        ));
        entity_types::create(&surreal, "Synthetic Trust")
            .await
            .unwrap();
        assert_eq!(
            entity_types::find_by_id(&surreal, llc.id).await.unwrap(),
            Some(llc.clone())
        );
        let listed = entity_types::list(&surreal, &[("name".into(), true)])
            .await
            .unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["Synthetic Trust", "Synthetic LLC"]
        );
        (created, llc)
    })
    .await;

    assert!(jurisdictions::list_all(&surreal).await.unwrap().is_empty());
    assert!(entity_types::list(&surreal, &[]).await.unwrap().is_empty());
    with_test_db(pg.clone(), async {
        for key in [
            MirrorKey::Jurisdiction(jurisdiction.id),
            MirrorKey::EntityType(entity_type.id),
        ] {
            assert_eq!(
                postgres_mirror::mirror(&surreal, &pg, &key).await.unwrap(),
                Mirrored::Unchanged,
                "an absent SurrealDB row must not delete the authoritative one"
            );
        }
    })
    .await;
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM jurisdiction")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(held, 2);
}

fn term(slug: &str, body: &str) -> Term {
    Term {
        slug: slug.into(),
        title: "Synthetic Term".into(),
        description: "A synthetic term.".into(),
        body: body.into(),
    }
}

#[tokio::test]
async fn glossary_and_questions_converge_in_postgres() {
    let Some(pg) = database().await else { return };
    let surreal = mem_surreal().await;
    with_test_db(pg.clone(), async {
        glossary::materialize(&surreal, &[term("synthetic", "First.")])
            .await
            .unwrap();
        let first = glossary::by_slug(&surreal, "synthetic")
            .await
            .unwrap()
            .unwrap();
        glossary::materialize(&surreal, &[term("synthetic", "First.")])
            .await
            .unwrap();
        assert_eq!(
            glossary::all(&surreal).await.unwrap(),
            std::slice::from_ref(&first)
        );
        glossary::materialize(&surreal, &[term("synthetic", "Second.")])
            .await
            .unwrap();
        let second = glossary::by_slug(&surreal, "synthetic")
            .await
            .unwrap()
            .unwrap();
        assert_eq!((second.id, second.body.as_str()), (first.id, "Second."));

        let new = NewQuestion::new("synthetic_code", "Synthetic prompt?", "string");
        let question = questions::find_or_create(&surreal, &new).await.unwrap();
        assert_eq!(
            questions::find_or_create(&surreal, &new).await.unwrap(),
            question
        );
        assert!(matches!(
            questions::create(&surreal, &new).await,
            Err(QuestionError::CodeTaken)
        ));
        let other = questions::create(
            &surreal,
            &NewQuestion::new("another_code", "Another?", "bool")
                .with_audience(questions::AUDIENCE_LAWYER),
        )
        .await
        .unwrap();
        let narrowed = questions::set_audience(&surreal, "synthetic_code", "client")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (narrowed.id, narrowed.audience.as_str()),
            (question.id, "client")
        );
        assert!(questions::set_audience(&surreal, "absent", "client")
            .await
            .unwrap()
            .is_none());
        let codes = |rows: Vec<questions::Question>| {
            rows.into_iter().map(|row| row.code).collect::<Vec<_>>()
        };
        assert_eq!(
            codes(
                questions::find_by_ids(&surreal, &[question.id, other.id])
                    .await
                    .unwrap()
            ),
            ["another_code", "synthetic_code"]
        );
        assert_eq!(
            codes(
                questions::find_by_codes(&surreal, &["synthetic_code".into()])
                    .await
                    .unwrap()
            ),
            ["synthetic_code"]
        );
        assert_eq!(questions::list_all(&surreal).await.unwrap().len(), 2);
        assert_eq!(
            questions::find_by_id(&surreal, other.id).await.unwrap(),
            Some(other)
        );
        let report = postgres_mirror::catalog::parity(&surreal, &pg)
            .await
            .unwrap();
        assert!(
            report.is_clean(),
            "only a SurrealDB row PostgreSQL lacks is parity evidence"
        );
    })
    .await;
    assert!(glossary::all(&surreal).await.unwrap().is_empty());
    assert!(questions::list_all(&surreal).await.unwrap().is_empty());
}

#[tokio::test]
async fn authorities_and_packages_live_in_postgres() {
    let Some(pg) = database().await else { return };
    let surreal = mem_surreal().await;
    let template = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO template (id, code, title, respondent_type, is_current, inserted_at, \
         updated_at) VALUES ($1, 'synthetic', 'Synthetic', 'person', true, now(), now())",
    )
    .bind(template)
    .execute(pg.pool())
    .await
    .unwrap();
    with_test_db(pg.clone(), async {
        let new = NewAuthority {
            class: rules::citation::AuthorityClass::CaseLaw,
            citation: "1 Synthetic 1",
            short_cite: None,
            title: "Synthetic v. Fixture",
            publisher: None,
            issued_on: Some("2026-01-02"),
            canonical_url: None,
            checked_on: None,
            archived_asset_id: None,
        };
        let authority = authorities::record(&surreal, &new).await.unwrap();
        assert_eq!(
            authorities::record(&surreal, &new).await.unwrap(),
            authority
        );
        let updated = authorities::update(
            &surreal,
            AuthorityLookup::Citation("1 Synthetic 1"),
            &AuthorityPatch {
                checked_on: Some("2026-02-03"),
                ..AuthorityPatch::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(updated.checked_on.as_deref(), Some("2026-02-03"));
        assert_eq!(updated.issued_on.as_deref(), Some("2026-01-02"));
        assert_eq!(
            authorities::find(&surreal, AuthorityLookup::Id(authority.id))
                .await
                .unwrap(),
            Some(updated)
        );

        let package = notation_packages::create(
            &surreal,
            &NewNotationPackage::new("synthetic-package", "Synthetic package", "simple", 10_000),
        )
        .await
        .unwrap();
        assert!(package.active);
        notation_packages::add_template(&surreal, package.id, template, 1, true)
            .await
            .unwrap();
        assert!(matches!(
            notation_packages::add_template(&surreal, package.id, Uuid::now_v7(), 2, true).await,
            Err(notation_packages::NotationPackageError::Postgres(_))
        ));
        assert_eq!(
            notation_packages::find_by_code(&surreal, "synthetic-package")
                .await
                .unwrap(),
            Some(package.clone())
        );
        let members = notation_packages::templates(&surreal, package.id)
            .await
            .unwrap();
        assert_eq!(
            members
                .iter()
                .map(|member| (member.template_id, member.position))
                .collect::<Vec<_>>(),
            [(template, 1)]
        );
    })
    .await;
    assert!(
        authorities::find(&surreal, AuthorityLookup::Citation("1 Synthetic 1"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        notation_packages::find_by_code(&surreal, "synthetic-package")
            .await
            .unwrap()
            .is_none()
    );
}
