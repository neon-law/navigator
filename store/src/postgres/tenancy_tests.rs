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

struct TenancySeed {
    entity: Uuid,
    second_entity: Uuid,
    dri: Uuid,
    plain_admin: Uuid,
    lawyer: Uuid,
    client: Uuid,
    second_admin: Uuid,
}

/// Seed one entity plus people, and return their ids. Roles are written
/// directly so each wrapper's authorization branch can be driven precisely.
async fn seed_entity_and_people(pg: &crate::postgres::PostgresDb) -> TenancySeed {
    let entity = Uuid::new_v4();
    let second_entity = Uuid::new_v4();
    let entity_type = Uuid::new_v4();
    let jurisdiction = Uuid::new_v4();
    sqlx::query("INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Wrapper Jurisdiction','WRP')")
        .bind(jurisdiction)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Wrapper Entity')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    for (id, name) in [
        (entity, "Wrapper Practice Entity"),
        (second_entity, "Wrapper Second Entity"),
    ] {
        sqlx::query(
            "INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(name)
        .bind(entity_type)
        .bind(jurisdiction)
        .execute(pg.pool())
        .await
        .unwrap();
    }
    let pool = pg.pool().clone();
    let insert = |id: Uuid, name: String, role: String| {
        let pool = pool.clone();
        async move {
            sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,$2,$3,$4)")
                .bind(id)
                .bind(format!("{name} <{name}@wrapper.example>"))
                .bind(format!("{name}@wrapper.example"))
                .bind(role)
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    let dri = Uuid::new_v4();
    let plain_admin = Uuid::new_v4();
    let lawyer = Uuid::new_v4();
    let client = Uuid::new_v4();
    let second_admin = Uuid::new_v4();
    insert(dri, "Wrapper Dri".into(), "owner".into()).await;
    insert(plain_admin, "Wrapper Admin".into(), "admin".into()).await;
    insert(lawyer, "Wrapper Lawyer".into(), "lawyer".into()).await;
    insert(client, "Wrapper Client".into(), "client".into()).await;
    insert(second_admin, "Wrapper Second Admin".into(), "admin".into()).await;
    TenancySeed {
        entity,
        second_entity,
        dri,
        plain_admin,
        lawyer,
        client,
        second_admin,
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario drives the firm and membership wrapper seams and every guard branch"
)]
async fn firm_membership_wrappers_authorize_and_guard_tenancy() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let seed = seed_entity_and_people(&pg).await;

    // Creation guards run before anything touches the firm table.
    let ghost_entity = tenancy::create_firm_on(
        &pg,
        &NewFirm {
            name: "Ghost Entity Practice".into(),
            status: "active".into(),
            entity_id: Uuid::new_v4(),
            admin_dri_person_id: seed.dri,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(ghost_entity, FirmError::NoSuchEntity(_)));
    let ghost_dri = tenancy::create_firm_on(
        &pg,
        &NewFirm {
            name: "Ghost Dri Practice".into(),
            status: "active".into(),
            entity_id: seed.entity,
            admin_dri_person_id: Uuid::new_v4(),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(ghost_dri, FirmError::NoSuchPerson(_)));
    let ineligible = tenancy::create_firm_on(
        &pg,
        &NewFirm {
            name: "Ineligible Practice".into(),
            status: "active".into(),
            entity_id: seed.entity,
            admin_dri_person_id: seed.lawyer,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(ineligible, FirmError::IneligibleAdminDriTier(_)));

    // The happy creation derives the slug and seeds the admin DRI's
    // membership in the same transaction.
    let firm = tenancy::create_firm_on(
        &pg,
        &NewFirm {
            name: "Wrapper Practice".into(),
            status: "active".into(),
            entity_id: seed.entity,
            admin_dri_person_id: seed.dri,
        },
    )
    .await
    .unwrap();
    assert_eq!(firm.slug, "wrapper-practice");
    assert_eq!(firm.entity_id, Some(seed.entity));
    let duplicate_slug = tenancy::create_firm_on(
        &pg,
        &NewFirm {
            name: "Wrapper Practice".into(),
            status: "active".into(),
            entity_id: seed.second_entity,
            admin_dri_person_id: seed.plain_admin,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(duplicate_slug, FirmError::DuplicateSlug(_)));
    let duplicate_entity = tenancy::create_firm_on(
        &pg,
        &NewFirm {
            name: "Duplicate Entity Practice".into(),
            status: "active".into(),
            entity_id: seed.entity,
            admin_dri_person_id: seed.plain_admin,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(duplicate_entity, FirmError::DuplicateEntity));

    // Membership admission: clients refuse, missing firms refuse, a real
    // lawyer joins, and a second row for the same person is a duplicate.
    let client_refused = tenancy::add_membership_on(
        &pg,
        &NewPersonFirmRole {
            person_id: seed.client,
            firm_id: firm.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(client_refused, FirmError::ClientCannotJoinFirm(_)));
    let ghost_member_firm = tenancy::add_membership_on(
        &pg,
        &NewPersonFirmRole {
            person_id: seed.lawyer,
            firm_id: Uuid::new_v4(),
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(ghost_member_firm, FirmError::NoSuchFirm(_)));
    let joined = tenancy::add_membership_on(
        &pg,
        &NewPersonFirmRole {
            person_id: seed.lawyer,
            firm_id: firm.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(joined.person_id, seed.lawyer);
    assert!(!joined.is_dri);
    let duplicate_membership = tenancy::add_membership_on(
        &pg,
        &NewPersonFirmRole {
            person_id: seed.lawyer,
            firm_id: firm.id,
            membership: FirmMembership::Clerk,
            is_dri: false,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        duplicate_membership,
        FirmError::DuplicateMembership
    ));

    // Wrapper calls resolve their database handle from the test scope; the
    // `_on` seams above took the handle explicitly.
    tenancy::with_test_db(
        pg.clone(),
        Box::pin(async {
            // Membership edits and removals authorize against the acting
            // person's tier before they touch the row.
            let demoted = tenancy::update_membership(
                crate::persons::Role::Owner,
                None,
                seed.lawyer,
                firm.id,
                FirmMembership::Clerk,
            )
            .await
            .unwrap();
            assert!(matches!(demoted.membership, FirmMembership::Clerk));
            let denied_edit = tenancy::update_membership(
                crate::persons::Role::Lawyer,
                None,
                seed.lawyer,
                firm.id,
                FirmMembership::Clerk,
            )
            .await
            .unwrap_err();
            assert!(matches!(denied_edit, FirmError::NotAuthorized));
            let stranger = tenancy::update_membership(
                crate::persons::Role::Owner,
                None,
                seed.plain_admin,
                firm.id,
                FirmMembership::Clerk,
            )
            .await
            .unwrap_err();
            assert!(matches!(stranger, FirmError::NotAMember(_, _)));
            let dri_removed =
                tenancy::remove_membership(crate::persons::Role::Owner, None, seed.dri, firm.id)
                    .await
                    .unwrap_err();
            assert!(matches!(
                dri_removed,
                FirmError::WouldLeaveFirmWithoutAdminDri(_)
            ));
            tenancy::remove_membership(crate::persons::Role::Owner, None, seed.lawyer, firm.id)
                .await
                .unwrap();

            // A second firm with its own admin DRI: appointing its admin into the
            // first firm is a cross-firm appointment, and the happy transfer within
            // one firm demotes the prior DRI in the same transaction.
            let second_firm = tenancy::create_firm_on(
                &pg,
                &NewFirm {
                    name: "Wrapper Second Practice".into(),
                    status: "active".into(),
                    entity_id: seed.second_entity,
                    admin_dri_person_id: seed.second_admin,
                },
            )
            .await
            .unwrap();
            let denied_appointment =
                tenancy::appoint_admin_dri(crate::persons::Role::Lawyer, firm.id, seed.dri)
                    .await
                    .unwrap_err();
            assert!(matches!(denied_appointment, FirmError::NotAuthorized));
            let cross_firm =
                tenancy::appoint_admin_dri(crate::persons::Role::Owner, firm.id, seed.second_admin)
                    .await
                    .unwrap_err();
            assert!(matches!(cross_firm, FirmError::AdminDriCrossFirm(_, _)));
            let wrong_membership =
                tenancy::appoint_admin_dri(crate::persons::Role::Owner, firm.id, seed.plain_admin)
                    .await
                    .unwrap_err();
            assert!(matches!(
                wrong_membership,
                FirmError::AdminDriCrossFirm(_, _)
            ));
            tenancy::add_membership_on(
                &pg,
                &NewPersonFirmRole {
                    person_id: seed.lawyer,
                    firm_id: firm.id,
                    membership: FirmMembership::Lawyer,
                    is_dri: false,
                },
            )
            .await
            .unwrap();
            let ineligible =
                tenancy::appoint_admin_dri(crate::persons::Role::Owner, firm.id, seed.lawyer)
                    .await
                    .unwrap_err();
            assert!(matches!(ineligible, FirmError::IneligibleAdminDriTier(_)));
            // A platform admin whose firm membership is not the admin tier is
            // rejected by the membership check instead.
            tenancy::add_membership_on(
                &pg,
                &NewPersonFirmRole {
                    person_id: seed.plain_admin,
                    firm_id: firm.id,
                    membership: FirmMembership::Clerk,
                    is_dri: false,
                },
            )
            .await
            .unwrap();
            let wrong_membership_tier =
                tenancy::appoint_admin_dri(crate::persons::Role::Owner, firm.id, seed.plain_admin)
                    .await
                    .unwrap_err();
            assert!(matches!(
                wrong_membership_tier,
                FirmError::WrongAdminDriMembership(_, _)
            ));
            let transferred =
                tenancy::appoint_admin_dri(crate::persons::Role::Owner, firm.id, seed.dri)
                    .await
                    .unwrap();
            assert!(transferred.is_dri);

            // Firm edits run through the wrapper with authorization; deletes refuse
            // a non-manager and then remove a firm with no projects.
            let renamed = tenancy::update_firm(
                crate::persons::Role::Owner,
                None,
                firm.id,
                &crate::firms::FirmEdit {
                    name: Some("Wrapper Practice Renamed".into()),
                    status: None,
                    entity_id: None,
                },
            )
            .await
            .unwrap();
            assert_eq!(renamed.name, "Wrapper Practice Renamed");
            let denied_rename = tenancy::update_firm(
                crate::persons::Role::Lawyer,
                None,
                firm.id,
                &crate::firms::FirmEdit {
                    name: Some("Nope".into()),
                    status: None,
                    entity_id: None,
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(denied_rename, FirmError::NotAuthorized));
            let ghost_rename = tenancy::update_firm(
                crate::persons::Role::Owner,
                None,
                Uuid::new_v4(),
                &crate::firms::FirmEdit {
                    name: Some("Ghost".into()),
                    status: None,
                    entity_id: None,
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(ghost_rename, FirmError::NoSuchFirm(_)));
            let denied_delete = tenancy::delete_firm(crate::persons::Role::Lawyer, None, firm.id)
                .await
                .unwrap_err();
            assert!(matches!(denied_delete, FirmError::NotAuthorized));
            tenancy::delete_firm(crate::persons::Role::Owner, None, second_firm.id)
                .await
                .unwrap();
            assert!(tenancy::find_firm_on(&pg, second_firm.id)
                .await
                .unwrap()
                .is_none());
        }),
    )
    .await;
    pg.0.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario drives the brand wrapper seams with their authorization and referential guards"
)]
async fn brand_wrappers_authorize_and_guard_the_brand_seams() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let seed = seed_entity_and_people(&pg).await;
    let firm = tenancy::create_firm_on(
        &pg,
        &NewFirm {
            name: "Brand Practice".into(),
            status: "active".into(),
            entity_id: seed.entity,
            admin_dri_person_id: seed.dri,
        },
    )
    .await
    .unwrap();

    // Wrapper calls resolve their database handle from the test scope.
    tenancy::with_test_db(
        pg.clone(),
        Box::pin(async {
            // A brand is created through the wrapper with the acting DRI's
            // authorization; clients and anonymous actors refuse before any
            // query.
            let created = tenancy::create_brand(
                crate::persons::Role::Owner,
                Some(seed.dri),
                &NewBrand {
                    name: "Wrapper Brand".into(),
                    key: "wrapper-brand".into(),
                    firm_id: Some(firm.id),
                    ..NewBrand::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(created.key, "wrapper-brand");
            assert_eq!(created.firm_id, Some(firm.id));
            assert!(created.is_law_firm);
            let denied_creation = tenancy::create_brand(
                crate::persons::Role::Client,
                Some(seed.client),
                &NewBrand {
                    name: "Client Brand".into(),
                    key: "client-brand".into(),
                    firm_id: Some(firm.id),
                    ..NewBrand::default()
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(denied_creation, BrandError::NotAuthorized));
            let anonymous = tenancy::create_brand(
                crate::persons::Role::Owner,
                None,
                &NewBrand {
                    name: "Anonymous Brand".into(),
                    key: "anonymous-brand".into(),
                    firm_id: Some(firm.id),
                    ..NewBrand::default()
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(anonymous, BrandError::NotAuthorized));
            let firmless = tenancy::create_brand(
                crate::persons::Role::Owner,
                Some(seed.dri),
                &NewBrand {
                    name: "Firmless Brand".into(),
                    key: "firmless-brand".into(),
                    firm_id: None,
                    ..NewBrand::default()
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(firmless, BrandError::FirmRequired));
            let unauthorized_writer = tenancy::create_brand(
                crate::persons::Role::Admin,
                Some(seed.plain_admin),
                &NewBrand {
                    name: "Usurper Brand".into(),
                    key: "usurper-brand".into(),
                    firm_id: Some(firm.id),
                    ..NewBrand::default()
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(unauthorized_writer, BrandError::NotAuthorized));

            // Attachments guard both ends: ghost firms, unknown keys, and a second
            // seat for the same brand all refuse, and the happy attach is visible.
            tenancy::attach_brand(Uuid::new_v4(), &created.key)
                .await
                .unwrap_err();
            tenancy::attach_brand(firm.id, "ghost-brand")
                .await
                .unwrap_err();
            tenancy::attach_brand(firm.id, &created.key).await.unwrap();
            assert_eq!(
                tenancy::firm_brand_keys(firm.id).await.unwrap(),
                vec!["wrapper-brand".to_string()]
            );
            let duplicate_seat = tenancy::attach_brand(firm.id, &created.key)
                .await
                .unwrap_err();
            assert!(matches!(duplicate_seat, FirmError::DuplicateBrand));

            // Presentation edits run through the wrapper with authorization.
            let renamed = tenancy::update_brand(
                crate::persons::Role::Owner,
                Some(seed.dri),
                created.id,
                &BrandEdit {
                    name: Some("Wrapper Brand Renamed".into()),
                    primary_color: Some(Some("#336699".into())),
                    ..BrandEdit::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(renamed.name, "Wrapper Brand Renamed");
            assert_eq!(renamed.primary_color.as_deref(), Some("#336699"));
            let denied_edit = tenancy::update_brand(
                crate::persons::Role::Client,
                Some(seed.client),
                created.id,
                &BrandEdit {
                    name: Some("Client Edited".into()),
                    ..BrandEdit::default()
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(denied_edit, BrandError::NotAuthorized));
            let ghost_edit = tenancy::update_brand(
                crate::persons::Role::Owner,
                Some(seed.dri),
                Uuid::new_v4(),
                &BrandEdit::default(),
            )
            .await
            .unwrap_err();
            assert!(matches!(ghost_edit, BrandError::NoSuchBrand(_)));

            // An admin who is not the firm's DRI is refused: non-owner
            // actors run the DRI check, and a bare admin membership lacks
            // the appointment.
            tenancy::add_membership_on(
                &pg,
                &NewPersonFirmRole {
                    person_id: seed.second_admin,
                    firm_id: firm.id,
                    membership: FirmMembership::Admin,
                    is_dri: false,
                },
            )
            .await
            .unwrap();
            let non_dri = tenancy::set_brand_logo(
                crate::persons::Role::Admin,
                Some(seed.second_admin),
                created.id,
                "logos/refused.png",
                "image/png",
            )
            .await
            .unwrap_err();
            assert!(matches!(non_dri, BrandError::NotAuthorized));

            // Logo and font writes are the only writers for their object
            // keys, and the acting firm DRI admin passes the DRI check.
            let logged = tenancy::set_brand_logo(
                crate::persons::Role::Admin,
                Some(seed.dri),
                created.id,
                "logos/wrapper.png",
                "image/png",
            )
            .await
            .unwrap();
            assert_eq!(logged.logo_object_key.as_deref(), Some("logos/wrapper.png"));
            let fonted = tenancy::set_brand_font(
                crate::persons::Role::Admin,
                Some(seed.dri),
                created.id,
                "Wrapper Serif",
                "fonts/wrapper.woff2",
                "OFL-1.1",
            )
            .await
            .unwrap();
            assert_eq!(fonted.font_family.as_deref(), Some("Wrapper Serif"));
            assert_eq!(
                fonted.font_object_key.as_deref(),
                Some("fonts/wrapper.woff2")
            );

            // The website setter canonicalizes https hosts and refuses insecure
            // schemes through the shared domain validator.
            let website =
                tenancy::set_brand_website(created.id, Some("https://wrapper.example.com/"))
                    .await
                    .unwrap();
            assert_eq!(
                website.website_url.as_deref(),
                Some("https://wrapper.example.com")
            );
            let insecure =
                tenancy::set_brand_website(created.id, Some("http://wrapper.example.com/"))
                    .await
                    .unwrap_err();
            assert!(matches!(insecure, BrandError::InvalidWebsiteUrl(_)));

            // Moving a brand repoints its owning firm and records the new seat.
            let target = tenancy::create_firm_on(
                &pg,
                &NewFirm {
                    name: "Brand Target Practice".into(),
                    status: "active".into(),
                    entity_id: seed.second_entity,
                    admin_dri_person_id: seed.second_admin,
                },
            )
            .await
            .unwrap();
            tenancy::move_brand(&created.key, target.id).await.unwrap();
            assert_eq!(
                tenancy::brand_by_key(&created.key)
                    .await
                    .unwrap()
                    .unwrap()
                    .firm_id,
                Some(target.id)
            );
            assert!(tenancy::firm_brand_keys(firm.id).await.unwrap().is_empty());
            assert_eq!(
                tenancy::firm_brand_keys(target.id).await.unwrap(),
                vec!["wrapper-brand".to_string()]
            );

            // Deletion refuses while a firm still references the brand.
            let referenced =
                tenancy::delete_brand(crate::persons::Role::Owner, Some(seed.dri), created.id)
                    .await
                    .unwrap_err();
            assert!(matches!(referenced, BrandError::StillReferenced));
            tenancy::detach_brand(target.id, &created.key)
                .await
                .unwrap();
            tenancy::delete_brand(crate::persons::Role::Owner, Some(seed.dri), created.id)
                .await
                .unwrap();
            assert!(tenancy::find_brand_on(&pg, created.id)
                .await
                .unwrap()
                .is_none());
        }),
    )
    .await;
    pg.0.close().await;
}
