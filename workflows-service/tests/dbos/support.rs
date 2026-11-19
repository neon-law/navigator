#[path = "../support/native_database.rs"]
mod native_database;
pub use native_database::{database, TestDatabase, APP_VERSION, DEADLINE};
use uuid::Uuid;

/// A synthetic Firm, its admin, and one open Project on it.
pub struct FirmMatter {
    pub admin: Uuid,
    pub firm: Uuid,
    pub project: Uuid,
}

/// Seed a [`FirmMatter`] in `surreal`.
pub async fn firm_matter(surreal: &store::surreal::SurrealDb, label: &str) -> FirmMatter {
    use store::persons::{NewPerson, Role};
    let admin = store::persons::create(
        surreal,
        &NewPerson::with_role(
            format!("{label} Admin"),
            format!(
                "{}-admin-{}@example.com",
                label.to_lowercase(),
                Uuid::now_v7()
            ),
            Role::Admin,
        ),
    )
    .await
    .expect("seed the Firm admin");
    let firm = store::firms::create(
        surreal,
        &store::firms::NewFirm {
            name: format!("{label} Synthetic Firm"),
            status: "active".into(),
            entity_id: store::test_support::seed_entity(surreal).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .expect("seed the Firm");
    let project = store::projects::create(
        surreal,
        &store::projects::NewProject {
            code: format!(
                "{}-{}",
                label.to_lowercase(),
                &Uuid::now_v7().simple().to_string()[..8]
            ),
            name: format!("Synthetic {label} Matter"),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(surreal).await,
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .expect("seed the Project");
    FirmMatter {
        admin: admin.id,
        firm: firm.id,
        project: project.id,
    }
}
