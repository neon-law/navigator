use super::*;
use crate::external_identities::ExternalSystem;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::postgres_mirror::tests::{
    import_predecessor_histories, sealed_prior_family_histories as sealed_old_histories,
    verified_family_capture as verified_capture,
};
use crate::test_support::mem_surreal;

async fn source_delegation(
    source: &SurrealDb,
    holder: Uuid,
    subject: Uuid,
    notified: Option<&str>,
) -> Uuid {
    let id = Uuid::now_v7();
    source
        .query("CREATE $id SET delegate_person_id=$holder, subject_person_id=$subject, basis='assistant', granted_by_person_id=$holder, granted_at='2026-01-01T00:00:00Z', subject_notified_at=$notified, inserted_at='2026-01-01T00:00:00Z', updated_at='2026-01-01T00:00:00Z'")
        .bind(("id", crate::surreal::record_id("person_delegate", id)))
        .bind(("holder", crate::surreal::record_id("person", holder)))
        .bind(("subject", crate::surreal::record_id("person", subject)))
        .bind(("notified", notified.map(str::to_owned)))
        .await.unwrap().check().unwrap();
    id
}

async fn person(source: &SurrealDb) -> Uuid {
    super::super::from_source(crate::persons::create(
        source,
        &crate::persons::NewPerson::new(
            "Captured Person",
            format!("identity-detail-{}@example.com", Uuid::new_v4()),
        ),
    ))
    .await
    .unwrap()
    .id
}

async fn import(
    source: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    super::super::during_initial_copy(super::super::mirror(source, pg, key)).await
}

async fn claim(source: &SurrealDb, mailbox: &str, person: Uuid) {
    source
        .query("CREATE $id SET person_id=$person")
        .bind(("id", RecordId::new("person_mailbox", mailbox.to_owned())))
        .bind(("person", record_id("person", person)))
        .await
        .unwrap()
        .check()
        .unwrap();
}

fn mailbox_table(report: &ParityReport) -> &super::super::TableParity {
    report
        .tables
        .iter()
        .find(|table| table.table == "person_mailbox")
        .unwrap()
}

#[tokio::test]
async fn scoped_mailbox_census_counts_claims_without_reassignment_or_values_in_reports() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let source = super::super::from_source(mem_surreal()).await;
    let first = person(&source).await;
    let second = person(&source).await;
    for id in [first, second] {
        import(&source, &pg, &MirrorKey::Person(id)).await.unwrap();
    }
    claim(&source, "first-alias@example.com", first).await;
    claim(&source, "second-alias@example.com", first).await;
    let before = scoped_parity(&source, &pg).await.unwrap();
    let table = mailbox_table(&before);
    assert_eq!(
        (table.source_rows, table.target_rows, table.divergent.len()),
        (4, 0, 4)
    );
    assert_eq!(
        table
            .divergent
            .iter()
            .filter(|key| **key == MirrorKey::PersonMailbox(first))
            .count(),
        3
    );
    assert!(!format!("{before:?}").contains('@'));
    for id in [first, second] {
        assert_eq!(
            import(&source, &pg, &MirrorKey::PersonMailbox(id))
                .await
                .unwrap(),
            Mirrored::Written
        );
    }
    let clean = scoped_parity(&source, &pg).await.unwrap();
    assert!(clean.is_clean(), "{clean:?}");
    assert_eq!(
        (
            mailbox_table(&clean).source_rows,
            mailbox_table(&clean).target_rows
        ),
        (4, 4)
    );
    source
        .query("UPDATE $id SET person_id=$person")
        .bind((
            "id",
            RecordId::new("person_mailbox", "first-alias@example.com"),
        ))
        .bind(("person", record_id("person", second)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        import(&source, &pg, &MirrorKey::PersonMailbox(second))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    let owner: Uuid = sqlx::query_scalar(
        "SELECT person_id FROM person_mailbox WHERE id='first-alias@example.com'",
    )
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(owner, first);
    let changed = scoped_parity(&source, &pg).await.unwrap();
    assert_eq!(
        mailbox_table(&changed).divergent,
        [MirrorKey::PersonMailbox(second)]
    );
    sqlx::query("INSERT INTO person_mailbox(id,person_id) VALUES('native-only@example.com',$1)")
        .bind(first)
        .execute(pg.pool())
        .await
        .unwrap();
    import(&source, &pg, &MirrorKey::PersonMailbox(first))
        .await
        .unwrap();
    let extra = scoped_parity(&source, &pg).await.unwrap();
    assert_eq!(
        (
            mailbox_table(&extra).source_rows,
            mailbox_table(&extra).target_rows,
            mailbox_table(&extra).divergent.len()
        ),
        (4, 5, 2)
    );
    assert!(!format!("{extra:?}").contains('@'));
}

#[tokio::test]
async fn scoped_mailbox_refuses_malformed_keys_holders_and_casefold_aliases() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let source = super::super::from_source(mem_surreal()).await;
    let holder = person(&source).await;
    for (id, person) in [
        (
            record_id("person_mailbox", Uuid::new_v4()),
            record_id("person", holder),
        ),
        (
            RecordId::new("person_mailbox", "malformed-holder@example.com"),
            RecordId::new("person", "not-a-uuid"),
        ),
    ] {
        source
            .query("CREATE $id SET person_id=$person")
            .bind(("id", id.clone()))
            .bind(("person", person))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(matches!(
            scoped_parity(&source, &pg).await,
            Err(MirrorError::Source)
        ));
        source
            .query("DELETE $id")
            .bind(("id", id))
            .await
            .unwrap()
            .check()
            .unwrap();
    }
    claim(&source, "duplicate@example.com", holder).await;
    claim(&source, " DUPLICATE@EXAMPLE.COM ", holder).await;
    assert!(matches!(
        scoped_parity(&source, &pg).await,
        Err(MirrorError::Source)
    ));
    assert!(matches!(
        import(&source, &pg, &MirrorKey::PersonMailbox(holder)).await,
        Err(MirrorError::Source)
    ));
    let copied: i64 = sqlx::query_scalar("SELECT count(*) FROM person_mailbox")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(copied, 0);
}

