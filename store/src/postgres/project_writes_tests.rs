//! Project writes that commit in PostgreSQL first: sparse column updates,
//! the Firm-scoped Brand change, delete, archive restore, and matter open.

use super::projects::{self as records, Column, OpenMatter, ProjectError};
use super::projects_tests::{fixture, insert_person, Fixture};
use super::PostgresDb;
use crate::projects::NewProject;
use uuid::Uuid;

async fn wear(pg: &PostgresDb, firm: Uuid, brand: &str) {
    sqlx::query("INSERT INTO brand (name,brand_key) VALUES ($1,$1) ON CONFLICT DO NOTHING")
        .bind(brand)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO firm_brand (firm_id,brand_key) VALUES ($1,$2)")
        .bind(firm)
        .bind(brand)
        .execute(pg.pool())
        .await
        .unwrap();
}

/// A second Firm, on an Entity of its own shaped like `entity`.
async fn other_firm(pg: &PostgresDb, like_entity: Uuid) -> Uuid {
    let (firm, entity) = (Uuid::new_v4(), Uuid::new_v4());
    sqlx::query("INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) SELECT $1,'Other Entity',entity_type_id,jurisdiction_id FROM entity WHERE id=$2")
        .bind(entity)
        .bind(like_entity)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO firm (id,name,slug,status,entity_id) VALUES ($1,'Other Firm','other-firm','active',$2)")
        .bind(firm)
        .bind(entity)
        .execute(pg.pool())
        .await
        .unwrap();
    firm
}

#[tokio::test]
async fn a_sparse_update_writes_only_the_named_columns() {
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let created = records::create_on(&pg, &input).await.unwrap();
    let assigned = "2026-11-02".parse().unwrap();
    let updated = records::update_on(
        &pg,
        created.id,
        &[
            Column::Text("drive_folder_id", Some("folder-1")),
            Column::Text("description", None),
            Column::Id("jurisdiction_id", input.jurisdiction_id),
            Column::Date("plan_assigned_on", Some(assigned)),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(updated.drive_folder_id.as_deref(), Some("folder-1"));
    assert_eq!(updated.description, None);
    assert_eq!(updated.plan_assigned_on.as_deref(), Some("2026-11-02"));
    assert_eq!(updated.name, created.name);
    assert_eq!(updated.repository_url, created.repository_url);
    assert!(updated.updated_at > created.updated_at);
    assert_eq!(
        records::find_by_id_on(&pg, created.id).await.unwrap(),
        Some(updated)
    );

    assert!(records::update_on(&pg, Uuid::new_v4(), &[], None)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        records::update_on(
            &pg,
            created.id,
            &[Column::Id("jurisdiction_id", Some(Uuid::new_v4()))],
            None,
        )
        .await,
        Err(ProjectError::NoSuchJurisdiction)
    ));
}

#[tokio::test]
async fn a_brand_change_within_the_firm_refuses_a_brand_another_firm_wears() {
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let firm = input.firm_id.unwrap();
    let created = records::create_on(&pg, &input).await.unwrap();
    wear(&pg, firm, "second-brand").await;
    let stranger = other_firm(&pg, input.entity_id).await;
    wear(&pg, stranger, "stranger-brand").await;

    let moved = records::update_on(
        &pg,
        created.id,
        &[Column::Text("brand", Some("second-brand"))],
        Some((firm, Some("second-brand"))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(moved.brand, "second-brand");

    for (within, brand) in [(firm, "stranger-brand"), (stranger, "stranger-brand")] {
        assert!(records::update_on(
            &pg,
            created.id,
            &[Column::Text("brand", Some(brand))],
            Some((within, Some(brand))),
        )
        .await
        .unwrap()
        .is_none());
    }
    assert_eq!(
        records::find_by_id_on(&pg, created.id)
            .await
            .unwrap()
            .unwrap()
            .brand,
        "second-brand"
    );
}

#[tokio::test]
async fn opening_a_matter_writes_the_project_and_both_dri_rows_together() {
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let firm = input.firm_id.unwrap();
    wear(&pg, firm, &input.brand).await;
    let lawyer = insert_person(&pg, "lawyer").await;
    let client = insert_person(&pg, "client").await;
    let open = |project| OpenMatter {
        id: None,
        project,
        closed_at: Some("2026-11-01T00:00:00Z"),
        lawyer,
        lawyer_participation: "lawyer",
        client,
    };

    let opened = records::open_on(&pg, &open(&input)).await.unwrap();
    assert_eq!(opened.code, input.code);
    assert_eq!(opened.firm_id, Some(firm));
    assert_eq!(opened.closed_at.as_deref(), Some("2026-11-01T00:00:00Z"));
    let roles = super::participation::for_project_on(&pg, opened.id)
        .await
        .unwrap();
    let lawyer_row = roles.iter().find(|row| row.person_id == lawyer).unwrap();
    assert_eq!(lawyer_row.participation, "lawyer");
    assert!(lawyer_row.is_lawyer_dri && !lawyer_row.is_client_dri);
    let client_row = roles.iter().find(|row| row.person_id == client).unwrap();
    assert_eq!(client_row.participation, "client");
    assert!(client_row.is_client_dri && !client_row.is_lawyer_dri);
    assert_eq!(roles.len(), 2);

    assert!(matches!(
        records::open_on(&pg, &open(&input)).await,
        Err(ProjectError::CodeTaken)
    ));
    let stranger = other_firm(&pg, input.entity_id).await;
    let elsewhere = NewProject {
        code: "elsewhere".into(),
        firm_id: Some(stranger),
        ..input.clone()
    };
    assert!(matches!(
        records::open_on(&pg, &open(&elsewhere)).await,
        Err(ProjectError::FirmBrandChanged)
    ));
    assert!(records::find_by_code_on(&pg, "elsewhere")
        .await
        .unwrap()
        .is_none());
}
