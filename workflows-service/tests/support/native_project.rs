//! Native Project parents built through canonical accessors on the caller fixture.
use store::application_store::ApplicationStore;
use store::test_support::seed_entity_with_parents as entity;
use uuid::Uuid;

pub async fn matter(db: &ApplicationStore) -> (store::projects::Project, Uuid) {
    let brand = format!("synthetic-{}", Uuid::new_v4().simple());
    let (firm, admin) = store::test_support::seed_firm_with_new_brand(db, &brand).await;
    let entity = entity(db).await;
    let project = new_project(db, firm, &brand, entity).await;
    (project, admin)
}

pub async fn new_project(
    db: &ApplicationStore,
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
