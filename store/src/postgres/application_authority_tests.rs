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
        PostgresDb::application().await,
        Err(PostgresError::Configuration)
    ));
    let source = crate::application_store::ApplicationStore::uninitialized();
    assert!(matches!(
        crate::persons::find_by_id(&source, uuid::Uuid::nil()).await,
        Err(crate::persons::PersonError::Postgres(_))
    ));
}

#[tokio::test]
async fn application_history_rejects_unenrolled_database() {
    let (options, database) = crate::postgres::fixtures::fresh_schema()
        .await
        .expect("required PostgreSQL application fixture");
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
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
    let families = crate::postgres::history::Family::all();
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
    let (options, database) = crate::postgres::fixtures::fresh_schema()
        .await
        .expect("required PostgreSQL application fixture");
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
    super::fixtures::activate_application(&pg).await;
    PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::application_store::ApplicationStore::uninitialized();
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
    let (options, database) = crate::postgres::fixtures::fresh_schema()
        .await
        .expect("required PostgreSQL fixture");
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
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
    let data_key: [u8; 32] = rand::random();
    let wrapped = kms.wrap_data_key(&data_key, &context).await.unwrap();
    let ciphertext = cloud::seal(b"synthetic-value", &data_key, &context).unwrap();
    let base64 = base64::engine::general_purpose::STANDARD;
    sqlx::query("INSERT INTO deployment_secret_change (id,request_id,actor_id,key,ciphertext,wrapped_key,kms_key_version,inserted_at,inserted_at_text,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$8)")
        .bind(id).bind(id.to_string()).bind(actor).bind(key).bind(base64.encode(ciphertext))
        .bind(base64.encode(wrapped.ciphertext)).bind(wrapped.key_version)
        .bind(chrono::DateTime::parse_from_rfc3339(timestamp).unwrap()).bind(timestamp)
        .execute(pg.pool()).await.unwrap();
    PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::application_store::ApplicationStore::uninitialized();
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

async fn application_fixture() -> (PostgresDb, uuid::Uuid, uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let (options, database) = crate::postgres::fixtures::fresh_schema()
        .await
        .expect("required PostgreSQL fixture");
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
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
        let source = crate::application_store::ApplicationStore::uninitialized();
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
        let source = crate::application_store::ApplicationStore::uninitialized();
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
        let source=crate::application_store::ApplicationStore::uninitialized();
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
        let source=crate::application_store::ApplicationStore::uninitialized();
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
        let source = crate::application_store::ApplicationStore::uninitialized();
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

fn native_deadline(project_id: uuid::Uuid) -> crate::project_deadlines::NewDeadline {
    crate::project_deadlines::NewDeadline {
        project_id,
        title: Some("Synthetic deadline".into()),
        description: None,
        hearing: None,
        due_on: None,
        due_at: "2026-12-01T12:00:00Z".into(),
        authority_kind: "statute".into(),
        authority: "Synthetic authority".into(),
        source: "lawyer".into(),
        source_document_id: None,
        owner_person_id: None,
        calendar_role: "firm".into(),
        category: Some("court".into()),
        internal_lead_days: Some(14),
        client_lead_days: Some(7),
        replay_key: Some("synthetic-deadline".into()),
    }
}

async fn wait_for_deadline_writers(pg: &PostgresDb, project: uuid::Uuid, count: i64) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=1312902739 AND objid::bigint=(hashtext($1::text)::bigint & 4294967295)")
                .bind(project).fetch_one(pg.pool()).await.unwrap();
            if waiting >= count { break; }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }).await.expect("deadline operations reach the cooperating Project lock");
}

#[tokio::test]
async fn deadline_civil_date_round_trips_and_reschedules_the_same_record() {
    use crate::project_deadlines as deadlines;
    let (pg, _, _, project, _) = application_fixture().await;
    PostgresDb::with_test_db(pg, async {
        let db = crate::application_store::ApplicationStore::uninitialized();
        let mut input = native_deadline(project);
        let day: deadlines::DueOn = serde_json::from_value(serde_json::json!({"date":"2028-07-12","timezone":"America/Los_Angeles","cutoff":"23:59:00"})).unwrap();
        input.due_at = day.instant().unwrap().to_rfc3339();
        input.due_on = Some(day.clone());
        input.category = Some("tickler".into());
        let first = deadlines::create(&db, &input).await.unwrap();
        assert_eq!(first.due_on.as_ref(), Some(&day));
        assert_eq!(deadlines::create(&db, &input).await.unwrap(), first);
        let updated = deadlines::update(&db, project, first.id, &deadlines::DeadlinePatch {
            description: Some(Some("Reviewed".into())), ..Default::default()
        }).await.unwrap();
        assert_eq!(updated.due_on.as_ref(), Some(&day));
        let timed = deadlines::update(&db, project, first.id, &deadlines::DeadlinePatch {
            due_at: Some("2028-07-12T10:00:00Z".into()), ..Default::default()
        }).await.unwrap();
        assert!(timed.due_on.is_none());
        assert_eq!(timed.id, first.id);
        let hearing = deadlines::DeadlinePatch {
            calendar_role: Some("hearing".into()),
            category: Some(Some("hearing".into())),
            hearing: Some(deadlines::Hearing {duration_minutes: std::num::NonZeroU32::new(90),location:Some("Courtroom A".into())}),
            ..Default::default()
        };
        let saved = deadlines::update(&db, project, timed.id, &hearing).await.unwrap();
        let event = crate::deadline_calendar::deadline_event(&saved,"sample",None).unwrap();
        assert_eq!(event.timing, cloud::calendar::EventTiming::Hearing(90));
        assert_eq!(event.location.as_deref(), Some("Courtroom A"));
        let moved = deadlines::update(&db, project, timed.id, &deadlines::DeadlinePatch {
            hearing:Some(deadlines::Hearing {location:Some("Courtroom B".into()),..Default::default()}),..Default::default()
        }).await.unwrap();
        assert_eq!(moved.hearing.as_ref().unwrap().minutes(), 90);
    }).await;
}