#[tokio::test]
async fn scoped_mailbox_unknown_owner_is_counted_and_not_invented() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let source = super::super::from_source(mem_surreal()).await;
    let unknown = Uuid::new_v4();
    claim(&source, "unknown-owner@example.com", unknown).await;
    let report = scoped_parity(&source, &pg).await.unwrap();
    let table = mailbox_table(&report);
    assert_eq!((table.source_rows, table.target_rows), (1, 0));
    assert_eq!(table.divergent, [MirrorKey::PersonMailbox(unknown)]);
    assert!(!format!("{report:?}").contains('@'));
    assert!(
        matches!(import(&source,&pg,&MirrorKey::PersonMailbox(unknown)).await,Err(MirrorError::Target(ref code)) if code=="23503")
    );
    let copied: i64 = sqlx::query_scalar("SELECT count(*) FROM person")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(copied, 0);
}

async fn details(
    source: &SurrealDb,
    holder: Uuid,
    subject: Uuid,
    licensing: Uuid,
) -> Vec<MirrorKey> {
    super::super::from_source(async {
        let address = crate::addresses::create(
            source,
            &crate::addresses::NewAddress {
                person_id: Some(holder),
                line1: "1 Captured Way".into(),
                city: "Synthetic City".into(),
                region: "SC".into(),
                postal_code: "00000".into(),
                country: "US".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id;
        let credential = Uuid::now_v7();
        source.query("CREATE $id SET person_id=$person, jurisdiction_id=$jurisdiction, license_number='SYN-0001'")
            .bind(("id", record_id("credential", credential)))
            .bind(("person", record_id("person", holder)))
            .bind(("jurisdiction", record_id("jurisdiction", licensing)))
            .await.unwrap().check().unwrap();
        let token = Uuid::now_v7();
        source.query("CREATE $id SET person_id=$person, email='captured@example.com', purpose='email_confirm', token_hash=$hash, expires_at=$expires")
            .bind(("id", record_id("email_token", token)))
            .bind(("person", record_id("person", holder)))
            .bind(("hash", crate::git_access_tokens::hash_token("synthetic-plaintext-token")))
            .bind(("expires", surrealdb::types::Datetime::from(Utc::now() + chrono::Duration::hours(1))))
            .await.unwrap().check().unwrap();
        let delegate = source_delegation(source, holder, subject, None).await;
        let external = crate::external_identities::link(
            source,
            holder,
            ExternalSystem::Github,
            "synthetic-1001",
            Some("captured-handle"),
        )
        .await
        .unwrap()
        .id;
        vec![
            MirrorKey::Address(address),
            MirrorKey::Credential(credential),
            MirrorKey::EmailToken(token),
            MirrorKey::PersonDelegate(delegate),
            MirrorKey::ExternalIdentity(external),
        ]
    })
    .await
}

#[tokio::test]
async fn scoped_detail_import_preserves_every_typed_image_and_retains_native_drift() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let source = super::super::from_source(mem_surreal()).await;
    let holder = person(&source).await;
    let subject = person(&source).await;
    for id in [holder, subject] {
        import(&source, &pg, &MirrorKey::Person(id)).await.unwrap();
        import(&source, &pg, &MirrorKey::PersonMailbox(id))
            .await
            .unwrap();
    }
    let licensing = super::super::from_source(crate::jurisdictions::create(
        &source,
        &crate::jurisdictions::NewJurisdiction::new("Captured Licensing State", "SYN", "state"),
    ))
    .await
    .unwrap()
    .id;
    let keys = details(&source, holder, subject, licensing).await;
    for key in &keys {
        assert_eq!(
            import(&source, &pg, key).await.unwrap(),
            Mirrored::Written,
            "{key}"
        );
        assert_eq!(
            import(&source, &pg, key).await.unwrap(),
            Mirrored::Unchanged,
            "{key}"
        );
    }
    let report = scoped_parity(&source, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        report
            .tables
            .iter()
            .map(|table| table.table)
            .collect::<Vec<_>>(),
        [
            "address",
            "credential",
            "email_token",
            "person_delegate",
            "person_external_identity",
            "person_mailbox"
        ]
    );
    for (key, sql) in keys.iter().zip([
        "UPDATE address SET line1='Native street' WHERE id=$1",
        "UPDATE credential SET license_number='NATIVE-0001' WHERE id=$1",
        "UPDATE email_token SET used_at=now() WHERE id=$1",
        "UPDATE person_delegate SET instrument_reference='Native instrument' WHERE id=$1",
        "UPDATE person_external_identity SET handle='native-handle' WHERE id=$1",
    ]) {
        let id = Uuid::parse_str(&key.report_id().unwrap()).unwrap();
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .execute(pg.pool())
            .await
            .unwrap();
        assert_eq!(
            import(&source, &pg, key).await.unwrap(),
            Mirrored::Unchanged,
            "{key}"
        );
    }
    let drift = scoped_parity(&source, &pg).await.unwrap();
    for key in &keys {
        assert!(
            drift.divergent().any(|found| found == key),
            "{key}: {drift:?}"
        );
    }
    let hash: String = sqlx::query_scalar("SELECT token_hash FROM email_token WHERE person_id=$1")
        .bind(holder)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        hash,
        crate::git_access_tokens::hash_token("synthetic-plaintext-token")
    );
}

