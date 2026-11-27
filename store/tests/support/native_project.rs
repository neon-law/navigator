//! Valid native Project references shared by the PostgreSQL integration tests.
use store::application_store::ApplicationStore;
use store::persons::Role;
use store::projects::{NewProject, Project};
use uuid::Uuid;

/// Create only the references an ordinary Project insert requires.
pub async fn input(code: &str) -> NewProject {
    store::postgres::PostgresDb::application()
        .await
        .expect("admitted PostgreSQL fixture");
    let db = ApplicationStore::uninitialized();
    let unique = Uuid::new_v4();
    let jurisdiction = store::jurisdictions::create(
        &db,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic jurisdiction {unique}"),
            unique.to_string(),
            "state",
        ),
    )
    .await
    .unwrap();
    let kind = store::entity_types::create(&db, &format!("Synthetic kind {unique}"))
        .await
        .unwrap();
    let entity_input = store::entities::NewEntity {
        name: format!("Synthetic entity {unique}"),
        entity_type_id: kind.id,
        jurisdiction_id: jurisdiction.id,
        phone: None,
        url: None,
        xero_id: None,
        firm_anchor_key: None,
    };
    let firm_entity = store::entities::create(&db, &entity_input).await.unwrap();
    let client_entity = store::entities::create(
        &db,
        &store::entities::NewEntity {
            name: format!("Synthetic client {unique}"),
            ..entity_input
        },
    )
    .await
    .unwrap();
    let admin = store::persons::create(
        &db,
        &store::persons::NewPerson::with_role(
            "Synthetic administrator",
            format!("synthetic-{unique}@example.com"),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        &db,
        &store::firms::NewFirm {
            name: format!("Synthetic firm {unique}"),
            status: "active".into(),
            entity_id: firm_entity.id,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    let brand = format!("synthetic-{unique}");
    store::brands::create(
        &db,
        Role::Admin,
        Some(admin.id),
        &store::brands::NewBrand {
            name: format!("Synthetic brand {unique}"),
            key: brand.clone(),
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::firms::attach_brand(&db, Role::Owner, None, firm.id, &brand)
        .await
        .unwrap();
    NewProject {
        code: code.into(),
        name: code.into(),
        status: "open".into(),
        entity_id: client_entity.id,
        firm_id: Some(firm.id),
        brand,
        ..Default::default()
    }
}

/// Insert through the same admitted domain accessor as the application.
pub async fn create(code: &str) -> Project {
    store::projects::create(&ApplicationStore::uninitialized(), &input(code).await)
        .await
        .unwrap()
}
