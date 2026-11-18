use super::{PostgresDb, PostgresError};

#[tokio::test]
async fn absent_postgres_configuration_cannot_select_source() {
    const CHILD: &str = "NAVIGATOR_TEST_APPLICATION_CONFIGURATION";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "postgres::application_authority_tests::absent_postgres_configuration_cannot_select_source", "--nocapture"])
            .env(CHILD, "child")
            .env_remove("NAVIGATOR_POSTGRES_HOST")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert!(matches!(
        PostgresDb::configured().await,
        Err(PostgresError::Configuration)
    ));
    let source = crate::surreal::SurrealDb::uninitialized();
    assert!(matches!(
        crate::persons::find_by_id(&source, uuid::Uuid::nil()).await,
        Err(crate::persons::PersonError::Postgres(_))
    ));
}

#[tokio::test]
async fn application_history_rejects_unenrolled_database() {
    let (options, database) = crate::postgres_mirror::tests::fresh_schema()
        .await
        .expect("required PostgreSQL application fixture");
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    assert!(matches!(
        pg.require_application_history().await,
        Err(PostgresError::History)
    ));
    sqlx::query("INSERT INTO mirror_backfill_run (id,batch_size,status,scope,completed_at) VALUES ($1,1,'completed','global',now())")
        .bind(uuid::Uuid::new_v4()).execute(pg.pool()).await.unwrap();
    assert!(matches!(
        pg.require_application_history().await,
        Err(PostgresError::History)
    ));
    let families = crate::postgres_backfill::family::Family::all();
    for (index, family) in families.iter().enumerate() {
        super::fixtures::activate_family(&pg, *family).await;
        assert_eq!(
            pg.require_application_history().await.is_ok(),
            index + 1 == families.len()
        );
    }
}

#[tokio::test]
async fn people_create_alias_and_merge_do_not_require_source() {
    let (options, database) = crate::postgres_mirror::tests::fresh_schema()
        .await
        .expect("required PostgreSQL application fixture");
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    super::fixtures::activate_application(&pg).await;
    PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::surreal::SurrealDb::uninitialized();
        let keep = crate::persons::create(
            &source,
            &crate::persons::NewPerson::new("Synthetic Keep", "keep@example.invalid"),
        )
        .await
        .unwrap();
        let drop = crate::persons::create(
            &source,
            &crate::persons::NewPerson::new("Synthetic Drop", "drop@example.invalid"),
        )
        .await
        .unwrap();
        crate::persons::add_alias(&source, drop.id, "alias@example.invalid")
            .await
            .unwrap();
        let report = crate::person_merge::merge(&source, keep.id, drop.id)
            .await
            .unwrap();
        assert_eq!(report.kept_id, keep.id);
        assert!(report.emails.contains(&"alias@example.invalid".into()));
        assert_eq!(
            crate::persons::find_by_any_email(&source, "DROP@example.invalid")
                .await
                .unwrap()
                .unwrap()
                .id,
            keep.id
        );
        assert!(crate::persons::find_by_id(&source, drop.id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            crate::persons::find_by_any_email(&source, "ALIAS@example.invalid")
                .await
                .unwrap()
                .unwrap()
                .id,
            keep.id
        );
    })
    .await;
}

