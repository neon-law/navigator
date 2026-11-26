use super::*;
use crate::persons::Role;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::projects::{self, NewProject};

struct Fixture {
    pg: crate::postgres::PostgresDb,
    owner: Uuid,
    admin: Uuid,
    lawyer: Uuid,
    entity: Uuid,
    other_entity: Uuid,
}

async fn fixture() -> Option<Fixture> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let mut jurisdictions = Vec::new();
    for (name, code) in [
        ("Synthetic Jurisdiction", "SYN"),
        ("Second Jurisdiction", "SEC"),
    ] {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO jurisdiction (id,name,code) VALUES ($1,$2,$3)")
            .bind(id)
            .bind(name)
            .bind(code)
            .execute(pg.pool())
            .await
            .unwrap();
        jurisdictions.push(id);
    }
    let entity_type = Uuid::new_v4();
    sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Entity Type')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    let mut entities = Vec::new();
    for (jurisdiction_id, name) in jurisdictions
        .into_iter()
        .zip(["Synthetic Entity", "Second Entity"])
    {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(name)
        .bind(entity_type)
        .bind(jurisdiction_id)
        .execute(pg.pool())
        .await
        .unwrap();
        entities.push(id);
    }
    let people = [
        (Role::Owner, "Synthetic Owner"),
        (Role::Admin, "Synthetic Admin"),
        (Role::Lawyer, "Synthetic Lawyer"),
    ]
    .map(|(role, name)| (Uuid::new_v4(), role, name));
    for (id, role, name) in people {
        sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,$2,$3,$4)")
            .bind(id)
            .bind(name)
            .bind(format!("{}@example.test", id.simple()))
            .bind(role.as_str())
            .execute(pg.pool())
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO brand (name,brand_key) VALUES ('Synthetic Neon','neon')")
        .execute(pg.pool())
        .await
        .unwrap();
    Some(Fixture {
        pg,
        owner: people[0].0,
        admin: people[1].0,
        lawyer: people[2].0,
        entity: entities[0],
        other_entity: entities[1],
    })
}

#[tokio::test]
async fn an_owner_may_be_the_admin_dri() {
    let Some(fixture) = fixture().await else {
        return;
    };
    crate::postgres::tenancy::with_test_authority(fixture.pg.clone(), true, async {
        let store = crate::test_support::uninitialized_store();
        let opened = create(
            &store,
            &NewFirm {
                name: "Owner-Opened Practice".into(),
                status: "active".into(),
                entity_id: fixture.entity,
                admin_dri_person_id: fixture.owner,
            },
        )
        .await
        .unwrap();
        let row = membership_for_person(&store, fixture.owner, opened.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.membership, FirmMembership::Admin);
        assert!(row.is_dri);

        let firm = create(
            &store,
            &NewFirm {
                name: "Owner-Appointed Practice".into(),
                status: "active".into(),
                entity_id: fixture.other_entity,
                admin_dri_person_id: fixture.admin,
            },
        )
        .await
        .unwrap();
        assert!(membership_for_person(&store, fixture.owner, firm.id)
            .await
            .unwrap()
            .is_none());
        let appointed = appoint_admin_dri(
            &store,
            Role::Owner,
            Some(fixture.owner),
            firm.id,
            fixture.owner,
        )
        .await
        .unwrap();
        assert_eq!(appointed.membership, FirmMembership::Admin);
        assert!(appointed.is_dri);
        let dris: Vec<Uuid> = memberships_for_firm(&store, firm.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| row.is_dri)
            .map(|row| row.person_id)
            .collect();
        assert_eq!(
            dris,
            vec![fixture.owner],
            "the outgoing Admin DRI is cleared"
        );
    })
    .await;
    fixture.pg.pool().close().await;
}