#[tokio::test]
async fn scoped_details_refuse_orphan_parents_malformed_ids_and_unparseable_times() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let source = super::super::from_source(mem_surreal()).await;
    let missing = Uuid::new_v4();
    let address = Uuid::new_v4();
    source.query("CREATE $id SET person_id=$person,line1='Orphan street',city='Synthetic',region='SC',postal_code='00000',country='US'")
        .bind(("id",record_id("address",address))).bind(("person",record_id("person",missing)))
        .await.unwrap().check().unwrap();
    assert!(
        matches!(import(&source,&pg,&MirrorKey::Address(address)).await,Err(MirrorError::Target(ref code)) if code=="23503")
    );
    assert_eq!(
        scoped_parity(&source, &pg).await.unwrap().tables[0].divergent,
        [MirrorKey::Address(address)]
    );
    source.query("CREATE address:invalid_uuid SET line1='Malformed',city='Synthetic',region='SC',postal_code='00000',country='US'")
        .await.unwrap().check().unwrap();
    assert!(matches!(
        scoped_parity(&source, &pg).await,
        Err(MirrorError::Source)
    ));
    source
        .query("DELETE address:invalid_uuid")
        .await
        .unwrap()
        .check()
        .unwrap();
    let holder = person(&source).await;
    let subject = person(&source).await;
    let delegate = source_delegation(&source, holder, subject, Some("yesterday")).await;
    assert!(matches!(
        import(&source, &pg, &MirrorKey::PersonDelegate(delegate)).await,
        Err(MirrorError::SourceTimestamp)
    ));
}