#[tokio::test]
async fn deployment_change_retains_exact_authenticated_timestamp_bytes() {
    use base64::Engine as _;
    use cloud::RuntimeKms as _;
    let (options, database) = crate::postgres_mirror::tests::fresh_schema()
        .await
        .expect("required PostgreSQL fixture");
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    super::fixtures::activate_application(&pg).await;
    let id = uuid::Uuid::new_v4();
    let actor = uuid::Uuid::new_v4().to_string().to_uppercase();
    let timestamp = "2026-11-18T09:04:03.123456789-05:00";
    let key = "NAVIGATOR_META_APP_SECRET";
    let context = cloud::KmsContext::new(
        id,
        "production-sops-change",
        format!("{key}\n{timestamp}\n{actor}"),
    );
    let kms = cloud::FakeKms::new("synthetic-runtime-key");
    let data_key = [17; 32];
    let wrapped = kms.wrap_data_key(&data_key, &context).await.unwrap();
    let ciphertext = cloud::seal(b"synthetic-value", &data_key, &context).unwrap();
    let base64 = base64::engine::general_purpose::STANDARD;
    sqlx::query("INSERT INTO deployment_secret_change (id,request_id,actor_id,key,ciphertext,wrapped_key,kms_key_version,inserted_at,inserted_at_text,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$8)")
        .bind(id).bind(id.to_string()).bind(actor).bind(key).bind(base64.encode(ciphertext))
        .bind(base64.encode(wrapped.ciphertext)).bind(wrapped.key_version)
        .bind(chrono::DateTime::parse_from_rfc3339(timestamp).unwrap()).bind(timestamp)
        .execute(pg.pool()).await.unwrap();
    PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::surreal::SurrealDb::uninitialized();
        let rows = crate::deployment_secret_changes::latest(
            &source,
            crate::persons::Role::Owner,
            crate::DeploymentEnvironment::Production,
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].inserted_at, timestamp);
        assert!(crate::deployment_secret_changes::open(&kms, &rows[0])
            .await
            .is_ok_and(|value| value == "synthetic-value"));
        let mut changed = rows[0].clone();
        changed.inserted_at = "2026-11-18T14:04:03.123456Z".into();
        assert!(crate::deployment_secret_changes::open(&kms, &changed)
            .await
            .is_err());
        sqlx::query("UPDATE deployment_secret_change SET inserted_at_text=NULL")
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(crate::deployment_secret_changes::latest(
            &source,
            crate::persons::Role::Owner,
            crate::DeploymentEnvironment::Production
        )
        .await
        .is_err());
    })
    .await;
}

#[test]
fn source_forbidden_boundary_rejects_sdk_access_before_any_engine() {
    const CHILD: &str = "NAVIGATOR_TEST_FORBIDDEN_SOURCE_ACCESS";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "postgres::application_authority_tests::source_forbidden_boundary_rejects_sdk_access_before_any_engine", "--nocapture"])
            .env(CHILD,"child").env("NAVIGATOR_FORBID_SURREAL","1").output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let source = crate::surreal::SurrealDb::uninitialized();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| &*source)).is_err());
    assert_eq!(
        crate::surreal::record_uuid(&crate::surreal::record_id("person", uuid::Uuid::nil())),
        Some(uuid::Uuid::nil())
    );
}

async fn application_fixture() -> (PostgresDb, uuid::Uuid, uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let (options, database) = crate::postgres_mirror::tests::fresh_schema()
        .await
        .expect("required PostgreSQL fixture");
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    super::fixtures::activate_application(&pg).await;
    let (person, notation, _question) = super::fixtures::workflow_fixture(&pg).await;
    let (firm, project) = super::fixtures::firm_for_notation(&pg, notation).await;
    let brand = uuid::Uuid::new_v4();
    let key = brand.to_string();
    sqlx::query("INSERT INTO brand(id,name,brand_key,firm_id) VALUES ($1,'Synthetic Brand',$2,$3)")
        .bind(brand)
        .bind(&key)
        .bind(firm)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE project SET brand=$2 WHERE id=$1")
        .bind(project)
        .bind(&key)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE person SET role='owner',email_confirmed=true WHERE id=$1")
        .bind(person)
        .execute(pg.pool())
        .await
        .unwrap();

    (pg, person, notation, project, firm)
}