#[tokio::test]
async fn deadline_concurrent_identical_replay_keeps_calendar_revision() {
    let (pg, _, _, project, _) = application_fixture().await;
    let input = native_deadline(project);
    let mut lease = pg.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(1312902739,hashtext($1::text))")
        .bind(project)
        .execute(&mut *lease)
        .await
        .unwrap();
    let spawn = |input| {
        let pg = pg.clone();
        tokio::spawn(PostgresDb::with_test_db(pg, async move {
            crate::project_deadlines::create(
                &crate::application_store::ApplicationStore::uninitialized(),
                &input,
            )
            .await
        }))
    };
    let left = spawn(input.clone());
    let right = spawn(input.clone());
    wait_for_deadline_writers(&pg, project, 2).await;
    lease.commit().await.unwrap();
    let left = left.await.unwrap().unwrap();
    let right = right.await.unwrap().unwrap();
    assert_eq!(left, right);
    assert_eq!(left.calendar_sequence, Some(0));
    PostgresDb::with_test_db(pg, async {
        assert_eq!(
            crate::project_deadlines::create(
                &crate::application_store::ApplicationStore::uninitialized(),
                &input
            )
            .await
            .unwrap(),
            left
        );
    })
    .await;
}

#[tokio::test]
async fn open_deadlines_batch_across_projects_in_postgres() {
    use crate::project_deadlines as deadlines;
    let (pg, _, _, project, _) = application_fixture().await;
    let mut projects = Vec::new();
    for code in ["included", "excluded"] {
        let id = sqlx::query_scalar(
            "INSERT INTO project (code, name, status, entity_id, brand, firm_id)
             SELECT $1, $1, 'open', entity_id, brand, firm_id FROM project WHERE id = $2 RETURNING id",
        )
        .bind(code)
        .bind(project)
        .fetch_one(pg.pool())
        .await
        .unwrap();
        projects.push(id);
    }
    PostgresDb::with_test_db(pg, async {
        let source = crate::application_store::ApplicationStore::uninitialized();
        let open = deadlines::create(&source, &native_deadline(project))
            .await
            .unwrap();
        let mut closed_input = native_deadline(project);
        closed_input.replay_key = Some("closed".into());
        let closed = deadlines::create(&source, &closed_input).await.unwrap();
        deadlines::complete(&source, project, closed.id)
            .await
            .unwrap();
        let mut early = native_deadline(projects[0]);
        early.due_at = "2020-01-01T00:00:00Z".into();
        let early = deadlines::create(&source, &early).await.unwrap();
        deadlines::create(&source, &native_deadline(projects[1]))
            .await
            .unwrap();
        let rows = deadlines::open_for_projects(&[project, projects[0]])
            .await
            .unwrap();
        assert_eq!(
            rows.iter().map(|d| d.id).collect::<Vec<_>>(),
            [early.id, open.id]
        );
        assert!(deadlines::open_for_projects(&[]).await.unwrap().is_empty());
    })
    .await;
}

