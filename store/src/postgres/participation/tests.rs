use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one ledger lifecycle proves concurrent uniqueness and DRI preservation"
)]
async fn participation_is_derived_unique_and_retains_both_dri_markers() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let jurisdiction = Uuid::new_v4();
    let entity_type = Uuid::new_v4();
    let entity = Uuid::new_v4();
    let project = Uuid::new_v4();
    let person = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Synthetic Jurisdiction','TST')",
    )
    .bind(jurisdiction)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Entity')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Entity',$2,$3)")
        .bind(entity).bind(entity_type).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO project (id,code,name,status,entity_id) VALUES ($1,'synthetic-ledger','Synthetic Project','open',$2)")
        .bind(project).bind(entity).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,'Synthetic Person','ledger@example.com','lawyer')")
        .bind(person).execute(pg.pool()).await.unwrap();

    let (first, second) = tokio::join!(add_on(&pg, project, person), add_on(&pg, project, person));
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "only one concurrent insertion succeeds"
    );
    assert!(matches!(
        (&first, &second),
        (Ok(_), Err(ParticipationError::Duplicate)) | (Err(ParticipationError::Duplicate), Ok(_))
    ));
    let row = first.or(second).unwrap();
    assert_eq!(row.participation, "lawyer");
    assert!(!row.is_lawyer_dri && !row.is_client_dri);
    assert_eq!(
        for_person_on(&pg, person, project).await.unwrap(),
        Some(row.clone())
    );
    assert_eq!(
        for_project_on(&pg, project).await.unwrap(),
        vec![row.clone()]
    );
    assert_eq!(
        for_person_projects_on(&pg, person).await.unwrap(),
        vec![row.clone()]
    );

    sqlx::query("UPDATE person_project_role SET participation='counterparty' WHERE id=$1")
        .bind(row.id)
        .execute(pg.pool())
        .await
        .unwrap();
    let (lawyer, client) = tokio::join!(
        designate_dri_on(&pg, project, person, crate::projects::DriSide::Lawyer),
        designate_dri_on(&pg, project, person, crate::projects::DriSide::Client)
    );
    lawyer.unwrap();
    client.unwrap();
    let flagged = for_person_on(&pg, person, project).await.unwrap().unwrap();
    assert_eq!(flagged.id, row.id);
    assert_eq!(
        flagged.participation, "counterparty",
        "DRI designation preserves imported client-side scope"
    );
    assert_eq!(flagged.inserted_at, row.inserted_at);
    assert!(flagged.is_lawyer_dri && flagged.is_client_dri);
    clear_dri_on(&pg, row.id, crate::projects::DriSide::Client)
        .await
        .unwrap();
    let cleared = for_person_on(&pg, person, project).await.unwrap().unwrap();
    assert!(cleared.is_lawyer_dri && !cleared.is_client_dri);
    let replacement = Uuid::new_v4();
    sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,'Synthetic Client','ledger-client@example.com','client')")
        .bind(replacement).execute(pg.pool()).await.unwrap();
    let updated = update_on(&pg, row.id, replacement).await.unwrap().unwrap();
    assert_eq!(updated.participation, "client");
    assert_eq!(updated.inserted_at, row.inserted_at);
    assert!(updated.is_lawyer_dri && !updated.is_client_dri);
    assert!(for_person_on(&pg, person, project).await.unwrap().is_none());
    assert_eq!(
        for_person_on(&pg, replacement, project).await.unwrap(),
        Some(updated)
    );
    assert!(update_on(&pg, Uuid::new_v4(), replacement)
        .await
        .unwrap()
        .is_none());
    remove_on(&pg, row.id).await.unwrap();
    remove_on(&pg, row.id).await.unwrap();
    assert!(for_person_on(&pg, replacement, project)
        .await
        .unwrap()
        .is_none());
    let recreated = designate_dri_on(&pg, project, person, DriSide::Lawyer)
        .await
        .unwrap();
    assert_eq!(recreated.participation, "lawyer");
    assert!(recreated.is_lawyer_dri && !recreated.is_client_dri);
    clear_dri_on(&pg, recreated.id, DriSide::Lawyer)
        .await
        .unwrap();
    assert!(
        !for_person_on(&pg, person, project)
            .await
            .unwrap()
            .unwrap()
            .is_lawyer_dri
    );
    assert!(matches!(
        add_on(&pg, project, Uuid::new_v4()).await,
        Err(ParticipationError::NoSuchPerson)
    ));
    assert!(matches!(
        add_on(&pg, Uuid::new_v4(), person).await,
        Err(ParticipationError::NoSuchProject)
    ));
    assert!(matches!(
        update_on(&pg, row.id, Uuid::new_v4()).await,
        Err(ParticipationError::NoSuchPerson)
    ));
    sqlx::query("UPDATE person SET role='invalid' WHERE id=$1")
        .bind(person)
        .execute(pg.pool())
        .await
        .unwrap();
    assert!(matches!(
        add_on(&pg, project, person).await,
        Err(ParticipationError::InvalidRole)
    ));
    assert!(matches!(
        update_on(&pg, recreated.id, person).await,
        Err(ParticipationError::InvalidRole)
    ));
}

#[tokio::test]
async fn unavailable_postgres_returns_errors_without_any_legacy_connection() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(25))
        .connect_lazy("postgresql://postgres:synthetic@127.0.0.1:1/smoke")
        .unwrap();
    pool.close().await;
    let pg = PostgresDb(pool);
    let id = Uuid::new_v4();
    assert!(for_person_on(&pg, id, id).await.is_err());
    assert!(for_project_on(&pg, id).await.is_err());
    assert!(for_person_projects_on(&pg, id).await.is_err());
    assert!(add_on(&pg, id, id).await.is_err());
    assert!(designate_dri_on(&pg, id, id, DriSide::Lawyer)
        .await
        .is_err());
    assert!(clear_dri_on(&pg, id, DriSide::Client).await.is_err());
    assert!(update_on(&pg, id, id).await.is_err());
    assert!(remove_on(&pg, id).await.is_err());
}
