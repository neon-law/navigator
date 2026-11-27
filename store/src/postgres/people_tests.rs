use super::*;

#[tokio::test]
async fn configured_authority_fails_closed_when_postgres_is_unavailable() {
    const CASE: &str = "NAVIGATOR_POSTGRES_PEOPLE_UNAVAILABLE_TEST_CASE";
    if std::env::var(CASE).as_deref() != Ok("child") {
        let mut child =
            std::process::Command::new(std::env::current_exe().expect("test executable"));
        child
            .args([
                "--exact",
                "postgres::people::tests::configured_authority_fails_closed_when_postgres_is_unavailable",
                "--nocapture",
            ])
            .env(CASE, "child")
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env("NAVIGATOR_POSTGRES_PORT", "1")
            .env("NAVIGATOR_POSTGRES_USER", "postgres")
            .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
            .env("NAVIGATOR_POSTGRES_DATABASE", "smoke")
            .env("NAVIGATOR_POSTGRES_SSLMODE", "disable");
        let output = child
            .output()
            .expect("launch isolated unavailable-PostgreSQL test");
        assert!(
            output.status.success(),
            "isolated fail-closed test failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    // Applying the schema reads the Firm anchor, which this configuration
    // refuses too; the accessors below are refused before any query.
    let application_store = crate::application_store::ApplicationStore::uninitialized();
    assert!(matches!(
        crate::persons::find_by_id(&application_store, Uuid::nil()).await,
        Err(PersonError::Postgres(_))
    ));
    assert!(matches!(
        crate::addresses::list_all(&application_store).await,
        Err(crate::addresses::AddressError::Authority(
            crate::postgres::identity::AuthorityError::Unavailable(_)
        ))
    ));
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario drives every PostgreSQL People helper and its guard rails"
)]
async fn postgres_people_helpers_read_filter_write_and_guard_columns() {
    let Some((options, database)) = crate::postgres::fixtures::fresh_schema().await else {
        return;
    };
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
    crate::postgres::fixtures::activate_application(&pg).await;
    Box::pin(PostgresDb::with_test_db(pg.clone(), async {
        let application_store = crate::application_store::ApplicationStore::uninitialized();

        let owner = super::create(
            &pg,
            &application_store,
            &NewPerson::new(
                "Helper Owner",
                format!("helper-owner-{}@example.test", Uuid::now_v7()),
            ),
        )
        .await
        .unwrap();
        let lawyer = super::create(
            &pg,
            &application_store,
            &NewPerson::new(
                "Helper Lawyer",
                format!("helper-lawyer-{}@example.test", Uuid::now_v7()),
            ),
        )
        .await
        .unwrap();
        let client = super::create(
            &pg,
            &application_store,
            &NewPerson::new(
                "Helper Client",
                format!("helper-client-{}@example.test", Uuid::now_v7()),
            ),
        )
        .await
        .unwrap();
        assert_eq!(client.role, Role::Client);
        super::set_role(&pg, &application_store, owner.id, Role::Owner)
            .await
            .unwrap()
            .unwrap();
        super::set_role(&pg, &application_store, lawyer.id, Role::Lawyer)
            .await
            .unwrap()
            .unwrap();
        // The client keeps the default role; give the owner and lawyer phones.
        super::set_text(
            &pg,
            &application_store,
            owner.id,
            "phone",
            Some("+15555550001"),
        )
        .await
        .unwrap()
        .unwrap();
        super::set_text(
            &pg,
            &application_store,
            lawyer.id,
            "phone",
            Some("+15555550002"),
        )
        .await
        .unwrap()
        .unwrap();

        // Reads: case-insensitive email, subject lookup, id batch, phone filter,
        // and role filter all agree with what was written.
        let mixed_case = owner.email.to_uppercase();
        assert_eq!(
            super::by_email(&pg, &mixed_case).await.unwrap().unwrap().id,
            owner.id
        );
        assert!(super::by_email(&pg, "missing-helper@example.test")
            .await
            .unwrap()
            .is_none());
        super::link_subject(
            &pg,
            &application_store,
            owner.id,
            "oidc_subject",
            "helper-owner-subject",
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            super::by_subject(&pg, "oidc_subject", "helper-owner-subject")
                .await
                .unwrap()
                .unwrap()
                .id,
            owner.id
        );
        let many = super::many_by_ids(&pg, &[owner.id, lawyer.id, Uuid::new_v4()])
            .await
            .unwrap();
        assert_eq!(many.len(), 2);
        let with_phone = super::all_with_phone(&pg).await.unwrap();
        assert_eq!(with_phone.len(), 2);
        assert_eq!(super::by_role(&pg, Role::Owner).await.unwrap().len(), 1);
        assert_eq!(super::by_role(&pg, Role::Client).await.unwrap().len(), 1);

        // The whole directory reads in name order; filtered pages are
        // `directory_page`'s, covered below.
        let directory = super::directory(&pg).await.unwrap();
        assert_eq!(directory.len(), 3);
        assert!(directory
            .windows(2)
            .all(|pair| pair[0].name <= pair[1].name));
        let limited = super::search(&pg, Some("helper"), None, 2).await.unwrap();
        assert_eq!(limited.len(), 2);

        // The default DRI prefers the highest authority tier, tie-broken by id.
        assert_eq!(super::default_firm_dri(&pg).await.unwrap(), Some(owner.id));

        // The mailbox holder reads the authoritative claim; a missing row and a
        // missing person both fail open for admission.
        assert_eq!(
            super::mailbox_holder(&pg, &owner.email).await.unwrap(),
            Some(owner.id)
        );
        assert!(super::is_admitted(&pg, Uuid::new_v4()).await.unwrap());

        // Guard rails: unsupported columns and subjects are refused, not written.
        assert!(
            super::set_text(&pg, &application_store, owner.id, "name", Some("Nope"))
                .await
                .is_err()
        );
        assert!(
            super::set_flag(&pg, &application_store, owner.id, "bogus_flag", true)
                .await
                .is_err()
        );
        assert!(super::link_subject(
            &pg,
            &application_store,
            owner.id,
            "bogus_subject",
            "x",
            false
        )
        .await
        .is_err());

        // Flag, contact, and edit writes move the authoritative row and project.
        let flagged = super::set_flag(&pg, &application_store, owner.id, "email_confirmed", true)
            .await
            .unwrap()
            .unwrap();
        assert!(flagged.email_confirmed);
        assert!(
            super::by_id(&pg, owner.id)
                .await
                .unwrap()
                .unwrap()
                .email_confirmed
        );
        assert!(
            crate::persons::find_by_id(&application_store, owner.id)
                .await
                .unwrap()
                .unwrap()
                .email_confirmed
        );
        let contacted = super::update_contact(
            &pg,
            &application_store,
            lawyer.id,
            &crate::persons::ContactUpdate {
                name: "Helper Lawyer Renamed".into(),
                title: Some("Senior Partner".into()),
                phone: Some("+15555550003".into()),
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(contacted.name, "Helper Lawyer Renamed");
        assert_eq!(contacted.title.as_deref(), Some("Senior Partner"));
        let edited = super::edit(
            &pg,
            &application_store,
            lawyer.id,
            &PersonEdit {
                given_name: Some(Some("Helper".into())),
                family_name: Some(Some("Lawyer".into())),
                linkedin_url: Some(Some("https://www.linkedin.com/in/helper-lawyer".into())),
                ..PersonEdit::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(edited.given_name.as_deref(), Some("Helper"));
        assert_eq!(edited.family_name.as_deref(), Some("Lawyer"));

        // The adopt-legacy subject link migrates an OIDC subject to a provider
        // column and clears the legacy claim in the same statement.
        let adopted = super::link_subject(
            &pg,
            &application_store,
            owner.id,
            "microsoft_subject",
            "helper-owner-subject",
            true,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            adopted.microsoft_subject.as_deref(),
            Some("helper-owner-subject")
        );
        assert_eq!(adopted.oidc_subject, None);

        // find_or_create is a no-op on an existing email and creates otherwise.
        let found = super::find_or_create(
            &pg,
            &application_store,
            &NewPerson::new("Helper Owner Again", &owner.email),
        )
        .await
        .unwrap();
        assert_eq!(found.id, owner.id);
        let created_again = super::find_or_create(
            &pg,
            &application_store,
            &NewPerson::new(
                "Helper Second",
                format!("helper-second-{}@example.test", Uuid::now_v7()),
            ),
        )
        .await
        .unwrap();
        assert_ne!(created_again.id, owner.id);

        // Delete removes the row and its mailbox claim; the projection runs.
        super::delete(&pg, &application_store, created_again.id)
            .await
            .unwrap();
        assert!(super::by_id(&pg, created_again.id).await.unwrap().is_none());
        assert_eq!(
            super::mailbox_holder(&pg, &created_again.email)
                .await
                .unwrap(),
            None
        );
        pg.0.close().await;
    }))
    .await;
}

#[tokio::test]
async fn directory_views_join_firm_names_and_scope_to_visible_people() {
    let Some((options, database)) = crate::postgres::fixtures::fresh_schema().await else {
        return;
    };
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
    crate::postgres::fixtures::activate_application(&pg).await;
    PostgresDb::with_test_db(pg.clone(), async {
    let application_store = crate::application_store::ApplicationStore::uninitialized();
    let person = |name: &str| NewPerson::new(name, format!("{}@example.test", Uuid::now_v7()));
    let member = super::create(&pg, &application_store, &person("Directory Member"))
        .await
        .unwrap();
    let participant = super::create(&pg, &application_store, &person("Directory Participant"))
        .await
        .unwrap();
    let stranger = super::create(&pg, &application_store, &person("Directory Stranger"))
        .await
        .unwrap();

    let (jurisdiction, entity_type, entity) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (first, second, project) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    sqlx::raw_sql(AssertSqlSafe(format!(
        "INSERT INTO jurisdiction (id,name,code) VALUES ('{jurisdiction}','Synthetic Jurisdiction','TST');
         INSERT INTO entity_type (id,name) VALUES ('{entity_type}','Synthetic Entity');
         INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ('{entity}','Synthetic Entity','{entity_type}','{jurisdiction}');
         INSERT INTO firm (id,name,slug,status) VALUES ('{first}','First Firm','first-firm','active'), ('{second}','Second Firm','second-firm','active');
         INSERT INTO person_firm_role (person_id,firm_id,membership,inserted_at) VALUES
             ('{}','{second}','lawyer',now() - interval '1 day'), ('{}','{first}','lawyer',now());
         INSERT INTO project (id,code,name,status,entity_id,firm_id) VALUES ('{project}','synthetic-matter','Synthetic Matter','open','{entity}','{first}');
         INSERT INTO person_project_role (person_id,project_id,participation) VALUES ('{}','{project}','client');",
        member.id, member.id, participant.id
    )))
    .execute(&pg.0)
    .await
    .unwrap();

    let params = |query: &str| {
        crate::listing::ListParams::parse(query, &crate::firms::PEOPLE_DIRECTORY).unwrap()
    };
    let ids = |page: &crate::listing::Page<(Person, Vec<String>)>| -> Vec<Uuid> {
        page.items.iter().map(|(p, _)| p.id).collect()
    };

    // Everyone, name order, each with Firm names in membership order.
    let everyone = super::directory_page(&pg, &params(""), None).await.unwrap();
    assert_eq!(ids(&everyone), vec![member.id, participant.id, stranger.id]);
    assert_eq!(everyone.items[0].1, vec!["Second Firm", "First Firm"]);
    assert!(everyone.items[1].1.is_empty());
    assert!(everyone.next.is_none() && everyone.prev.is_none());

    // An Admin of the first Firm sees its member and its matter's participant.
    let scoped = super::directory_page(&pg, &params("sort=-name"), Some(&[first]))
        .await
        .unwrap();
    assert_eq!(ids(&scoped), vec![participant.id, member.id]);

    // Filters still apply, match literally, and no Firm admits nobody.
    let filtered = super::directory_page(&pg, &params("filter[name]=PARTICIPANT"), Some(&[first]))
        .await
        .unwrap();
    assert_eq!(ids(&filtered), vec![participant.id]);
    let literal = super::directory_page(&pg, &params("filter[name]=Directory%25"), None)
        .await
        .unwrap();
    assert!(literal.items.is_empty());
    assert!(super::directory_page(&pg, &params(""), Some(&[]))
        .await
        .unwrap()
        .items
        .is_empty());
    pg.0.close().await;
    }).await;
}

/// Keyset pages walk the whole directory forward and back without a gap or a
/// repeat — through tied names, under each sort direction.
#[tokio::test]
async fn directory_pages_walk_every_row_once_in_both_directions() {
    let Some((options, database)) = crate::postgres::fixtures::fresh_schema().await else {
        return;
    };
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
    crate::postgres::fixtures::activate_application(&pg).await;
    PostgresDb::with_test_db(pg.clone(), async {
        let application_store = crate::application_store::ApplicationStore::uninitialized();
        let mut created = Vec::new();
        for name in [
            "Row 1", "Row 2", "Row 2", "Row 2", "Row 3", "Row 4", "Row 5",
        ] {
            let input = NewPerson::new(name, format!("{}@example.test", Uuid::now_v7()));
            created.push(
                super::create(&pg, &application_store, &input)
                    .await
                    .unwrap(),
            );
        }
        for sort in ["name", "-name", "email", "name,-email"] {
            let parse = |query: String| {
                crate::listing::ListParams::parse(&query, &crate::firms::PEOPLE_DIRECTORY).unwrap()
            };
            let mut forward = Vec::new();
            let mut pages = Vec::new();
            let mut query = format!("sort={sort}&page[size]=3");
            loop {
                let page = super::directory_page(&pg, &parse(query.clone()), None)
                    .await
                    .unwrap();
                forward.extend(page.items.iter().map(|(p, _)| p.id));
                pages.push(page.items.len());
                let Some(next) = page.next.clone() else { break };
                query = format!("sort={sort}&page[size]=3&page[after]={next}");
            }
            assert_eq!(pages, vec![3, 3, 1], "{sort}");
            let mut unique = forward.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(
                unique.len(),
                created.len(),
                "{sort}: no row twice, none missed"
            );

            // Walking back from the last page retraces the same rows.
            let last = super::directory_page(&pg, &parse(query.clone()), None)
                .await
                .unwrap();
            let prev = last.prev.expect("the last page links back");
            let back = super::directory_page(
                &pg,
                &parse(format!("sort={sort}&page[size]=3&page[before]={prev}")),
                None,
            )
            .await
            .unwrap();
            let back_ids: Vec<_> = back.items.iter().map(|(p, _)| p.id).collect();
            assert_eq!(back_ids, forward[3..6].to_vec(), "{sort}");
            assert!(back.next.is_some() && back.prev.is_some(), "{sort}");
        }
        pg.0.close().await;
    })
    .await;
}

#[tokio::test]
async fn email_is_citext_matching_any_case_and_keeping_the_entered_casing() {
    let Some((options, database)) = crate::postgres::fixtures::fresh_schema().await else {
        return;
    };
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
    crate::postgres::fixtures::activate_application(&pg).await;
    PostgresDb::with_test_db(pg.clone(), async {
        let application_store = crate::application_store::ApplicationStore::uninitialized();
        let entered = format!("Cased.Person-{}@Example.test", Uuid::now_v7());
        let person = super::create(
            &pg,
            &application_store,
            &NewPerson::new("Cased Person", &entered),
        )
        .await
        .unwrap();
        assert_eq!(person.email, entered);

        // A lookup in any case finds the row and returns the casing entered.
        let found = super::by_email(&pg, &entered.to_lowercase())
            .await
            .unwrap()
            .expect("a differently-cased lookup finds the Person");
        assert_eq!(
            (found.id, found.email.as_str()),
            (person.id, entered.as_str())
        );
        let query = format!("filter[email]={}", entered.to_uppercase());
        let params =
            crate::listing::ListParams::parse(&query, &crate::firms::PEOPLE_DIRECTORY).unwrap();
        let filtered = super::directory_page(&pg, &params, None).await.unwrap();
        assert_eq!(filtered.items.len(), 1);

        // A case variant is the same mailbox: the unique index refuses it.
        assert!(matches!(
            super::create(
                &pg,
                &application_store,
                &NewPerson::new("Variant", entered.to_uppercase())
            )
            .await,
            Err(PersonError::EmailTaken)
        ));

        // A change only in letter case is written, and the mailbox stays put.
        let recased = entered.to_lowercase();
        let edited = super::edit(
            &pg,
            &application_store,
            person.id,
            &PersonEdit {
                email: Some(recased.clone()),
                ..PersonEdit::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(edited.email, recased);
        assert_eq!(
            super::mailbox_holder(&pg, &entered).await.unwrap(),
            Some(person.id)
        );
        pg.0.close().await;
    })
    .await;
}