#[tokio::test]
async fn scoped_comparisons_do_not_read_unregistered_catalog_or_graph_rows() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let source = super::super::from_source(mem_surreal()).await;
    source.query("REMOVE TABLE jurisdiction; REMOVE TABLE entity_type; REMOVE TABLE entity_role; DEFINE TABLE jurisdiction SCHEMALESS; DEFINE TABLE entity_type SCHEMALESS; DEFINE TABLE entity_role SCHEMALESS; CREATE jurisdiction:malformed SET name=123; CREATE entity_type:malformed SET name=123; CREATE entity_role:malformed SET inserted_at='not-a-time'")
        .await.unwrap().check().unwrap();
    assert!(identity::scoped_parity(&source, &pg)
        .await
        .unwrap()
        .is_clean());
    assert!(scoped_parity(&source, &pg).await.unwrap().is_clean());
}

struct DriverCapture {
    source: SurrealDb,
    person: Uuid,
    address: Uuid,
    old_history: serde_json::Value,
}

async fn driver_capture(pg: &PostgresDb) -> DriverCapture {
    let source = crate::surreal::test_support::unmigrated().await;
    super::super::from_source(crate::schema::apply(&source))
        .await
        .unwrap();
    let entity = crate::postgres_mirror::tests::synthetic_entity(&source).await;
    let entity = super::super::from_source(crate::entities::find_by_id(&source, entity))
        .await
        .unwrap()
        .unwrap();
    import_predecessor_histories(&source, pg).await;
    let old_history = sealed_old_histories(pg).await;
    let first = person(&source).await;
    let second = person(&source).await;
    claim(&source, "captured-first-alias@example.com", first).await;
    claim(&source, "captured-second-alias@example.com", first).await;
    let keys = details(&source, first, second, entity.jurisdiction_id).await;
    let MirrorKey::Address(address) = keys[0] else {
        unreachable!()
    };
    source.query("CREATE $id SET person_id=$person,e164='+15555550199',label='mobile',is_primary=true,inserted_at=time::now(),updated_at=time::now()")
        .bind(("id",record_id("person_phone",Uuid::new_v4()))).bind(("person",record_id("person",first)))
        .await.unwrap().check().unwrap();
    DriverCapture {
        source,
        person: first,
        address,
        old_history,
    }
}

#[derive(Clone, Copy)]
enum ExistingIdentity {
    None,
    Drift,
    TargetOnly,
}

async fn advance_identity_driver(
    pg: &PostgresDb,
    captured: &DriverCapture,
    existing: ExistingIdentity,
) -> crate::postgres_backfill::RunReport {
    use crate::postgres_backfill::{self as backfill, family::Family};
    if !matches!(existing, ExistingIdentity::None) {
        let row = super::super::from_source(crate::persons::find_by_id(
            &captured.source,
            captured.person,
        ))
        .await
        .unwrap()
        .unwrap();
        let (id, email) = if matches!(existing, ExistingIdentity::Drift) {
            (captured.person, row.email)
        } else {
            (Uuid::new_v4(), "native-only-driver@example.com".into())
        };
        sqlx::query(
            "INSERT INTO person(id,name,email,role) VALUES($1,'Native disagreement',$2,'admin')",
        )
        .bind(id)
        .bind(email)
        .execute(pg.pool())
        .await
        .unwrap();
    }
    let receipt = verified_capture(&captured.source).await;
    let started = Box::pin(backfill::start_family(
        &captured.source,
        pg,
        Family::Identity,
        receipt,
        1,
        Some(1),
        false,
    ))
    .await
    .unwrap();
    assert_eq!(started.status, backfill::RunStatus::Paused);
    let advanced = Box::pin(backfill::resume_family(
        &captured.source,
        pg,
        Family::Identity,
        started.run_id,
        None,
        false,
    ))
    .await
    .unwrap();
    assert_eq!(advanced.run_id, started.run_id);
    assert_eq!(advanced.status, backfill::RunStatus::Paused);
    let activated = Box::pin(backfill::resume_family(
        &captured.source,
        pg,
        Family::Identity,
        started.run_id,
        None,
        true,
    ))
    .await
    .unwrap();
    assert_eq!(captured.old_history, sealed_old_histories(pg).await);
    activated
}