#[tokio::test]
async fn membership_refuses_a_dangling_person_or_firm() {
    let Some(fixture) = fixture().await else {
        return;
    };
    crate::postgres::tenancy::with_test_authority(fixture.pg.clone(), true, async {
        let store = crate::test_support::uninitialized_store();
        let firm = create(
            &store,
            &NewFirm {
                name: "Practice Two".into(),
                status: "active".into(),
                entity_id: fixture.entity,
                admin_dri_person_id: fixture.admin,
            },
        )
        .await
        .unwrap();
        let missing_person = Uuid::now_v7();
        let error = add_membership(
            &store,
            &NewPersonFirmRole {
                person_id: missing_person,
                firm_id: firm.id,
                membership: FirmMembership::Clerk,
                is_dri: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, FirmError::NoSuchPerson(id) if id == missing_person));

        let missing_firm = Uuid::now_v7();
        let error = add_membership(
            &store,
            &NewPersonFirmRole {
                person_id: fixture.lawyer,
                firm_id: missing_firm,
                membership: FirmMembership::Clerk,
                is_dri: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, FirmError::NoSuchFirm(id) if id == missing_firm));
    })
    .await;
    fixture.pg.pool().close().await;
}

#[tokio::test]
async fn project_firm_id_round_trips() {
    let Some(fixture) = fixture().await else {
        return;
    };
    crate::postgres::tenancy::with_test_authority(fixture.pg.clone(), true, async {
        let store = crate::test_support::uninitialized_store();
        let firm = create(
            &store,
            &NewFirm {
                name: "Practice Three".into(),
                status: "active".into(),
                entity_id: fixture.entity,
                admin_dri_person_id: fixture.admin,
            },
        )
        .await
        .unwrap();
        let created = projects::create(
            &store,
            &NewProject {
                code: "owned-matter".to_string(),
                name: "Owned Matter".to_string(),
                status: "open".to_string(),
                entity_id: fixture.entity,
                firm_id: Some(firm.id),
                ..NewProject::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(created.firm_id, Some(firm.id));
        let reloaded = crate::postgres::projects::find_by_id_on(&fixture.pg, created.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.firm_id, Some(firm.id));
    })
    .await;
    fixture.pg.pool().close().await;
}

#[tokio::test]
async fn create_refuses_a_dangling_firm_id() {
    let Some(fixture) = fixture().await else {
        return;
    };
    crate::postgres::tenancy::with_test_authority(fixture.pg.clone(), true, async {
        let store = crate::test_support::uninitialized_store();
        let missing = Uuid::now_v7();
        let error = projects::create(
            &store,
            &NewProject {
                code: "orphan-matter".to_string(),
                name: "Orphan Matter".to_string(),
                status: "open".to_string(),
                entity_id: fixture.entity,
                firm_id: Some(missing),
                ..NewProject::default()
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            projects::ProjectStoreError::NoSuchFirm(id) if id == missing
        ));
    })
    .await;
    fixture.pg.pool().close().await;
}

#[tokio::test]
async fn create_requires_a_live_entity_and_refuses_a_second_firm_on_it() {
    let Some(fixture) = fixture().await else {
        return;
    };
    crate::postgres::tenancy::with_test_authority(fixture.pg.clone(), true, async {
        let store = crate::test_support::uninitialized_store();
        let missing = Uuid::now_v7();
        let error = create(
            &store,
            &NewFirm {
                name: "Ghost Practice".into(),
                status: "active".into(),
                entity_id: missing,
                admin_dri_person_id: fixture.admin,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, FirmError::NoSuchEntity(id) if id == missing));

        create(
            &store,
            &NewFirm {
                name: "First".into(),
                status: "active".into(),
                entity_id: fixture.entity,
                admin_dri_person_id: fixture.admin,
            },
        )
        .await
        .unwrap();
        let duplicate = create(
            &store,
            &NewFirm {
                name: "Second".into(),
                status: "active".into(),
                entity_id: fixture.entity,
                admin_dri_person_id: fixture.admin,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(duplicate, FirmError::DuplicateEntity));
    })
    .await;
    fixture.pg.pool().close().await;
}
