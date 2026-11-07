use super::*;

async fn wait_for_person_projection_waiter(pg: &PostgresDb, id: Uuid) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks \
                 WHERE locktype='advisory' AND NOT granted \
                   AND classid::bigint=1312902736 \
                   AND objid::bigint=(hashtext($1::text)::bigint & 4294967295) \
                   AND objsubid=2)",
            )
            .bind(id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("projection should queue on the held per-Person advisory lock");
}

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
            .env("NAVIGATOR_POSTGRES_PEOPLE_AUTHORITY", "1")
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

    let surreal = crate::test_support::mem_surreal().await;
    assert!(matches!(
        crate::persons::find_by_id(&surreal, Uuid::nil()).await,
        Err(PersonError::Postgres(_))
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end authority, projection, concurrency, and recovery scenario.
async fn authoritative_facade_uses_postgres_and_keeps_the_compatibility_projection() {
    let case = std::env::var("NAVIGATOR_POSTGRES_PEOPLE_TEST_CASE").ok();
    if case.as_deref() != Some("child") {
        let Some(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").ok() else {
            assert_ne!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
                Ok("1"),
                "required PostgreSQL People test lacks its disposable port"
            );
            return;
        };
        let (admin_options, _) = crate::postgres_mirror::tests::test_postgres()
            .expect("PostgreSQL fixture port was present");
        let admin = sqlx::PgPool::connect_with(admin_options.clone())
            .await
            .expect("connect to synthetic PostgreSQL admin database");
        let database = format!("people_test_{}", Uuid::now_v7().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
            .execute(&admin)
            .await
            .expect("create isolated PostgreSQL People database");

        // The graph migration requires the AGE extension; production creates it
        // in the privileged init path before any migration runs.
        let mut fixture_options = admin_options.clone();
        fixture_options = fixture_options.database(&database);
        let fixture = sqlx::PgPool::connect_with(fixture_options)
            .await
            .expect("connect to isolated PostgreSQL People database");
        sqlx::query("CREATE EXTENSION IF NOT EXISTS age")
            .execute(&fixture)
            .await
            .expect("create AGE extension on isolated PostgreSQL People database");
        fixture.close().await;

        let mut child =
            std::process::Command::new(std::env::current_exe().expect("test executable"));
        child
            .args([
                "--exact",
                "postgres::people::tests::authoritative_facade_uses_postgres_and_keeps_the_compatibility_projection",
                "--nocapture",
            ])
            .env("NAVIGATOR_POSTGRES_PEOPLE_TEST_CASE", "child")
            .env("NAVIGATOR_POSTGRES_PEOPLE_AUTHORITY", "1")
            .env("NAVIGATOR_TEST_POSTGRES_PORT", &port)
            .env("NAVIGATOR_TEST_POSTGRES_DATABASE", &database)
            .env("NAV_REQUIRE_POSTGRES", "1")
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env("NAVIGATOR_POSTGRES_PORT", &port)
            .env("NAVIGATOR_POSTGRES_USER", "postgres")
            .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
            .env("NAVIGATOR_POSTGRES_DATABASE", &database)
            .env("NAVIGATOR_POSTGRES_SSLMODE", "disable");
        let output = child.output();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE {database} WITH (FORCE)"
        )))
        .execute(&admin)
        .await
        .expect("drop isolated PostgreSQL People database");
        admin.close().await;
        let output = output.expect("launch isolated PostgreSQL People test");
        assert!(
            output.status.success(),
            "isolated PostgreSQL People test failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let port: u16 = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT")
        .expect("test fixture port")
        .parse()
        .expect("fixture port is numeric");
    let database =
        std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").expect("test fixture database");
    let options = sqlx::postgres::PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(&database)
        .ssl_mode(sqlx::postgres::PgSslMode::Disable);
    let pg = PostgresDb::connect(options)
        .await
        .expect("connect to synthetic PostgreSQL");
    pg.migrate_embedded(None)
        .await
        .expect("apply canonical schema");

    let surreal = crate::test_support::mem_surreal().await;
    let email = format!("person-{}@example.test", Uuid::now_v7());
    let input = NewPerson::new("Synthetic Person", &email);
    let created = crate::persons::create(&surreal, &input).await.unwrap();
    assert_eq!(
        crate::persons::find_by_id(&surreal, created.id)
            .await
            .unwrap(),
        Some(created.clone())
    );
    assert_eq!(
        crate::persons::mailbox_holder(&surreal, &email)
            .await
            .unwrap(),
        Some(created.id)
    );

    // The compatibility copy can be stale, but the legacy reverse mirror and
    // reconciliation path must not overwrite or delete PostgreSQL People.
    surreal
        .query("UPDATE $id SET name='Stale projection', role='owner'")
        .bind(("id", record_id("person", created.id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .unwrap();
    assert_eq!(
        crate::postgres_mirror::mirror(
            &surreal,
            &pg,
            &crate::postgres_mirror::MirrorKey::Person(created.id),
        )
        .await
        .unwrap(),
        crate::postgres_mirror::Mirrored::Unchanged
    );
    let identity = crate::postgres_mirror::identity::reconcile(&surreal, &pg)
        .await
        .unwrap();
    assert!(identity
        .divergent()
        .any(|key| key == &crate::postgres_mirror::MirrorKey::Person(created.id)));
    let unchanged: (String, String, String) =
        sqlx::query_as("SELECT name, email, role FROM person WHERE id=$1")
            .bind(created.id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(
        unchanged,
        (created.name.clone(), email.clone(), "client".into())
    );

    // A missing compatibility row must not make reconciliation delete the
    // authoritative PostgreSQL Person.
    surreal
        .query("DELETE $id")
        .bind(("id", record_id("person", created.id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .unwrap();
    assert_eq!(
        crate::postgres_mirror::mirror(
            &surreal,
            &pg,
            &crate::postgres_mirror::MirrorKey::Person(created.id),
        )
        .await
        .unwrap(),
        crate::postgres_mirror::Mirrored::Unchanged
    );
    crate::postgres_mirror::identity::reconcile(&surreal, &pg)
        .await
        .unwrap();
    assert_eq!(
        super::by_id(&pg, created.id).await.unwrap(),
        Some(created.clone())
    );

    let other_email = format!("other-{}@example.test", Uuid::now_v7());
    let other = crate::persons::create(
        &surreal,
        &NewPerson::new("Other Synthetic Person", &other_email),
    )
    .await
    .unwrap();
    surreal
        .query("DELETE person_mailbox WHERE id=$email; UPSERT type::record('person_mailbox', $email) SET person_id=$person")
        .bind(("email", email.clone()))
        .bind(("person", record_id("person", other.id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .unwrap();
    assert_eq!(
        crate::postgres_mirror::mirror(
            &surreal,
            &pg,
            &crate::postgres_mirror::MirrorKey::PersonMailbox(other.id),
        )
        .await
        .unwrap(),
        crate::postgres_mirror::Mirrored::Unchanged
    );
    let mailbox_parity = crate::postgres_mirror::identity_detail::reconcile(&surreal, &pg)
        .await
        .unwrap();
    assert!(mailbox_parity
        .divergent()
        .any(|key| { key == &crate::postgres_mirror::MirrorKey::PersonMailbox(other.id) }));
    assert_eq!(
        crate::persons::mailbox_holder(&surreal, &email)
            .await
            .unwrap(),
        Some(created.id),
        "legacy mailbox reconciliation cannot move the PostgreSQL claim"
    );

    // A stale source copy cannot override an authoritative read.
    surreal
        .query("DELETE $id")
        .bind(("id", record_id("person", created.id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .unwrap();
    assert_eq!(
        crate::persons::find_by_email_ci(&surreal, &email)
            .await
            .unwrap(),
        Some(created.clone())
    );

    let edit = PersonEdit {
        email: Some(format!("updated-{}@example.test", Uuid::now_v7())),
        role: Some(Role::Lawyer),
        ..PersonEdit::default()
    };
    let updated = crate::persons::edit(&surreal, created.id, &edit)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.role, Role::Lawyer);
    assert_eq!(
        crate::persons::mailbox_holder(&surreal, &email)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        crate::persons::mailbox_holder(&surreal, &updated.email)
            .await
            .unwrap(),
        Some(created.id)
    );

    // Concurrent email changes serialize on the Person row. Only the final
    // address may remain claimed, regardless of which caller commits last.
    let left_email = format!("left-{}@example.test", Uuid::now_v7());
    let right_email = format!("right-{}@example.test", Uuid::now_v7());
    let left = PersonEdit {
        email: Some(left_email.clone()),
        ..PersonEdit::default()
    };
    let right = PersonEdit {
        email: Some(right_email.clone()),
        ..PersonEdit::default()
    };
    let (left_result, right_result) = tokio::join!(
        crate::persons::edit(&surreal, created.id, &left),
        crate::persons::edit(&surreal, created.id, &right),
    );
    let left_result = left_result.unwrap().unwrap();
    let right_result = right_result.unwrap().unwrap();
    let final_person = crate::persons::find_by_id(&surreal, created.id)
        .await
        .unwrap()
        .unwrap();
    assert!(final_person.email == left_result.email || final_person.email == right_result.email);
    let claimed: Vec<String> =
        sqlx::query_scalar("SELECT id FROM person_mailbox WHERE person_id=$1 ORDER BY id")
            .bind(created.id)
            .fetch_all(&pg.0)
            .await
            .unwrap();
    assert_eq!(claimed, vec![final_person.email.to_lowercase()]);
    assert_ne!(left_result.email, right_result.email);

    let oidc_subject = format!("synthetic-primary-{}", Uuid::now_v7());
    crate::persons::link_oidc_subject(&surreal, created.id, &oidc_subject)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        crate::persons::find_by_oidc_subject(&surreal, &oidc_subject)
            .await
            .unwrap()
            .unwrap()
            .id,
        created.id
    );
    crate::persons::set_admitted(&surreal, created.id, false)
        .await
        .unwrap();
    assert!(!crate::persons::is_admitted(&surreal, created.id)
        .await
        .unwrap());

    let duplicate = NewPerson::new("Duplicate", &final_person.email);
    assert!(matches!(
        crate::persons::create(&surreal, &duplicate).await,
        Err(PersonError::EmailTaken)
    ));
    crate::persons::delete(&surreal, created.id).await.unwrap();
    assert!(crate::persons::find_by_id(&surreal, created.id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        crate::persons::mailbox_holder(&surreal, &final_person.email)
            .await
            .unwrap(),
        None
    );

    // A PostgreSQL commit survives projection failure. Retrying through the
    // supported create-or-find path repairs the compatibility copy without
    // making a duplicate Person.
    let recovery_email = format!("recovery-{}@example.test", Uuid::now_v7());
    let recovery_input = NewPerson::new("Recovery Person", &recovery_email);
    let unavailable_surreal = SurrealDb::uninitialized();
    assert!(matches!(
        crate::persons::create(&unavailable_surreal, &recovery_input).await,
        Err(PersonError::PostgresProjection(_))
    ));
    let recovered = crate::persons::find_or_create(&surreal, &recovery_input)
        .await
        .unwrap();
    assert_eq!(recovered.email, recovery_email);
    assert_eq!(
        crate::persons::find_by_id(&surreal, recovered.id)
            .await
            .unwrap(),
        Some(recovered.clone())
    );

    // A delayed projection must read the latest committed row, not a Person
    // snapshot captured before it waited for this same per-Person lock.
    let delayed_email = format!("delayed-{}@example.test", Uuid::now_v7());
    let person_id = recovered.id;
    let mut projection_lock = super::person_transaction(&pg, person_id).await.unwrap();
    let projection_pg = pg.clone();
    let projection_surreal = surreal.clone();
    let delayed_projection = tokio::spawn(async move {
        super::sync_current_surreal(&projection_pg, &projection_surreal, person_id).await
    });
    wait_for_person_projection_waiter(&pg, person_id).await;
    assert!(!delayed_projection.is_finished());
    sqlx::query("DELETE FROM person_mailbox WHERE person_id=$1")
        .bind(person_id)
        .execute(&mut *projection_lock)
        .await
        .unwrap();
    sqlx::query("UPDATE person SET name='Latest committed person', email=$2, role='owner', is_admitted=false WHERE id=$1")
    .bind(person_id)
    .bind(&delayed_email)
    .execute(&mut *projection_lock)
    .await
    .unwrap();
    sqlx::query("INSERT INTO person_mailbox(id, person_id) VALUES(lower($1), $2)")
        .bind(&delayed_email)
        .bind(person_id)
        .execute(&mut *projection_lock)
        .await
        .unwrap();
    projection_lock.commit().await.unwrap();
    delayed_projection.await.unwrap().unwrap();
    let mut response = surreal
        .query("SELECT VALUE name FROM ONLY $id")
        .bind(("id", record_id("person", person_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let projected_name: Option<String> = response.take(0).unwrap();
    assert_eq!(projected_name.as_deref(), Some("Latest committed person"));
    let mut response = surreal
        .query("SELECT VALUE email FROM ONLY $id")
        .bind(("id", record_id("person", person_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let projected_email: Option<String> = response.take(0).unwrap();
    assert_eq!(projected_email.as_deref(), Some(delayed_email.as_str()));
    let mut response = surreal
        .query("SELECT VALUE role FROM ONLY $id")
        .bind(("id", record_id("person", person_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let projected_role: Option<String> = response.take(0).unwrap();
    assert_eq!(projected_role.as_deref(), Some("owner"));
    let mut response = surreal
        .query("SELECT VALUE is_admitted FROM ONLY $id")
        .bind(("id", record_id("person", person_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let projected_admission: Option<bool> = response.take(0).unwrap();
    assert_eq!(projected_admission, Some(false));
    let mut response = surreal
        .query("SELECT VALUE <string>person_id.id FROM ONLY type::record('person_mailbox', $email)")
        .bind(("email", delayed_email.clone()))
        .await
        .unwrap()
        .check()
        .unwrap();
    let projected_holder: Option<String> = response.take(0).unwrap();
    let expected_holder = format!("person:u'{person_id}'");
    assert_eq!(projected_holder.as_deref(), Some(expected_holder.as_str()));

    // A delayed projector after a committed delete must remove the stale
    // compatibility row instead of resurrecting its earlier snapshot.
    let mut projection_lock = super::person_transaction(&pg, person_id).await.unwrap();
    let projection_pg = pg.clone();
    let projection_surreal = surreal.clone();
    let delayed_projection = tokio::spawn(async move {
        super::sync_current_surreal(&projection_pg, &projection_surreal, person_id).await
    });
    wait_for_person_projection_waiter(&pg, person_id).await;
    assert!(!delayed_projection.is_finished());
    sqlx::query("DELETE FROM person_mailbox WHERE person_id=$1")
        .bind(person_id)
        .execute(&mut *projection_lock)
        .await
        .unwrap();
    sqlx::query("DELETE FROM person WHERE id=$1")
        .bind(person_id)
        .execute(&mut *projection_lock)
        .await
        .unwrap();
    projection_lock.commit().await.unwrap();
    delayed_projection.await.unwrap().unwrap();
    let mut response = surreal
        .query("SELECT VALUE email FROM ONLY $id")
        .bind(("id", record_id("person", person_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let projected_email: Option<String> = response.take(0).unwrap();
    assert_eq!(projected_email, None);
    let mut response = surreal
        .query("SELECT VALUE <string>person_id.id FROM ONLY type::record('person_mailbox', $email)")
        .bind(("email", delayed_email))
        .await
        .unwrap()
        .check()
        .unwrap();
    let projected_holder: Option<String> = response.take(0).unwrap();
    assert_eq!(projected_holder, None);
    pg.0.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario drives every PostgreSQL People helper and its guard rails"
)]
async fn postgres_people_helpers_read_filter_write_and_guard_columns() {
    let Some((options, database)) = crate::postgres_mirror::tests::fresh_schema().await else {
        return;
    };
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    let surreal = crate::test_support::mem_surreal().await;

    let owner = super::create(
        &pg,
        &surreal,
        &NewPerson::new(
            "Helper Owner",
            format!("helper-owner-{}@example.test", Uuid::now_v7()),
        ),
    )
    .await
    .unwrap();
    let lawyer = super::create(
        &pg,
        &surreal,
        &NewPerson::new(
            "Helper Lawyer",
            format!("helper-lawyer-{}@example.test", Uuid::now_v7()),
        ),
    )
    .await
    .unwrap();
    let client = super::create(
        &pg,
        &surreal,
        &NewPerson::new(
            "Helper Client",
            format!("helper-client-{}@example.test", Uuid::now_v7()),
        ),
    )
    .await
    .unwrap();
    assert_eq!(client.role, Role::Client);
    super::set_role(&pg, &surreal, owner.id, Role::Owner)
        .await
        .unwrap()
        .unwrap();
    super::set_role(&pg, &surreal, lawyer.id, Role::Lawyer)
        .await
        .unwrap()
        .unwrap();
    // The client keeps the default role; give the owner and lawyer phones.
    super::set_text(&pg, &surreal, owner.id, "phone", Some("+15555550001"))
        .await
        .unwrap()
        .unwrap();
    super::set_text(&pg, &surreal, lawyer.id, "phone", Some("+15555550002"))
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
        &surreal,
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
        super::set_text(&pg, &surreal, owner.id, "name", Some("Nope"))
            .await
            .is_err()
    );
    assert!(super::set_flag(&pg, &surreal, owner.id, "bogus_flag", true)
        .await
        .is_err());
    assert!(
        super::link_subject(&pg, &surreal, owner.id, "bogus_subject", "x", false)
            .await
            .is_err()
    );

    // Flag, contact, and edit writes move the authoritative row and project.
    let flagged = super::set_flag(&pg, &surreal, owner.id, "email_confirmed", true)
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
        crate::persons::find_by_id(&surreal, owner.id)
            .await
            .unwrap()
            .unwrap()
            .email_confirmed
    );
    let contacted = super::update_contact(
        &pg,
        &surreal,
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
        &surreal,
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
        &surreal,
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
        &surreal,
        &NewPerson::new("Helper Owner Again", &owner.email),
    )
    .await
    .unwrap();
    assert_eq!(found.id, owner.id);
    let created_again = super::find_or_create(
        &pg,
        &surreal,
        &NewPerson::new(
            "Helper Second",
            format!("helper-second-{}@example.test", Uuid::now_v7()),
        ),
    )
    .await
    .unwrap();
    assert_ne!(created_again.id, owner.id);

    // Delete removes the row and its mailbox claim; the projection runs.
    super::delete(&pg, &surreal, created_again.id)
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
}

#[tokio::test]
async fn directory_views_join_firm_names_and_scope_to_visible_people() {
    let Some((options, database)) = crate::postgres_mirror::tests::fresh_schema().await else {
        return;
    };
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    let surreal = crate::test_support::mem_surreal().await;
    let person = |name: &str| NewPerson::new(name, format!("{}@example.test", Uuid::now_v7()));
    let member = super::create(&pg, &surreal, &person("Directory Member"))
        .await
        .unwrap();
    let participant = super::create(&pg, &surreal, &person("Directory Participant"))
        .await
        .unwrap();
    let stranger = super::create(&pg, &surreal, &person("Directory Stranger"))
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
}

/// Keyset pages walk the whole directory forward and back without a gap or a
/// repeat — through tied names, under each sort direction.
#[tokio::test]
async fn directory_pages_walk_every_row_once_in_both_directions() {
    let Some((options, database)) = crate::postgres_mirror::tests::fresh_schema().await else {
        return;
    };
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    let surreal = crate::test_support::mem_surreal().await;
    let mut created = Vec::new();
    for name in [
        "Row 1", "Row 2", "Row 2", "Row 2", "Row 3", "Row 4", "Row 5",
    ] {
        let input = NewPerson::new(name, format!("{}@example.test", Uuid::now_v7()));
        created.push(super::create(&pg, &surreal, &input).await.unwrap());
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
}

#[tokio::test]
async fn email_is_citext_matching_any_case_and_keeping_the_entered_casing() {
    let Some((options, database)) = crate::postgres_mirror::tests::fresh_schema().await else {
        return;
    };
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    let surreal = crate::test_support::mem_surreal().await;
    let entered = format!("Cased.Person-{}@Example.test", Uuid::now_v7());
    let person = super::create(&pg, &surreal, &NewPerson::new("Cased Person", &entered))
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
            &surreal,
            &NewPerson::new("Variant", entered.to_uppercase())
        )
        .await,
        Err(PersonError::EmailTaken)
    ));

    // A change only in letter case is written, and the mailbox stays put.
    let recased = entered.to_lowercase();
    let edited = super::edit(
        &pg,
        &surreal,
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
}
