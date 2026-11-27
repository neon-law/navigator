//! Project Drive-folder addresses are persisted by the admitted PostgreSQL authority.

#[path = "support/native_project.rs"]
mod native_project;

use store::projects::{create, set_drive_folder_id, ProjectStoreError};

#[tokio::test]
async fn projects_schema_rejects_a_duplicate_matter_code() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        native_project::create("drive-unique").await;

        assert!(matches!(
            create(
                &application_store,
                &native_project::input("drive-unique").await,
            )
            .await,
            Err(ProjectStoreError::CodeTaken)
        ));
    })
    .await;
}

#[tokio::test]
async fn a_folder_address_is_written_on_the_authoritative_project_record() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let matter = native_project::create("drive-address").await;

        let updated = set_drive_folder_id(&application_store, matter.id, Some("1QaBcD_2-efG"))
            .await
            .expect("set address")
            .expect("matter exists");
        assert_eq!(updated.drive_folder_id.as_deref(), Some("1QaBcD_2-efG"));
    })
    .await;
}