#[tokio::test]
async fn scheduled_completion_does_not_publish_other_states() {
    let (pg, _, _, project, _) = application_fixture().await;
    let brand: String = sqlx::query_scalar("SELECT brand FROM project WHERE id=$1")
        .bind(project)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let mut rows = Vec::new();
    for status in ["draft", "scheduled", "published", "cancelled"] {
        let id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO post(id,brand_key,slug,title,description,status,publish_at) VALUES ($1,$2,$3,'Synthetic','Synthetic',$4,'2026-11-18T01:00:00Z')")
            .bind(id).bind(&brand).bind(id.to_string()).bind(status).execute(pg.pool()).await.unwrap();
        rows.push((id, status));
    }
    PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::surreal::SurrealDb::uninitialized();
        crate::posts::publish_scheduled(&source, uuid::Uuid::nil())
            .await
            .unwrap();
        for (id, status) in rows {
            crate::posts::publish_scheduled(&source, id).await.unwrap();
            crate::posts::publish_scheduled(&source, id).await.unwrap();
            let (actual, at): (String, chrono::DateTime<chrono::Utc>) =
                sqlx::query_as("SELECT status,publish_at FROM post WHERE id=$1")
                    .bind(id)
                    .fetch_one(pg.pool())
                    .await
                    .unwrap();
            assert_eq!(
                actual,
                if status == "scheduled" {
                    "published"
                } else {
                    status
                }
            );
            assert_eq!(at.to_rfc3339(), "2026-11-18T01:00:00+00:00");
        }
    })
    .await;
}

