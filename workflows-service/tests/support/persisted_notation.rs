//! Native pinned Template/Asset/Notation fixture on the caller's owning pool.

use std::sync::Arc;
use store::{application_store::ApplicationStore, postgres::PostgresDb};
use uuid::Uuid;

#[path = "native_project.rs"]
mod native_project;

const BODY: &str = "---\nkind: letter\ntitle: Synthetic notation\nrespondent_type: person\njurisdiction: NV\nconfidential: true\nquestionnaire:\n  BEGIN:\n    _: END\n  END: {}\nworkflow:\n  BEGIN:\n    _: lawyer_review\n  lawyer_review:\n    approved: END\n  END: {}\n---\n\n# Synthetic notation\n";

pub async fn seed(pg: &PostgresDb, storage: Arc<dyn cloud::StorageService>) -> (Uuid, Uuid) {
    PostgresDb::with_application_db(pg.clone(), async {
        let source = ApplicationStore::uninitialized();
        let (parents, admin) = native_project::matter(&source).await;
        let project = store::projects::find_by_id(&source, parents.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(project.firm_id, parents.firm_id);
        assert_eq!(project.entity_id, parents.entity_id);
        assert_eq!(project.brand, parents.brand);
        assert_eq!(
            store::persons::find_by_id(&source, admin)
                .await
                .unwrap()
                .unwrap()
                .role,
            store::persons::Role::Admin
        );
        let actor = store::persons::create(
            &source,
            &store::persons::NewPerson::with_role(
                "Synthetic Client",
                format!("client-{}@example.com", Uuid::now_v7()),
                store::persons::Role::Client,
            ),
        )
        .await
        .unwrap();
        store::persons::set_email_confirmed(&source, actor.id, true)
            .await
            .unwrap();
        store::projects::designate_dri(
            &source,
            parents.id,
            actor.id,
            store::projects::DriSide::Client,
        )
        .await
        .unwrap();
        let saved = store::template_source::persist_contents(
            &source,
            &storage,
            parents.id,
            "letter__synthetic",
            BODY.to_owned(),
            "1111111111111111111111111111111111111111".to_owned(),
        )
        .await
        .unwrap();
        let notation = store::notations::create(
            &source,
            &store::notations::NewNotation::new(saved.template.id, actor.id, parents.id, "BEGIN"),
        )
        .await
        .unwrap();
        assert_eq!(notation.template_id, saved.template.id);
        assert_eq!(notation.template_body_sha256, saved.template.body_sha256);
        assert_eq!(
            notation.template_source_commit_sha,
            saved.template.source_commit_sha
        );
        let asset = store::assets::find_by_id(&source, saved.template.asset_id.unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(Some(asset.sha256_hex), notation.template_body_sha256);
        let body = store::templates::body(&source, &storage, &saved.template)
            .await
            .unwrap();
        assert_eq!(body, rules::clauses::compile(BODY).unwrap());
        (notation.id, actor.id)
    })
    .await
    .unwrap()
}