#[tokio::test]
async fn deadline_changed_replay_advances_revision_and_preserves_cancellation() {
    use crate::project_deadlines as deadlines;
    let (pg, _, _, project, _) = application_fixture().await;
    PostgresDb::with_test_db(pg, async {
        let source = crate::application_store::ApplicationStore::uninitialized();
        let mut input = native_deadline(project);
        let first = deadlines::create(&source, &input).await.unwrap();
        input.due_at = "2026-12-02T12:00:00Z".into();
        let changed = deadlines::create(&source, &input).await.unwrap();
        assert_eq!(changed.id, first.id);
        assert_eq!(changed.calendar_sequence, Some(1));
        let cancelled = deadlines::cancel(&source, project, first.id).await.unwrap();
        assert_eq!(cancelled.calendar_sequence, Some(2));
        assert!(deadlines::create(&source, &input).await.is_err());
        assert_eq!(
            deadlines::find(&source, project, first.id).await.unwrap(),
            Some(cancelled)
        );
        assert!(deadlines::find(&source, uuid::Uuid::new_v4(), first.id)
            .await
            .unwrap()
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn deadline_cancellation_race_refuses_stale_write() {
    use crate::project_deadlines as deadlines;
    let (pg, _, _, project, _) = application_fixture().await;
    let source = crate::application_store::ApplicationStore::uninitialized();
    let created = PostgresDb::with_test_db(
        pg.clone(),
        deadlines::create(&source, &native_deadline(project)),
    )
    .await
    .unwrap();
    let id = created.id;
    let mut lease = pg.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(1312902739,hashtext($1::text))")
        .bind(project)
        .execute(&mut *lease)
        .await
        .unwrap();
    let cancel = tokio::spawn(PostgresDb::with_test_db(pg.clone(), async move {
        deadlines::cancel(
            &crate::application_store::ApplicationStore::uninitialized(),
            project,
            id,
        )
        .await
    }));
    wait_for_deadline_writers(&pg, project, 1).await;
    let update = tokio::spawn(PostgresDb::with_test_db(pg.clone(), async move {
        deadlines::update(
            &crate::application_store::ApplicationStore::uninitialized(),
            project,
            id,
            &deadlines::DeadlinePatch {
                title: Some(Some("Stale edit".into())),
                ..Default::default()
            },
        )
        .await
    }));
    wait_for_deadline_writers(&pg, project, 2).await;
    lease.commit().await.unwrap();
    let cancelled = cancel.await.unwrap().unwrap();
    assert!(matches!(
        update.await.unwrap(),
        Err(deadlines::DeadlineError::Conflict)
    ));
    PostgresDb::with_test_db(pg, async {
        assert_eq!(
            deadlines::find(&source, project, id).await.unwrap(),
            Some(cancelled)
        );
    })
    .await;
}

#[tokio::test]
async fn deadline_identical_replay_compares_the_stored_instant_and_precision() {
    use crate::project_deadlines as deadlines;
    let (pg, _, _, project, _) = application_fixture().await;
    PostgresDb::with_test_db(pg, async {
        let source = crate::application_store::ApplicationStore::uninitialized();
        let mut input = native_deadline(project);
        input.due_at = "2026-12-01T04:00:00.123456789-08:00".into();
        let first = deadlines::create(&source, &input).await.unwrap();
        assert_eq!(first.calendar_sequence, Some(0));
        assert_eq!(deadlines::create(&source, &input).await.unwrap(), first);
        input.due_at = "2026-12-01T12:00:00.123457Z".into();
        assert_eq!(deadlines::create(&source, &input).await.unwrap(), first);
        input.due_at = "2026-12-01T12:00:00.123458Z".into();
        let changed = deadlines::create(&source, &input).await.unwrap();
        assert_eq!(changed.id, first.id);
        assert_eq!(changed.calendar_sequence, Some(1));
    })
    .await;
}

#[tokio::test]
async fn firm_directory_and_ownership_backfill_are_native_and_isolated() {
    use crate::{firms, persons, projects};
    let (pg, actor, _, project, firm) = application_fixture().await;
    Box::pin(PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::application_store::ApplicationStore::uninitialized();
        let (outsider, other_note, _) = super::fixtures::workflow_fixture(&pg).await;
        let (other_firm, other_project) = super::fixtures::firm_for_notation(&pg, other_note).await;
        let brand: String = sqlx::query_scalar("SELECT brand FROM project WHERE id=$1")
            .bind(project).fetch_one(pg.pool()).await.unwrap();
        firms::ensure_brand(&source, firm, &brand).await.unwrap();
        firms::ensure_brand(&source, firm, &brand).await.unwrap();
        assert!(matches!(firms::ensure_brand(&source, other_firm, &brand).await,
            Err(firms::FirmError::DuplicateBrand)));
        sqlx::query("INSERT INTO person_firm_role(id,person_id,firm_id,membership,is_dri) VALUES($1,$2,$3,'admin',true)")
            .bind(uuid::Uuid::new_v4()).bind(actor).bind(firm).execute(pg.pool()).await.unwrap();
        let client = persons::create(&source, &persons::NewPerson::new("Synthetic Contact", "firm-contact@example.invalid")).await.unwrap();
        projects::add_participation(&source, project, client.id, "client").await.unwrap();
        let visible = firms::visible_person_ids(&source, actor).await.unwrap();
        assert!(visible.contains(&actor));
        assert!(visible.contains(&client.id));
        assert!(!visible.contains(&outsider));
        let report = firms::admin_dri_invariant_report(&source).await.unwrap();
        assert!(report.iter().find(|row| row.firm_id == firm).unwrap().problem.is_none());
        assert!(matches!(report.iter().find(|row| row.firm_id == other_firm).unwrap().problem,
            Some(firms::AdminDriProblem::Missing)));
        sqlx::query("UPDATE project SET firm_id=NULL WHERE id=$1").bind(project).execute(pg.pool()).await.unwrap();
        assert_eq!(firms::backfill_unowned_projects(&source, None).await.unwrap(), 1);
        assert_eq!(firms::backfill_unowned_projects(&source, None).await.unwrap(), 0);
        let owners: Vec<(uuid::Uuid, Option<uuid::Uuid>)> = sqlx::query_as("SELECT id,firm_id FROM project WHERE id=ANY($1::uuid[]) ORDER BY id")
            .bind(vec![project, other_project]).fetch_all(pg.pool()).await.unwrap();
        assert!(owners.contains(&(project, Some(firm))));
        assert!(owners.contains(&(other_project, Some(other_firm))));
    })).await;
}

#[tokio::test]
async fn high_level_matter_open_requires_attestation_and_native_brand_ownership() {
    use crate::{entities, firms, persons, projects};
    let (pg, actor, _, existing_project, firm) = application_fixture().await;
    Box::pin(PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::application_store::ApplicationStore::uninitialized();
        let brand: String = sqlx::query_scalar("SELECT brand FROM project WHERE id=$1")
            .bind(existing_project).fetch_one(pg.pool()).await.unwrap();
        firms::ensure_brand(&source, firm, &brand).await.unwrap();
        let (entity_type_id, jurisdiction_id): (uuid::Uuid, uuid::Uuid) = sqlx::query_as("SELECT entity_type_id,jurisdiction_id FROM entity WHERE id=(SELECT entity_id FROM project WHERE id=$1)")
            .bind(existing_project).fetch_one(pg.pool()).await.unwrap();
        sqlx::query("UPDATE jurisdiction SET code=$2,jurisdiction_type='state' WHERE id=$1")
            .bind(jurisdiction_id).bind(jurisdiction_id.to_string()).execute(pg.pool()).await.unwrap();
        let entity = entities::create(&source, &entities::NewEntity {
            name: "Synthetic open client entity".into(), entity_type_id, jurisdiction_id,
            phone: None, url: None, xero_id: None, firm_anchor_key: None,
        }).await.unwrap();
        let client = persons::create(&source, &persons::NewPerson::new("Synthetic open client", "open-client@example.invalid")).await.unwrap();
        let mut command = projects::OpenMatterCommand {
            firm_id: Some(firm), name: "Synthetic native opening".into(), code: "native-opening".into(),
            client_id: client.id, entity_id: entity.id, jurisdiction_id: Some(jurisdiction_id),
            description: None, brand, attestation: false, acting_person_id: actor, closed_at: None,
        };
        assert!(matches!(projects::open_matter(&source, &command).await, Err(projects::OpenMatterError::AttestationRequired)));
        assert!(projects::find_by_code(&source, &command.code).await.unwrap().is_none());
        command.attestation = true;
        command.firm_id = Some(uuid::Uuid::new_v4());
        assert!(matches!(projects::open_matter(&source, &command).await, Err(projects::OpenMatterError::Invalid(_))));
        assert!(projects::find_by_code(&source, &command.code).await.unwrap().is_none());
        command.firm_id = Some(firm);
        let opened = projects::open_matter(&source, &command).await.unwrap();
        assert_eq!(opened.firm_id, Some(firm));
        let roles = projects::participations_for_project(&source, opened.id).await.unwrap();
        assert_eq!(roles.len(), 2);
        assert!(roles.iter().any(|row| row.person_id == actor && row.is_lawyer_dri));
        assert!(roles.iter().any(|row| row.person_id == client.id && row.is_client_dri));
        let attestations: i64 = sqlx::query_scalar("SELECT count(*) FROM relationship_log WHERE subject_type='project' AND subject_id=$1 AND actor_person_id=$2 AND action='conflict_attestation'")
            .bind(opened.id).bind(actor).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(attestations, 1);
        assert!(matches!(projects::open_matter(&source, &command).await, Err(projects::OpenMatterError::CodeConflict)));
        assert_eq!(projects::participations_for_project(&source, opened.id).await.unwrap(), roles);
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM relationship_log WHERE subject_type='project' AND subject_id=$1 AND action='conflict_attestation'")
            .bind(opened.id).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(after, 1);
    })).await;
}

