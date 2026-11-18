use super::{fixture, insert_person, Fixture};
use crate::projects::{NewProject, Project};
use uuid::Uuid;

async fn surreal_project(surreal: &crate::surreal::SurrealDb, id: Uuid) -> Option<Project> {
    crate::projects::find_by_id_in_surreal(surreal, id)
        .await
        .unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one Project lifecycle under tenancy authority walks every routed read, write and access seam"
)]
async fn tenancy_authority_routes_projects_participation_and_access_through_postgres() {
    use crate::persons::Role;
    use crate::projects::{self, DriSide, ProjectStoreError};
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let surreal = crate::test_support::mem_surreal().await;
    let lawyer = insert_person(&pg, "lawyer").await;
    let client = insert_person(&pg, "client").await;
    let stranger = insert_person(&pg, "lawyer").await;

    crate::postgres::tenancy::with_test_authority(
        pg.clone(),
        true,
        Box::pin(async {
            // Creation commits in PostgreSQL and projects into SurrealDB.
            let created = projects::create(&surreal, &input).await.unwrap();
            assert_eq!(created.id.get_version_num(), 4);
            assert_eq!(
                crate::postgres::projects::find_by_id_on(&pg, created.id)
                    .await
                    .unwrap(),
                Some(created.clone())
            );
            assert_eq!(
                surreal_project(&surreal, created.id).await,
                Some(created.clone())
            );
            assert!(matches!(
                projects::create(&surreal, &input).await,
                Err(ProjectStoreError::CodeTaken)
            ));
            assert!(matches!(
                projects::create(
                    &surreal,
                    &NewProject {
                        code: "unknown-brand".into(),
                        brand: "absent-brand".into(),
                        ..input.clone()
                    }
                )
                .await,
                Err(ProjectStoreError::UnknownBrand(_))
            ));

            // Reads answer from PostgreSQL even when SurrealDB has drifted.
            surreal
                .query("UPDATE $id SET name = 'Drifted'")
                .bind(("id", crate::surreal::record_id("project", created.id)))
                .await
                .unwrap();
            assert_eq!(
                projects::find_by_id(&surreal, created.id).await.unwrap(),
                Some(created.clone())
            );
            assert_eq!(
                projects::find_by_code(&surreal, &input.code).await.unwrap(),
                Some(created.clone())
            );
            assert_eq!(
                projects::find_by_name(&surreal, &input.name).await.unwrap(),
                Some(created.clone())
            );
            assert_eq!(
                projects::all(&surreal).await.unwrap(),
                vec![created.clone()]
            );
            let adopted = projects::find_or_create_by_code(&surreal, Uuid::new_v4(), &input)
                .await
                .unwrap();
            assert_eq!(adopted.id, created.id);

            // Participation is written in PostgreSQL, labelled from the Person's
            // tier, and projected into SurrealDB.
            let lawyer_row = projects::add_participation(&surreal, created.id, lawyer, "ignored")
                .await
                .unwrap();
            assert_eq!(lawyer_row.participation, "lawyer");
            assert_eq!(
                projects::participation_by_id_in_surreal(&surreal, lawyer_row.id)
                    .await
                    .unwrap(),
                Some(lawyer_row.clone())
            );
            assert!(matches!(
                projects::add_participation(&surreal, created.id, lawyer, "lawyer").await,
                Err(ProjectStoreError::DuplicateParticipation)
            ));
            assert!(matches!(
                projects::add_participation(&surreal, Uuid::new_v4(), lawyer, "lawyer").await,
                Err(ProjectStoreError::NoSuchProject(_))
            ));
            projects::designate_dri_in_surreal(&surreal, created.id, client, DriSide::Client)
                .await
                .unwrap();
            let client_row = projects::participation_for_person(&surreal, client, created.id)
                .await
                .unwrap()
                .unwrap();
            assert!(client_row.is_client_dri);
            assert_eq!(
                projects::participation_by_id_in_surreal(&surreal, client_row.id)
                    .await
                    .unwrap(),
                Some(client_row.clone())
            );
            assert_eq!(
                projects::participations_for_project(&surreal, created.id)
                    .await
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                projects::all_participations(&surreal).await.unwrap().len(),
                2
            );
            assert_eq!(
                projects::participations_for_person(&surreal, lawyer)
                    .await
                    .unwrap(),
                vec![lawyer_row.clone()]
            );

            // Access decisions read the PostgreSQL ledger.
            assert!(crate::access::can_see_project_as_lawyer(
                &surreal,
                Some(lawyer),
                Role::Lawyer,
                created.id
            )
            .await
            .unwrap());
            assert!(!crate::access::can_see_project_as_lawyer(
                &surreal,
                Some(stranger),
                Role::Lawyer,
                created.id
            )
            .await
            .unwrap());
            assert!(
                crate::access::can_see_project_as_client(&surreal, Some(client), created.id)
                    .await
                    .unwrap()
            );
            assert!(
                !crate::access::can_see_project_as_client(&surreal, Some(lawyer), created.id)
                    .await
                    .unwrap()
            );

            projects::clear_dri_in_surreal(&surreal, client_row.id, DriSide::Client)
                .await
                .unwrap();
            assert!(
                !projects::participation_by_id_in_surreal(&surreal, client_row.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_client_dri
            );
            let moved = projects::update_participation(&surreal, client_row.id, stranger, "client")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (moved.person_id, moved.participation.as_str()),
                (stranger, "lawyer")
            );
            projects::remove_participation(&surreal, moved.id)
                .await
                .unwrap();
            projects::remove_participation(&surreal, lawyer_row.id)
                .await
                .unwrap();
            assert!(projects::all_participations(&surreal)
                .await
                .unwrap()
                .is_empty());
            assert!(
                projects::participation_by_id_in_surreal(&surreal, lawyer_row.id)
                    .await
                    .unwrap()
                    .is_none()
            );

            // A write still committed through SurrealDB (repairing the drifted
            // copy first) is not complete until PostgreSQL holds it.
            surreal
                .query("UPDATE $id SET name = $name")
                .bind(("id", crate::surreal::record_id("project", created.id)))
                .bind(("name", created.name.clone()))
                .await
                .unwrap();
            let foldered =
                projects::set_drive_folder_id(&surreal, created.id, Some("synthetic-folder"))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(
                foldered.drive_folder_id.as_deref(),
                Some("synthetic-folder")
            );
            assert_eq!(
                crate::postgres::projects::find_by_id_on(&pg, created.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .drive_folder_id
                    .as_deref(),
                Some("synthetic-folder")
            );

            // Folder ownership is readable even when the SurrealDB copy is unavailable.
            let unavailable = crate::surreal::SurrealDb::uninitialized();
            assert!(projects::drive_folder_recorded_elsewhere(
                &unavailable,
                Uuid::nil(),
                "synthetic-folder"
            )
            .await
            .unwrap());
            assert!(!projects::drive_folder_recorded_elsewhere(
                &unavailable,
                created.id,
                "synthetic-folder"
            )
            .await
            .unwrap());

            let provisioned = projects::record_provisioned_repository(
                &surreal,
                created.id,
                "https://forge.example/synthetic/project.git",
                "2026-11-13T00:00:00Z",
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                surreal_project(&surreal, created.id)
                    .await
                    .unwrap()
                    .repository_url,
                provisioned.repository_url
            );

            projects::delete_project_with_surreal(&surreal, created.id)
                .await
                .unwrap();
            assert!(crate::postgres::projects::find_by_id_on(&pg, created.id)
                .await
                .unwrap()
                .is_none());
        }),
    )
    .await;

    // With the switch off, the legacy path neither reads nor writes PostgreSQL.
    crate::postgres::tenancy::with_test_authority(
        pg.clone(),
        false,
        Box::pin(async {
            let legacy = projects::create(
                &surreal,
                &NewProject {
                    code: "legacy-project".into(),
                    name: "Legacy Project".into(),
                    status: "open".into(),
                    entity_id: input.entity_id,
                    ..NewProject::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(legacy.id.get_version_num(), 7);
            assert!(crate::postgres::projects::find_by_id_on(&pg, legacy.id)
                .await
                .unwrap()
                .is_none());
            assert_eq!(
                projects::find_by_id(&surreal, legacy.id).await.unwrap(),
                Some(legacy)
            );
        }),
    )
    .await;
}
