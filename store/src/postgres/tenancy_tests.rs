use super::tenancy;
use crate::brands::{BrandEdit, BrandError, NewBrand};
use crate::firms::{FirmError, FirmMembership, NewFirm, NewPersonFirmRole};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use uuid::Uuid;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one synthetic Firm lifecycle scenario verifies atomic create, membership transfer, and Brand rules"
)]
async fn firm_creation_is_atomic_in_postgres_and_preserves_the_admin_dri_membership() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let jurisdiction = Uuid::new_v4();
    let entity_type = Uuid::new_v4();
    let entity = Uuid::new_v4();
    let person = Uuid::new_v4();
    let next_admin = Uuid::new_v4();
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
    sqlx::query("INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Practice Entity',$2,$3)")
        .bind(entity)
        .bind(entity_type)
        .bind(jurisdiction)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,'Synthetic Admin','synthetic-admin@example.com','admin')")
        .bind(person)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,'Synthetic Next Admin','synthetic-next-admin@example.com','admin')")
        .bind(next_admin)
        .execute(pg.pool())
        .await
        .unwrap();

    let input = NewFirm {
        name: "Synthetic Practice".into(),
        status: "active".into(),
        entity_id: entity,
        admin_dri_person_id: person,
    };
    let created = tenancy::create_firm_on(&pg, &input).await.unwrap();
    assert_eq!(
        tenancy::find_firm_on(&pg, created.id).await.unwrap(),
        Some(created.clone())
    );
    let member: (Uuid, Uuid, String, bool) = sqlx::query_as(
        "SELECT person_id,firm_id,membership,is_dri FROM person_firm_role WHERE person_id=$1 AND firm_id=$2",
    )
    .bind(person)
    .bind(created.id)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(member, (person, created.id, "admin".into(), true));

    let added = tenancy::add_membership_on(
        &pg,
        &NewPersonFirmRole {
            person_id: next_admin,
            firm_id: created.id,
            membership: FirmMembership::Admin,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    assert!(!added.is_dri);
    assert!(matches!(
        tenancy::add_membership_on(
            &pg,
            &NewPersonFirmRole {
                person_id: next_admin,
                firm_id: created.id,
                membership: FirmMembership::Admin,
                is_dri: false,
            },
        )
        .await,
        Err(FirmError::DuplicateMembership)
    ));
    let transferred = tenancy::appoint_admin_dri_on(&pg, created.id, next_admin)
        .await
        .unwrap();
    assert!(transferred.is_dri);
    let prior_dri: bool =
        sqlx::query_scalar("SELECT is_dri FROM person_firm_role WHERE person_id=$1 AND firm_id=$2")
            .bind(person)
            .bind(created.id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert!(!prior_dri, "the previous Admin DRI is demoted atomically");

    let brand = tenancy::create_brand_on(
        &pg,
        crate::persons::Role::Admin,
        next_admin,
        &NewBrand {
            name: "Synthetic Practice Brand".into(),
            key: format!("synthetic-{}", Uuid::new_v4()),
            firm_id: Some(created.id),
            ..NewBrand::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(brand.firm_id, Some(created.id));
    let invalid_font = sqlx::query(
        "INSERT INTO brand (name,brand_key,font_licence) VALUES ($1,$2,'not-a-license')",
    )
    .bind(format!("Invalid Font Brand {}", Uuid::new_v4()))
    .bind(format!("invalid-font-{}", Uuid::new_v4()))
    .execute(pg.pool())
    .await;
    assert!(
        invalid_font.is_err(),
        "the PostgreSQL constraint rejects invalid font licences from direct SQL"
    );
    assert_eq!(
        tenancy::find_brand_on(&pg, brand.id)
            .await
            .unwrap()
            .unwrap(),
        brand
    );
    let edited = tenancy::update_brand_on(
        &pg,
        crate::persons::Role::Admin,
        Some(next_admin),
        brand.id,
        &BrandEdit {
            name: Some("Synthetic Practice Brand Updated".into()),
            ..BrandEdit::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(edited.name, "Synthetic Practice Brand Updated");
    assert!(matches!(
        tenancy::update_brand_on(
            &pg,
            crate::persons::Role::Admin,
            Some(person),
            brand.id,
            &BrandEdit {
                name: Some("Unauthorized Rename".into()),
                ..BrandEdit::default()
            },
        )
        .await,
        Err(BrandError::NotAuthorized)
    ));
    assert!(matches!(
        tenancy::update_brand_on(
            &pg,
            crate::persons::Role::Client,
            Some(next_admin),
            brand.id,
            &BrandEdit {
                name: Some("Client Must Not Rename".into()),
                ..BrandEdit::default()
            },
        )
        .await,
        Err(BrandError::NotAuthorized)
    ));
    assert!(matches!(
        tenancy::create_brand_on(
            &pg,
            crate::persons::Role::Client,
            next_admin,
            &NewBrand {
                name: "Client Cannot Create".into(),
                key: format!("client-{}", Uuid::new_v4()),
                firm_id: Some(created.id),
                ..NewBrand::default()
            },
        )
        .await,
        Err(BrandError::NotAuthorized)
    ));
    tenancy::delete_brand_on(&pg, crate::persons::Role::Admin, Some(next_admin), brand.id)
        .await
        .unwrap();
    assert!(tenancy::find_brand_on(&pg, brand.id)
        .await
        .unwrap()
        .is_none());

    let duplicate_entity = NewFirm {
        name: "Another Synthetic Practice".into(),
        ..input
    };
    assert!(matches!(
        tenancy::create_firm_on(&pg, &duplicate_entity).await,
        Err(FirmError::DuplicateEntity)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM firm")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "refused duplicate leaves no partial Firm");

    let first_entity = Uuid::new_v4();
    let second_entity = Uuid::new_v4();
    sqlx::query("INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Concurrent Synthetic Entity One',$2,$3),($4,'Concurrent Synthetic Entity Two',$2,$3)")
        .bind(first_entity)
        .bind(entity_type)
        .bind(jurisdiction)
        .bind(second_entity)
        .execute(pg.pool())
        .await
        .unwrap();
    let first = NewFirm {
        name: "Concurrent Synthetic Practice".into(),
        status: "active".into(),
        entity_id: first_entity,
        admin_dri_person_id: person,
    };
    let second = NewFirm {
        entity_id: second_entity,
        admin_dri_person_id: next_admin,
        ..first.clone()
    };
    let (first_result, second_result) = tokio::join!(
        tenancy::create_firm_on(&pg, &first),
        tenancy::create_firm_on(&pg, &second),
    );
    assert_eq!(
        i32::from(first_result.is_ok()) + i32::from(second_result.is_ok()),
        1,
        "concurrent slug claim has exactly one winner"
    );
    let ((Err(loser), Ok(_)) | (Ok(_), Err(loser))) = (first_result, second_result) else {
        panic!("the concurrent slug claims must have one winner and one loser");
    };
    assert!(matches!(loser, FirmError::DuplicateSlug(_)));
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one synthetic PostgreSQL lifecycle test verifies Firm, membership, Brand listing, and deletion guards"
)]
async fn postgres_tenancy_crud_lists_and_guards_preserve_scope() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let jurisdiction = Uuid::new_v4();
    let entity_type = Uuid::new_v4();
    let entity = Uuid::new_v4();
    let admin = Uuid::new_v4();
    let lawyer = Uuid::new_v4();
    let client = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jurisdiction (id,name) VALUES ($1,'Lifecycle Synthetic Jurisdiction')",
    )
    .bind(jurisdiction)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Lifecycle Synthetic Type')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Lifecycle Synthetic Entity',$2,$3)")
        .bind(entity).bind(entity_type).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,'Lifecycle Admin',$2,'admin'),($3,'Lifecycle Lawyer',$4,'lawyer'),($5,'Lifecycle Client',$6,'client')")
        .bind(admin)
        .bind(format!("lifecycle-admin-{}@example.test", admin.simple()))
        .bind(lawyer)
        .bind(format!("lifecycle-lawyer-{}@example.test", lawyer.simple()))
        .bind(client)
        .bind(format!("lifecycle-client-{}@example.test", client.simple()))
        .execute(pg.pool())
        .await
        .unwrap();
    let firm = tenancy::create_firm_on(
        &pg,
        &NewFirm {
            name: "Lifecycle Synthetic Firm".into(),
            status: "active".into(),
            entity_id: entity,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap();
    let firm_brand_input = NewBrand {
        name: "Lifecycle Synthetic Brand".into(),
        key: format!("lifecycle-{}", Uuid::new_v4().simple()),
        firm_id: Some(firm.id),
        ..NewBrand::default()
    };
    let firm_brand =
        tenancy::create_brand(crate::persons::Role::Admin, Some(admin), &firm_brand_input);
    let firm_brand = tenancy::with_test_db(pg.clone(), firm_brand).await.unwrap();
    let system_brand_key = format!("system-{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO brand (name,brand_key) VALUES ('Lifecycle System Brand',$1)")
        .bind(&system_brand_key)
        .execute(pg.pool())
        .await
        .unwrap();

    tenancy::with_test_db(pg.clone(), async {
        assert_eq!(tenancy::all_firms().await.unwrap().len(), 1);
        assert_eq!(
            tenancy::firm_by_entity(entity).await.unwrap().unwrap().id,
            firm.id
        );
        let admin_memberships = tenancy::memberships_for_person(admin).await.unwrap();
        assert_eq!(admin_memberships.len(), 1);
        assert_eq!(
            tenancy::memberships_for_firm(firm.id).await.unwrap().len(),
            1
        );
        assert_eq!(tenancy::all_memberships().await.unwrap().len(), 1);
        assert!(tenancy::membership(admin, firm.id).await.unwrap().is_some());

        let added = tenancy::add_membership(&NewPersonFirmRole {
            person_id: lawyer,
            firm_id: firm.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        })
        .await
        .unwrap();
        assert_eq!(added.membership, FirmMembership::Lawyer);
        assert!(matches!(
            tenancy::add_membership(&NewPersonFirmRole {
                person_id: client,
                firm_id: firm.id,
                membership: FirmMembership::Lawyer,
                is_dri: false,
            })
            .await,
            Err(FirmError::ClientCannotJoinFirm(_))
        ));
        assert!(matches!(
            tenancy::update_membership(
                crate::persons::Role::Admin,
                Some(admin),
                admin,
                firm.id,
                FirmMembership::Lawyer,
            )
            .await,
            Err(FirmError::WouldLeaveFirmWithoutAdminDri(_))
        ));
        let updated_member = tenancy::update_membership(
            crate::persons::Role::Admin,
            Some(admin),
            lawyer,
            firm.id,
            FirmMembership::Clerk,
        )
        .await
        .unwrap();
        assert_eq!(updated_member.membership, FirmMembership::Clerk);
        tenancy::remove_membership(crate::persons::Role::Admin, Some(admin), lawyer, firm.id)
            .await
            .unwrap();

        let updated = tenancy::update_firm(
            crate::persons::Role::Admin,
            Some(admin),
            firm.id,
            &crate::firms::FirmEdit {
                name: Some("Lifecycle Renamed Firm".into()),
                ..crate::firms::FirmEdit::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(updated.name, "Lifecycle Renamed Firm");
        assert!(matches!(
            tenancy::update_firm(
                crate::persons::Role::Admin,
                Some(admin),
                firm.id,
                &crate::firms::FirmEdit {
                    entity_id: Some(Uuid::new_v4()),
                    ..crate::firms::FirmEdit::default()
                },
            )
            .await,
            Err(FirmError::NoSuchEntity(_))
        ));

        tenancy::attach_brand(firm.id, &firm_brand.key)
            .await
            .unwrap();
        assert_eq!(
            tenancy::firm_brand_keys(firm.id).await.unwrap(),
            vec![firm_brand.key.clone()]
        );
        assert_eq!(
            tenancy::firm_for_brand_key(&firm_brand.key).await.unwrap(),
            Some(firm.id)
        );
        assert_eq!(tenancy::all_firm_brand_rows().await.unwrap().len(), 1);
        assert!(tenancy::firm_brand_row_for_key(&firm_brand.key)
            .await
            .unwrap()
            .is_some());
        assert!(matches!(
            tenancy::delete_firm(crate::persons::Role::Admin, Some(admin), firm.id,).await,
            Err(FirmError::FirmOwnsBrands(_))
        ));
        assert_eq!(tenancy::system_brands().await.unwrap().len(), 1);
        assert_eq!(tenancy::all_firm_brands().await.unwrap().len(), 1);
        assert_eq!(tenancy::brands_for_firm(firm.id).await.unwrap().len(), 1);
        assert_eq!(
            tenancy::visible_brands_for_actor(crate::persons::Role::Admin, Some(admin))
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(tenancy::brand_for_actor(
            crate::persons::Role::Client,
            Some(client),
            &firm_brand.key,
        )
        .await
        .is_err());
        tenancy::detach_brand(firm.id, &firm_brand.key)
            .await
            .unwrap();
        assert!(tenancy::firm_brand_keys(firm.id).await.unwrap().is_empty());
        assert!(matches!(
            tenancy::delete_brand(crate::persons::Role::Admin, Some(admin), firm_brand.id,).await,
            Ok(())
        ));
        assert!(tenancy::find_brand(firm_brand.id).await.unwrap().is_none());
        assert_eq!(
            tenancy::brand_by_key(&system_brand_key)
                .await
                .unwrap()
                .unwrap()
                .name,
            "Lifecycle System Brand"
        );
        tenancy::delete_firm(crate::persons::Role::Admin, Some(admin), firm.id)
            .await
            .unwrap();
        assert!(tenancy::find_firm_on(&pg, firm.id).await.unwrap().is_none());
    })
    .await;
}