#[tokio::test]
async fn real_identity_driver_imports_all_nine_then_replays_without_source() {
    use crate::postgres_backfill::{
        self as backfill,
        family::{self, Family},
    };
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let captured = driver_capture(&pg).await;
    let activated = advance_identity_driver(&pg, &captured, ExistingIdentity::None).await;
    assert!(activated.is_clean(), "{activated:?}");
    assert_eq!(activated.parity.len(), 9);
    assert!(activated
        .parity
        .iter()
        .all(|table| table.surreal > 0 && table.surreal == table.postgres));
    let claims = activated
        .parity
        .iter()
        .find(|table| table.table == "person_mailbox")
        .unwrap();
    assert_eq!((claims.surreal, claims.postgres), (4, 4));
    family::require_scope(&pg, Family::Identity).await.unwrap();
    sqlx::query("DELETE FROM address WHERE id=$1")
        .bind(captured.address)
        .execute(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        super::super::mirror(&captured.source, &pg, &MirrorKey::Address(captured.address))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    let present: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM address WHERE id=$1)")
        .bind(captured.address)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert!(!present);
    let offline = crate::surreal::SurrealDb::uninitialized();
    let replay = Box::pin(backfill::resume_family(
        &offline,
        &pg,
        Family::Identity,
        activated.run_id,
        None,
        true,
    ))
    .await
    .unwrap();
    assert_eq!(replay.status, backfill::RunStatus::Completed);
    assert_eq!(replay.run_id, activated.run_id);
    assert_eq!(captured.old_history, sealed_old_histories(&pg).await);
}

async fn refused_identity_driver(existing: ExistingIdentity) {
    use crate::postgres_backfill::{
        self as backfill,
        family::{self, Family},
    };
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let captured = driver_capture(&pg).await;
    let result = advance_identity_driver(&pg, &captured, existing).await;
    assert!(!result.is_clean(), "{result:?}");
    assert_eq!(result.status, backfill::RunStatus::Paused);
    assert!(matches!(
        family::require_scope(&pg, Family::Identity).await,
        Err(family::FamilyError::NotActive)
    ));
    let retained: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM person WHERE name='Native disagreement' AND role='admin'",
    )
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(retained, 1);
    assert_eq!(captured.old_history, sealed_old_histories(&pg).await);
}

#[tokio::test]
async fn real_identity_driver_refuses_native_overlap_drift() {
    refused_identity_driver(ExistingIdentity::Drift).await;
}

#[tokio::test]
async fn real_identity_driver_refuses_native_target_only_rows() {
    refused_identity_driver(ExistingIdentity::TargetOnly).await;
}

#[tokio::test]
async fn entity_role_first_global_copy_preserves_its_explicit_endpoints_and_replay() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    assert!(!super::super::backfilled(&pg).await.unwrap());
    let source = super::super::from_source(mem_surreal()).await;
    let holder = person(&source).await;
    let entity = crate::postgres_mirror::tests::synthetic_entity(&source).await;
    let captured = crate::entity_roles::grant_in_surreal(&source, holder, entity, "manages")
        .await
        .unwrap();
    let key = MirrorKey::EntityRole {
        person_id: holder,
        entity_id: entity,
        role: "manages".into(),
    };
    assert_eq!(import(&source, &pg, &key).await.unwrap(), Mirrored::Written);
    let canonical: (Uuid, Uuid, String, DateTime<Utc>, DateTime<Utc>) =
        sqlx::query_as("SELECT person_id,entity_id,role,inserted_at,updated_at FROM entity_role")
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(
        canonical,
        (
            holder,
            entity,
            "manages".into(),
            super::super::micros(captured.inserted_at),
            super::super::micros(captured.updated_at),
        )
    );
    assert_eq!(
        import(&source, &pg, &key).await.unwrap(),
        Mirrored::Unchanged
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM entity_role")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);
    pg.pool().close().await;
}