#[tokio::test]
async fn physical_mail_replay_is_native_and_preserves_the_recorded_piece() {
    let (pg, person, _, project, _) = application_fixture().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::surreal::SurrealDb::uninitialized();
        let address = crate::addresses::create(
            &source,
            &crate::addresses::NewAddress {
                person_id: Some(person),
                line1: "Synthetic Line".into(),
                city: "Synthetic City".into(),
                region: "ST".into(),
                postal_code: "00000".into(),
                country: "US".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let room = crate::mailrooms::find_or_create(&source, "Synthetic Room", address.id)
            .await
            .unwrap();
        assert_eq!(
            crate::mailrooms::find_or_create(&source, "Synthetic Room", address.id)
                .await
                .unwrap(),
            room
        );
        assert_eq!(crate::mailrooms::count(&source).await.unwrap(), 1);
        let id = uuid::Uuid::new_v4();
        let mut input = crate::letters::NewLetter {
            mailroom_id: room.id,
            project_id: Some(project),
            direction: crate::letters::DIRECTION_INCOMING.into(),
            sender: "Synthetic Sender".into(),
            recipient: "Synthetic Recipient".into(),
            summary: "Synthetic summary".into(),
        };
        let letter = crate::letters::record_with_id(&source, id, &input)
            .await
            .unwrap();
        assert_eq!(
            crate::letters::record_with_id(&source, id, &input)
                .await
                .unwrap(),
            letter
        );
        input.summary = "Different synthetic summary".into();
        assert!(matches!(
            crate::letters::record_with_id(&source, id, &input).await,
            Err(crate::letters::LetterError::ReplayConflict)
        ));
        assert_eq!(
            crate::letters::find_by_id(&source, id).await.unwrap(),
            Some(letter)
        );
        assert_eq!(crate::letters::count(&source).await.unwrap(), 1);
        assert_eq!(crate::letters::list_all(&source).await.unwrap().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn concurrent_consent_preserves_one_lead_and_immutable_events() {
    let (pg, _, _, _, _) = application_fixture().await;
    Box::pin(PostgresDb::with_test_db(pg.clone(),async {
        let source=crate::surreal::SurrealDb::uninitialized();
        let now=chrono::Utc::now();
        let input=crate::leads::NewLead {email:"lead@example.invalid".into(),phone:None,
            brand_key:"synthetic".into(),source_path:"/synthetic".into(),consent_version:"synthetic-v1".into(),consented_at:now};
        let event=crate::leads::NewConsentEvent {consented_at:now,phone:None,source_path:"/synthetic".into(),
            sms_consent_version:"synthetic-v1".into(),sms_policy_version:"synthetic-v1".into(),sms_opt_in:false,
            call_consent_version:"synthetic-v1".into(),call_opt_in:false};
        let (left,right)=tokio::join!(crate::leads::record_consent_event(&source,&input,&event),crate::leads::record_consent_event(&source,&input,&event));
        let left=left.unwrap();let right=right.unwrap();
        assert_eq!(left.id,right.id);
        let lead=crate::leads::find(&source,left.id).await.unwrap().unwrap();
        assert_eq!(lead.submissions,2);
        assert_eq!(crate::leads::list(&source).await.unwrap().len(),1);
        assert_eq!(crate::leads::consent_history(&source,left.id).await.unwrap().len(),2);
        assert!(crate::leads::latest_consent_event(&source,left.id).await.unwrap().is_some());
        sqlx::raw_sql("CREATE FUNCTION reject_synthetic_consent() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic consent failure''; END'; CREATE TRIGGER reject_synthetic_consent BEFORE INSERT ON lead_consent FOR EACH ROW EXECUTE FUNCTION reject_synthetic_consent()")
            .execute(pg.pool()).await.unwrap();
        assert!(crate::leads::record_consent_event(&source,&input,&event).await.is_err());
        assert_eq!(crate::leads::find(&source,left.id).await.unwrap().unwrap().submissions,2);
        assert_eq!(crate::leads::consent_history(&source,left.id).await.unwrap().len(),2);
    })).await;
}

#[tokio::test]
async fn person_merge_rechecks_locked_role_and_preserves_historical_primary_atomically() {
    use crate::{
        person_merge::{self, PersonMergeError},
        persons::{self, NewPerson, Role},
    };
    let (pg, _, _, _, _) = application_fixture().await;
    PostgresDb::with_test_db(pg.clone(),async {
        let source=crate::surreal::SurrealDb::uninitialized();
        let keep=persons::create(&source,&NewPerson::new("Synthetic Keep","merge-keep@example.invalid")).await.unwrap();
        let dropped=persons::create(&source,&NewPerson::new("Synthetic Drop","merge-drop@example.invalid")).await.unwrap();
        persons::add_alias(&source,dropped.id,"merge-alias@example.invalid").await.unwrap();
        let mut writer=pg.pool().begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(1312902736,hashtext($1::text))").bind(dropped.id).execute(&mut *writer).await.unwrap();
        let task_pg=pg.clone();let task_source=source.clone();
        let merge=tokio::spawn(PostgresDb::with_test_db(task_pg,async move {person_merge::merge(&task_source,keep.id,dropped.id).await}));
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            loop {
                let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=1312902736 AND objid::bigint=(hashtext($1::text)::bigint & 4294967295))")
                    .bind(dropped.id).fetch_one(pg.pool()).await.unwrap();
                if waiting {break;}
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.expect("merge waits for the cooperating role writer");
        sqlx::query("UPDATE person SET role='lawyer',updated_at=now() WHERE id=$1").bind(dropped.id).execute(&mut *writer).await.unwrap();
        writer.commit().await.unwrap();
        assert!(matches!(merge.await.unwrap(),Err(PersonMergeError::Blocked)));
        assert!(persons::find_by_id(&source,dropped.id).await.unwrap().is_some());
        persons::set_role(&source,dropped.id,Role::Client).await.unwrap();
        sqlx::query("DELETE FROM person_mailbox WHERE id=lower($1)").bind(&dropped.email).execute(pg.pool()).await.unwrap();
        sqlx::raw_sql("CREATE FUNCTION reject_synthetic_merge() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic merge failure''; END'; CREATE TRIGGER reject_synthetic_merge BEFORE DELETE ON person FOR EACH ROW EXECUTE FUNCTION reject_synthetic_merge()")
            .execute(pg.pool()).await.unwrap();
        assert!(person_merge::merge(&source,keep.id,dropped.id).await.is_err());
        let claims:Vec<String>=sqlx::query_scalar("SELECT id FROM person_mailbox WHERE person_id=$1 ORDER BY id").bind(dropped.id).fetch_all(pg.pool()).await.unwrap();
        assert_eq!(claims,vec!["merge-alias@example.invalid"]);
        assert!(persons::find_by_id(&source,dropped.id).await.unwrap().is_some());
        sqlx::raw_sql("DROP TRIGGER reject_synthetic_merge ON person; DROP FUNCTION reject_synthetic_merge()").execute(pg.pool()).await.unwrap();
        let report=person_merge::merge(&source,keep.id,dropped.id).await.unwrap();
        assert!(report.emails.contains(&dropped.email));
        for address in [dropped.email.as_str(),"MERGE-ALIAS@example.invalid"] {
            assert_eq!(persons::find_by_any_email(&source,address).await.unwrap().unwrap().id,keep.id);
        }
        assert_eq!(persons::find_by_id(&source,keep.id).await.unwrap().unwrap().email,keep.email);
    }).await;
}

#[tokio::test]
async fn person_merge_rejects_foreign_primary_claim_and_handles_phone_only_people() {
    use crate::{
        person_merge,
        persons::{self, NewPerson},
    };
    let (pg, _, _, _, _) = application_fixture().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::surreal::SurrealDb::uninitialized();
        let keep = persons::create(
            &source,
            &NewPerson::new("Synthetic Keep", "claim-keep@example.invalid"),
        )
        .await
        .unwrap();
        let dropped = persons::create(
            &source,
            &NewPerson::new("Synthetic Drop", "claim-drop@example.invalid"),
        )
        .await
        .unwrap();
        let third = persons::create(
            &source,
            &NewPerson::new("Synthetic Third", "claim-third@example.invalid"),
        )
        .await
        .unwrap();
        persons::add_alias(&source, dropped.id, "claim-alias@example.invalid")
            .await
            .unwrap();
        sqlx::query("UPDATE person_mailbox SET person_id=$1 WHERE id=$2")
            .bind(third.id)
            .bind(&dropped.email)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(matches!(
            person_merge::merge(&source, keep.id, dropped.id).await,
            Err(person_merge::PersonMergeError::Person(
                persons::PersonError::EmailTaken
            ))
        ));
        assert!(persons::find_by_id(&source, dropped.id)
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            persons::find_by_any_email(&source, "claim-alias@example.invalid")
                .await
                .unwrap()
                .unwrap()
                .id,
            dropped.id
        );
        let holder: uuid::Uuid =
            sqlx::query_scalar("SELECT person_id FROM person_mailbox WHERE id=$1")
                .bind(&dropped.email)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(holder, third.id);
        sqlx::query("DELETE FROM person_mailbox WHERE id=$1")
            .bind(&dropped.email)
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE person SET email='' WHERE id=$1")
            .bind(dropped.id)
            .execute(pg.pool())
            .await
            .unwrap();
        person_merge::merge(&source, keep.id, dropped.id)
            .await
            .unwrap();
        assert_eq!(
            persons::find_by_any_email(&source, "claim-alias@example.invalid")
                .await
                .unwrap()
                .unwrap()
                .id,
            keep.id
        );
        let empty_claims: i64 =
            sqlx::query_scalar("SELECT count(*) FROM person_mailbox WHERE trim(id)=''")
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(empty_claims, 0);
    })
    .await;
}

#[tokio::test]
async fn activated_identity_cannot_be_resurrected_by_global_source_import() {
    let (pg, _, _, _, _) = application_fixture().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::surreal::SurrealDb::uninitialized();
        let person = crate::persons::create(
            &source,
            &crate::persons::NewPerson::new("Synthetic Deleted", "deleted-import@example.invalid"),
        )
        .await
        .unwrap();
        sqlx::query("DELETE FROM person_mailbox WHERE person_id=$1")
            .bind(person.id)
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query("DELETE FROM person WHERE id=$1")
            .bind(person.id)
            .execute(pg.pool())
            .await
            .unwrap();
        crate::postgres_mirror::from_source(crate::postgres_mirror::during_initial_copy(async {
            assert!(crate::postgres_graph::holds("relationship"));
            let outcome = crate::postgres_mirror::mirror(
                &source,
                &pg,
                &crate::postgres_mirror::MirrorKey::Person(person.id),
            )
            .await
            .unwrap();
            assert!(matches!(
                outcome,
                crate::postgres_mirror::Mirrored::Unchanged
            ));
        }))
        .await;
        let present: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM person WHERE id=$1)")
            .bind(person.id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert!(!present);
    })
    .await;
}
