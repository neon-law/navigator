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
