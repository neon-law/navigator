//! Native Project parents built through canonical accessors on the caller fixture.
use store::surreal::SurrealDb;
use uuid::Uuid;

async fn entity(db: &SurrealDb) -> Uuid {
    let id = Uuid::new_v4();
    let jurisdiction = store::jurisdictions::create(
        db,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic jurisdiction {id}"),
            id.to_string(),
            "state",
        ),
    )
    .await
    .unwrap();
    let kind = store::entity_types::create(db, &format!("Synthetic kind {id}"))
        .await
        .unwrap();
    store::entities::create(
        db,
        &store::entities::NewEntity {
            name: format!("Synthetic entity {id}"),
            entity_type_id: kind.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap()
    .id
}

pub async fn matter(db: &SurrealDb) -> (store::projects::Project, Uuid) {
    let admin = store::persons::create(
        db,
        &store::persons::NewPerson::with_role(
            "Synthetic administrator",
            format!("synthetic-{}@example.com", Uuid::new_v4()),
            store::persons::Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        db,
        &store::firms::NewFirm {
            name: format!("Synthetic firm {}", Uuid::new_v4()),
            status: "active".to_owned(),
            entity_id: entity(db).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    let brand = format!("synthetic-{}", Uuid::new_v4().simple());
    store::brands::create(
        db,
        store::persons::Role::Admin,
        Some(admin.id),
        &store::brands::NewBrand {
            name: "Synthetic brand".to_owned(),
            key: brand.clone(),
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::firms::attach_brand(db, store::persons::Role::Owner, None, firm.id, &brand)
        .await
        .unwrap();
    let entity = entity(db).await;
    let project = new_project(db, firm.id, &brand, entity).await;
    (project, admin.id)
}

pub async fn new_project(
    db: &SurrealDb,
    firm: Uuid,
    brand: &str,
    entity: Uuid,
) -> store::projects::Project {
    store::projects::create(
        db,
        &store::projects::NewProject {
            code: format!("synthetic-{}", Uuid::new_v4().simple()),
            name: "Synthetic matter".to_owned(),
            status: "open".to_owned(),
            entity_id: entity,
            firm_id: Some(firm),
            brand: brand.to_owned(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