#[tokio::test]
async fn clause_catalog_requires_history_and_never_hides_custom_versions_on_failure() {
    let (options, database) = crate::postgres::fixtures::fresh_schema()
        .await
        .expect("required PostgreSQL fixture");
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
    PostgresDb::with_test_db(pg.clone(), async {
        assert!(matches!(
            crate::clauses::catalog().await,
            Err(crate::clauses::ClauseError::Postgres(
                PostgresError::History
            ))
        ));
        super::fixtures::activate_application(&pg).await;
        let mut custom = rules::clauses::library_clauses().unwrap().remove(0);
        custom.code = "synthetic_custom_catalog".into();
        custom.version = 1;
        custom.retired_at = None;
        custom.body = "Synthetic custom Clause version".into();
        crate::clauses::seed(&pg, &[custom]).await.unwrap();
        let rows = crate::clauses::catalog().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].code, "synthetic_custom_catalog");
        assert_eq!(rows[0].body, "Synthetic custom Clause version");
        sqlx::query("ALTER TABLE clause RENAME TO synthetic_unavailable_clause")
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(matches!(
            crate::clauses::catalog().await,
            Err(crate::clauses::ClauseError::Query)
        ));
        assert!(crate::clauses::bundled_catalog()
            .unwrap()
            .iter()
            .all(|clause| clause.code != "synthetic_custom_catalog"));
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn notation_creation_history_never_skips_unconfigured_or_incomplete_application_admission() {
    use super::notation_creation_attempts as attempts;
    let (options, database) = crate::postgres::fixtures::fresh_schema()
        .await
        .expect("required PostgreSQL fixture");
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
    PostgresDb::with_test_db(pg.clone(), async {
        assert!(matches!(
            attempts::database().await,
            Err(PostgresError::History)
        ));
        super::fixtures::activate_application(&pg).await;
        let (person, notation, _) = super::fixtures::workflow_fixture(&pg).await;
        let project = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
            .bind(notation)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let admitted = attempts::database().await.unwrap();
        attempts::record_failure(&admitted, project, "synthetic", person)
            .await
            .unwrap();
        assert_eq!(
            attempts::unresolved(&admitted, project)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(attempts::unresolved(&admitted, uuid::Uuid::now_v7())
            .await
            .unwrap()
            .is_empty());
        sqlx::query("DELETE FROM mirror_backfill_checkpoint WHERE mirror='table:journal_entry'")
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(matches!(
            attempts::database().await,
            Err(PostgresError::History)
        ));
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn verified_plan_terms_follow_the_selected_benefit() {
    use crate::{firms, legal_plans, projects};
    use model::plan_benefit::{PlanBenefitTerms, PlanCapShape};

    let (pg, _, _, project, firm) = application_fixture().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::application_store::ApplicationStore::uninitialized();
        let brand: String = sqlx::query_scalar("SELECT brand FROM project WHERE id=$1")
            .bind(project)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        firms::ensure_brand(&source, firm, &brand).await.unwrap();
        let plan = legal_plans::create(
            &pg,
            firm,
            &legal_plans::NewLegalPlan {
                name: "Legal Ease".into(),
                contact: None,
                portal: None,
                acceptance_sla_hours: None,
                turnaround_sla_days: None,
                reporting_requirements: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            legal_plans::find_by_name(&pg, firm, "LegalEASE")
                .await
                .unwrap()
                .unwrap()
                .id,
            plan.id
        );
        let mut columns = projects::PlanColumns {
            legal_plan_id: Some(plan.id),
            benefit_code: Some("SAMPLE-BENEFIT".into()),
            ..Default::default()
        };
        let terms = PlanBenefitTerms {
            cap_shape: PlanCapShape::HourlyCap,
            cap_minutes: Some(240),
            cap_cents: None,
            contested_cap_cents: None,
            plan_rate_cents_per_hour: Some(9000),
            paralegal_rate_cents_per_hour: Some(4500),
        };
        let read = || legal_plans::project_benefit_terms(&pg, firm, project);
        projects::set_plan_columns_with_benefit(&source, &pg, project, &columns, Some(&terms))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read().await.unwrap(), Some(terms.clone()));
        columns.plan_member_id = Some("SAMPLE-MEMBER".into());
        projects::set_plan_columns(&source, &pg, project, &columns)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read().await.unwrap(), Some(terms.clone()));
        let invalid = PlanBenefitTerms {
            cap_cents: Some(5000),
            ..terms.clone()
        };
        assert!(projects::set_plan_columns_with_benefit(
            &source,
            &pg,
            project,
            &columns,
            Some(&invalid)
        )
        .await
        .is_err());
        assert_eq!(read().await.unwrap(), Some(terms));
        columns.benefit_code = Some("SAMPLE-OTHER".into());
        projects::set_plan_columns(&source, &pg, project, &columns)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read().await.unwrap(), None);
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the complete provider walks share a native application fixture and verify failure recovery"
)]
async fn accepted_plan_notices_open_one_pitch_per_provider_case() {
    use crate::plan_assignment::{fixtures, parse, PlanProvider};
    use crate::plan_intake::{self, AcceptInput, PlanIntakeError, StageInput};
    use crate::{firms, legal_plans, persons, projects, relationship_logs};
    use chrono::Utc;
    use uuid::Uuid;

    let (pg, _, _, existing_project, firm) = application_fixture().await;
    Box::pin(PostgresDb::with_test_db(pg.clone(), async {
        let source = crate::application_store::ApplicationStore::uninitialized();
        let (brand, occupied_code): (String, String) =
            sqlx::query_as("SELECT brand,code FROM project WHERE id=$1")
                .bind(existing_project).fetch_one(pg.pool()).await.unwrap();
        firms::ensure_brand(&source, firm, &brand).await.unwrap();
        let lawyer = persons::create(&source, &persons::NewPerson::with_role(
            "Synthetic Intake Lawyer", "intake-lawyer@example.invalid", persons::Role::Lawyer,
        )).await.unwrap();
        firms::ensure_membership(&source, &firms::NewPersonFirmRole {
            person_id: lawyer.id, firm_id: firm, membership: firms::FirmMembership::Lawyer, is_dri: false,
        }).await.unwrap();
        let outsider = persons::create(&source, &persons::NewPerson::with_role(
            "Synthetic Outside Lawyer", "outside-lawyer@example.invalid", persons::Role::Lawyer,
        )).await.unwrap();
        assert!(plan_intake::review_firm(&source, outsider.id, &brand).await.is_err());

        for (provider, plan_name, subject, notice, case_number, member_id, email, legacy) in [
            (PlanProvider::Arag, "ARAG group legal", "CaseAssist #4900000-001",
                fixtures::arag_case_request_text(), "4900000-001", None, "arag-member@example.invalid", false),
            (PlanProvider::LegalEase, "Legal Ease", fixtures::LEGAL_EASE_SUBJECT,
                fixtures::legal_ease_case_review_text(), "400001", Some("9900001"), "ease-member@example.invalid", true),
        ] {
            let assignment = parse(provider, subject, &notice).unwrap();
            assert!(assignment.contact_email.is_none());
            let plan = legal_plans::create(&pg, firm, &legal_plans::NewLegalPlan {
                name: plan_name.into(), contact: None, portal: None, acceptance_sla_hours: Some(24),
                turnaround_sla_days: Some(7), reporting_requirements: None,
            }).await.unwrap();
            let stage_input = StageInput {
                firm_id: firm, conversation_id: Uuid::now_v7(), source_message_id: Uuid::now_v7(),
                landing_at: Utc::now(), provider: provider.as_str().into(), assignment,
            };
            let staged = plan_intake::stage(&source, Some(&pg), &stage_input).await.unwrap();
            assert_eq!(staged.plan_case_number.as_deref(), Some(case_number));
            assert_eq!(staged.plan_member_id.as_deref(), member_id);
            assert_eq!(staged.legal_plan_id, Some(plan.id));
            assert!(staged.plan_matter_type.is_empty());
            assert!(staged.fee_plan_fee_cents.is_none());

            if legacy {
                sqlx::query("UPDATE plan_intake SET plan_case_number=NULL, plan_number='CERT-OLD', plan_matter_type='document_review' WHERE id=$1")
                    .bind(staged.id).execute(pg.pool()).await.unwrap();
            }
            let mut input = AcceptInput {
                attestation: true, acting_person_id: lawyer.id, brand: brand.clone(),
                code: format!("synthetic-{}", provider.as_str().replace('_', "-")),
                name: None, plan_case_number: None, plan_matter_type: String::new(), plan_state: None,
            };
            assert!(matches!(plan_intake::accept(&source, &pg, staged.id, &input).await, Err(PlanIntakeError::MissingMemberAnchor)));
            assert!(matches!(plan_intake::resolve_member(&source, &pg, staged.id, outsider.id, &brand, None, email).await, Err(PlanIntakeError::NotFound)));
            plan_intake::resolve_member(&source, &pg, staged.id, lawyer.id, &brand, None, email).await.unwrap();
            let anchored = plan_intake::find_by_id(&source, staged.id).await.unwrap().unwrap();
            sqlx::query("UPDATE plan_intake SET conflict_checked=false WHERE id=$1")
                .bind(staged.id).execute(pg.pool()).await.unwrap();
            plan_intake::resolve_member(&source, &pg, staged.id, lawyer.id, &brand, None, email).await.unwrap();
            let reviewed = plan_intake::find_by_id(&source, staged.id).await.unwrap().unwrap();
            assert!(reviewed.conflict_checked);
            assert_eq!(reviewed.member_entity_id, anchored.member_entity_id);
            if legacy {
                input.plan_case_number = Some("CERT-OLD".into());
                assert!(matches!(plan_intake::accept(&source, &pg, staged.id, &input).await, Err(PlanIntakeError::MissingCaseNumber)));
                input.plan_case_number = Some(case_number.into());
            }
            input.attestation = false;
            assert!(matches!(plan_intake::accept(&source, &pg, staged.id, &input).await,
                Err(PlanIntakeError::Open(projects::OpenMatterError::AttestationRequired))));
            input.attestation = true;
            let colliding = AcceptInput { code: occupied_code.clone(), ..input.clone() };
            assert!(matches!(plan_intake::accept(&source, &pg, staged.id, &colliding).await,
                Err(PlanIntakeError::Open(projects::OpenMatterError::CodeConflict))));
            if provider == PlanProvider::Arag {
                sqlx::query("ALTER TABLE project_deadlines ADD CONSTRAINT synthetic_turnaround_fault CHECK (title <> 'Plan turnaround deadline') NOT VALID")
                    .execute(pg.pool()).await.unwrap();
                let refused = plan_intake::accept(&source, &pg, staged.id, &input).await.unwrap_err();
                assert!(matches!(
                    refused,
                    PlanIntakeError::Deadline(crate::project_deadlines::DeadlineError::Postgres(
                        super::matter::MatterAuthorityError::Query(ref error)
                    )) if error.as_database_error().and_then(sqlx::error::DatabaseError::code).as_deref() == Some("23514")
                ), "{refused:?}");
                let partial = projects::find_by_code(&source, &input.code).await.unwrap().unwrap();
                assert_eq!(partial.id, staged.id);
                assert!(matches!(plan_intake::decline_from_review(&source, &pg, staged.id, lawyer.id, &brand, "decline").await,
                    Err(PlanIntakeError::NotStaged)));
                assert!(matches!(plan_intake::resolve_member(&source, &pg, staged.id, lawyer.id, &brand, None, "different@example.invalid").await,
                    Err(PlanIntakeError::NotStaged)));
                sqlx::query("ALTER TABLE project_deadlines DROP CONSTRAINT synthetic_turnaround_fault")
                    .execute(pg.pool()).await.unwrap();
            }
            let onboarding_count = std::sync::atomic::AtomicUsize::new(0);
            let start_onboarding = |plan: plan_intake::PlanOnboarding| {
                let count = &onboarding_count;
                let project_id = staged.id;
                async move {
                    assert_eq!(plan.project_id, project_id);
                    assert_eq!(plan.provider, provider.as_str());
                    count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                }
            };
            let (first, retry) = tokio::join!(
                plan_intake::accept_with_onboarding(&source, &pg, staged.id, &input, start_onboarding),
                plan_intake::accept_with_onboarding(&source, &pg, staged.id, &input, start_onboarding)
            );
            let accepted = first.unwrap();
            assert_eq!(retry.unwrap(), accepted);
            assert_eq!(onboarding_count.load(std::sync::atomic::Ordering::Relaxed), 1);
            let opened = projects::find_by_id(&source, accepted.project_id).await.unwrap().unwrap();
            assert_eq!(opened.status, "open");
            assert!(opened.plan_matter_type.is_none());
            assert_eq!(opened.plan_case_number.as_deref(), Some(case_number));
            assert_eq!(opened.plan_member_id.as_deref(), member_id);
            assert_eq!(opened.legal_plan_id, Some(plan.id));
            let roles = projects::participations_for_project(&source, opened.id).await.unwrap();
            assert_eq!(roles.len(), 2);
            assert!(roles.iter().any(|role| role.is_client_dri && Some(role.person_id) == accepted.staged.member_person_id));
            let attestations = relationship_logs::for_project(&source, opened.id).await.unwrap();
            assert_eq!(attestations.iter().filter(|log| log.action == "conflict_attestation" && log.actor_person_id == Some(lawyer.id)).count(), 1);
            assert_eq!(crate::project_deadlines::list(&source, opened.id).await.unwrap().len(), 2);

            let duplicate = plan_intake::stage(&source, Some(&pg), &StageInput {
                source_message_id: Uuid::now_v7(), ..stage_input
            }).await.unwrap();
            plan_intake::resolve_member(&source, &pg, duplicate.id, lawyer.id, &brand, None, email).await.unwrap();
            let duplicate_code = AcceptInput { code: format!("{}-duplicate", input.code), ..input };
            assert!(matches!(plan_intake::accept(&source, &pg, duplicate.id, &duplicate_code).await, Err(PlanIntakeError::DuplicateCase)));
            assert!(projects::find_by_code(&source, &duplicate_code.code).await.unwrap().is_none());
        }
    })).await;
}

#[tokio::test]
async fn a_project_override_and_governing_jurisdiction_precede_the_venue() {
    use crate::projects::calendar_timezone;
    use crate::{cases, jurisdictions, projects};
    use chrono_tz::America;
    let (pg, _, _, id, _) = application_fixture().await;
    PostgresDb::with_test_db(pg, async {
        let db = crate::application_store::ApplicationStore::uninitialized();
        let row = projects::find_by_id(&db, id).await.unwrap().unwrap();
        assert_eq!(
            calendar_timezone(&db, &row).await.unwrap(),
            America::New_York
        );
        cases::open_case(
            &db,
            &cases::NewCase {
                project_id: row.id,
                caption: "Sample",
                forum: None,
                jurisdiction: Some("CA"),
                docket_number: None,
                judge: None,
                posture: "plaintiff",
            },
        )
        .await
        .unwrap();
        assert_eq!(
            calendar_timezone(&db, &row).await.unwrap(),
            America::Los_Angeles
        );
        let state = jurisdictions::create(
            &db,
            &jurisdictions::NewJurisdiction::new("Illinois", "IL", "state"),
        )
        .await
        .unwrap();
        let row = projects::set_jurisdiction(&db, row.id, Some(state.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            calendar_timezone(&db, &row).await.unwrap(),
            America::Chicago
        );
        let update = |zone: &str| projects::UpdateProjectCommand {
            calendar_timezone: Some(zone.into()),
            ..Default::default()
        };
        assert!(projects::update_project(&db, row.id, &update("fake/zone"))
            .await
            .is_err());
        let saved = projects::update_project(&db, row.id, &update("America/Denver"))
            .await
            .unwrap();
        assert_eq!(
            calendar_timezone(&db, &saved).await.unwrap(),
            America::Denver
        );
        let saved = projects::update_project(&db, row.id, &update(""))
            .await
            .unwrap();
        assert_eq!(
            calendar_timezone(&db, &saved).await.unwrap(),
            America::Chicago
        );
    })
    .await;
}

#[tokio::test]
async fn court_date_approval_survives_replay_and_preserves_the_effective_deadline() {
    use crate::deadline_proposals as queue;
    use model::deadline_proposals::{Decision, Draft, Status};
    use uuid::Uuid;
    let (pg, lawyer, _, project, _) = application_fixture().await;
    sqlx::query("INSERT INTO person_project_role(project_id,person_id,participation) VALUES($1,$2,'lawyer')").bind(project).bind(lawyer).execute(pg.pool()).await.unwrap();
    let order = Uuid::now_v7();
    let hash = "a".repeat(64);
    sqlx::query("INSERT INTO asset(id,project_id,kind,storage_key,sha256_hex,content_type,byte_size) VALUES($1,$2,'order','synthetic',$3,'application/pdf',123)").bind(order).bind(project).bind(&hash).execute(pg.pool()).await.unwrap();
    let mut draft:Draft=serde_json::from_value(serde_json::json!({"kind":"signed_order","title":"Synthetic court date","timezone":"America/New_York","due_on":"2027-03-10","evidence":[]})).unwrap();
    let id = Uuid::new_v4();
    let first = queue::stage(&pg, project, id, None, &draft).await.unwrap();
    assert_eq!(first.status, Status::Pending);
    assert!(matches!(
        queue::decide(&pg, project, id, lawyer, 1, Decision::Confirm).await,
        Err(queue::Error::Invalid(_))
    ));
    assert!(queue::get(&pg, Uuid::new_v4(), id).await.unwrap().is_none());
    draft.evidence.push(serde_json::from_value(serde_json::json!({"role":"signed_order","id":order,"sha256_hex":hash,"page":1,"rects":[{"x":0.1,"y":0.1,"width":0.3,"height":0.1}]})).unwrap());
    assert!(queue::stage(&pg, Uuid::new_v4(), id, None, &draft)
        .await
        .is_err());
    let amended = queue::correct(&pg, project, id, 1, &draft).await.unwrap();
    assert_eq!(amended.revision, 2);
    assert!(matches!(
        queue::decide(&pg, project, id, lawyer, 1, Decision::Confirm).await,
        Err(queue::Error::Conflict)
    ));
    assert!(matches!(
        queue::decide(&pg, project, id, Uuid::new_v4(), 2, Decision::Confirm).await,
        Err(queue::Error::Forbidden)
    ));
    let approved = queue::decide(&pg, project, id, lawyer, 2, Decision::Confirm)
        .await
        .unwrap();
    assert_eq!(approved.status, Status::Approved);
    assert!(approved.deadline_id.is_none());
    let job = queue::approved(&pg).await.unwrap().pop().unwrap();
    let (a, b) = tokio::join!(queue::publish(&pg, job), queue::publish(&pg, job));
    a.unwrap();
    b.unwrap();
    let published = queue::decide(&pg, project, id, lawyer, 2, Decision::Confirm)
        .await
        .unwrap();
    let target = published.deadline_id.unwrap();
    assert_eq!(published.status, Status::Published);
    draft.due_on = Some("2027-03-12".parse().unwrap());
    draft.timezone = chrono_tz::America::Chicago;
    queue::correct(&pg, project, id, 2, &draft).await.unwrap();
    assert_revision_timezones(&pg, project, id, &draft).await;
    let day = |pg: PostgresDb| async move {
        sqlx::query_as::<_, (String, bool)>(
            "SELECT due_on->>'date',calendar_review_pending FROM project_deadlines WHERE id=$1",
        )
        .bind(target)
        .fetch_one(pg.pool())
        .await
        .unwrap()
    };
    assert_eq!(day(pg.clone()).await, ("2027-03-10".to_string(), false));
    queue::decide(
        &pg,
        project,
        id,
        lawyer,
        3,
        Decision::Return {
            reason: "Check source".into(),
        },
    )
    .await
    .unwrap();
    assert!(queue::approved(&pg).await.unwrap().is_empty());
    queue::correct(&pg, project, id, 3, &draft).await.unwrap();
    queue::decide(&pg, project, id, lawyer, 4, Decision::Confirm)
        .await
        .unwrap();
    queue::publish(&pg, queue::approved(&pg).await.unwrap()[0])
        .await
        .unwrap();
    assert_eq!(
        queue::get(&pg, project, id)
            .await
            .unwrap()
            .unwrap()
            .deadline_id,
        Some(target)
    );
    assert_eq!(day(pg.clone()).await, ("2027-03-12".to_string(), false));
    assert_direct_court_writes_require_review(pg, project, target).await;
}

async fn assert_direct_court_writes_require_review(
    pg: PostgresDb,
    project: uuid::Uuid,
    target: uuid::Uuid,
) {
    PostgresDb::with_test_db(pg, async {
        let src = crate::application_store::ApplicationStore::uninitialized();
        let mut input = native_deadline(project);
        input.category = Some("court".into());
        let provisional = crate::project_deadlines::create(&src, &input)
            .await
            .unwrap();
        assert!(provisional.calendar_review_pending);
        assert!(matches!(
            crate::project_deadlines::update(
                &src,
                project,
                target,
                &crate::project_deadlines::DeadlinePatch {
                    due_at: Some("2030-01-01T12:00:00Z".into()),
                    ..Default::default()
                }
            )
            .await,
            Err(crate::project_deadlines::DeadlineError::Invalid(_))
        ));
    })
    .await;
}

async fn assert_revision_timezones(
    pg: &PostgresDb,
    project: uuid::Uuid,
    id: uuid::Uuid,
    draft: &model::deadline_proposals::Draft,
) {
    let mut unzoned = serde_json::to_value(draft).unwrap();
    unzoned.as_object_mut().unwrap().remove("timezone");
    let unzoned: model::deadline_proposals::DraftInput = serde_json::from_value(unzoned).unwrap();
    let source = crate::application_store::ApplicationStore::uninitialized();
    for (revision, expected) in [
        (None, chrono_tz::America::New_York),
        (Some(3), chrono_tz::America::Chicago),
    ] {
        let resolved = crate::deadline_proposals::resolve_draft(
            pg,
            &source,
            project,
            id,
            revision,
            unzoned.clone(),
        )
        .await
        .unwrap();
        assert_eq!(resolved.timezone, expected);
    }
}

#[tokio::test]
async fn forgejo_evidence_binds_the_verified_repository_and_historical_archived_original() {
    use crate::deadline_proposals as queue;
    use model::deadline_proposals::{Decision, Draft};
    use serde_json::json;
    use uuid::Uuid;

    let (pg, lawyer, _, project, firm) = application_fixture().await;
    sqlx::query("INSERT INTO person_project_role(project_id,person_id,participation) VALUES($1,$2,'lawyer')")
        .bind(project).bind(lawyer).execute(pg.pool()).await.unwrap();
    let authority = Uuid::now_v7();
    let order = Uuid::now_v7();
    let archive = Uuid::now_v7();
    let replacement = Uuid::now_v7();
    let hash = "b".repeat(64);
    for (id, project_id) in [(order, Some(project)), (archive, None), (replacement, None)] {
        sqlx::query("INSERT INTO asset(id,project_id,kind,storage_key,sha256_hex,content_type,byte_size) VALUES($1,$2,'order',$3,$4,'application/pdf',51)")
            .bind(id).bind(project_id).bind(id.to_string()).bind(&hash).execute(pg.pool()).await.unwrap();
    }
    sqlx::query("INSERT INTO authority(id,class,citation,title,archived_asset_id) VALUES($1,'regulation','Synthetic Rule 1','Fixture Rule',$2)").bind(authority).bind(archive).execute(pg.pool()).await.unwrap();
    let id = Uuid::now_v7();
    let source = |role, id| json!({"role":role,"id":id,"sha256_hex":hash,"page":1,"rects":[{"x":0.1,"y":0.1,"width":0.5,"height":0.1}]});
    let mut rule = source("authority", authority);
    rule["forgejo"] =
        json!({"repository":"firm_library","commit":"c".repeat(40),"path":"rules/filed.pdf"});
    let mut draft: Draft = serde_json::from_value(json!({"kind":"signed_order","title":"Synthetic hearing response","due_on":"2028-03-01","timezone":"America/New_York","evidence":[source("signed_order",order),rule]})).unwrap();
    let mut attested = queue::VerifiedForgejo {
        project_id: project,
        firm_id: firm,
        source_id: authority,
        asset_id: archive,
        sha256_hex: hash,
        pin: draft.evidence[1].forgejo.clone().unwrap(),
        origin: "https://forge.example.test".into(),
        owner: "another-firm".into(),
        name: "library".into(),
        repository_id: 41,
    };
    assert!(queue::stage(&pg, project, id, None, &draft).await.is_err());
    assert!(queue::stage_verified(
        &pg,
        project,
        id,
        None,
        &draft,
        std::slice::from_ref(&attested)
    )
    .await
    .is_err());
    attested.owner = sqlx::query_scalar("SELECT slug FROM firm WHERE id=$1")
        .bind(firm)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    queue::stage_verified(
        &pg,
        project,
        id,
        None,
        &draft,
        std::slice::from_ref(&attested),
    )
    .await
    .unwrap();
    let receipt = queue::evidence_receipt(&pg, project, id, 1, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.asset_id, archive);
    assert_eq!(receipt.forgejo.as_ref(), Some(&attested));
    sqlx::query("UPDATE authority SET archived_asset_id=$2 WHERE id=$1")
        .bind(authority)
        .bind(replacement)
        .execute(pg.pool())
        .await
        .unwrap();
    assert!(matches!(
        queue::decide(&pg, project, id, lawyer, 1, Decision::Confirm).await,
        Err(queue::Error::Invalid(_))
    ));
    assert!(
        queue::correct_verified(&pg, project, id, 1, &draft, std::slice::from_ref(&attested))
            .await
            .is_err()
    );
    draft.evidence[1].forgejo = None;
    queue::correct(&pg, project, id, 1, &draft).await.unwrap();
    assert_historical_evidence(&pg, project, id, receipt, replacement).await;
    assert_firm_library_order(&pg, draft, attested, order).await;
}

async fn assert_firm_library_order(
    pg: &PostgresDb,
    mut draft: model::deadline_proposals::Draft,
    mut attested: crate::deadline_proposals::VerifiedForgejo,
    order: uuid::Uuid,
) {
    use crate::deadline_proposals as queue;
    use uuid::Uuid;
    let second = Uuid::now_v7();
    let project = attested.project_id;
    let firm = attested.firm_id;
    draft.evidence[0].forgejo = Some(attested.pin.clone());
    attested.source_id = order;
    attested.asset_id = order;
    attested.firm_id = Uuid::now_v7();
    assert!(queue::stage_verified(
        pg,
        project,
        second,
        None,
        &draft,
        std::slice::from_ref(&attested)
    )
    .await
    .is_err());
    attested.firm_id = firm;
    queue::stage_verified(pg, project, second, None, &draft, &[attested])
        .await
        .unwrap();
    assert_eq!(
        queue::evidence_receipt(pg, project, second, 1, 0)
            .await
            .unwrap()
            .unwrap()
            .asset_id,
        order
    );
}

async fn assert_historical_evidence(
    pg: &PostgresDb,
    project: uuid::Uuid,
    id: uuid::Uuid,
    receipt: crate::deadline_proposals::SourceReceipt,
    replacement: uuid::Uuid,
) {
    use crate::deadline_proposals as queue;
    use uuid::Uuid;
    let archive = receipt.asset_id;
    assert!(sqlx::query("UPDATE deadline_proposal SET source_receipts=NULL WHERE project_id=$1 AND id=$2 AND revision=1")
        .bind(project).bind(id).execute(pg.pool()).await.is_err());
    assert!(queue::get_revision(pg, project, id, 1)
        .await
        .unwrap()
        .unwrap()
        .draft
        .evidence[1]
        .forgejo
        .is_some());
    assert!(queue::get_revision(pg, Uuid::now_v7(), id, 1)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        queue::evidence_receipt(pg, project, id, 1, 1)
            .await
            .unwrap(),
        Some(receipt)
    );
    assert_eq!(
        queue::evidence_receipt(pg, project, id, 2, 1)
            .await
            .unwrap()
            .unwrap()
            .asset_id,
        replacement
    );
    assert!(queue::evidence_receipt(pg, Uuid::now_v7(), id, 1, 1)
        .await
        .unwrap()
        .is_none());
    sqlx::query("UPDATE asset SET deleted_at=now() WHERE id=$1")
        .bind(archive)
        .execute(pg.pool())
        .await
        .unwrap();
    assert!(queue::evidence_receipt(pg, project, id, 1, 1)
        .await
        .unwrap()
        .is_none());
}
