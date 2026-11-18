use super::*;
use crate::delegations::{Basis, NewDelegation, NoticeChannel};
use crate::external_identities::ExternalSystem;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::mem_surreal;

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
    let source = mem_surreal().await;
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
    assert!(!format!("{before:?}").contains("@"));
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
    assert!(!format!("{extra:?}").contains("@"));
}

#[tokio::test]
async fn scoped_mailbox_refuses_malformed_keys_holders_and_casefold_aliases() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let source = mem_surreal().await;
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
    let source = mem_surreal().await;
    let unknown = Uuid::new_v4();
    claim(&source, "unknown-owner@example.com", unknown).await;
    let report = scoped_parity(&source, &pg).await.unwrap();
    let table = mailbox_table(&report);
    assert_eq!((table.source_rows, table.target_rows), (1, 0));
    assert_eq!(table.divergent, [MirrorKey::PersonMailbox(unknown)]);
    assert!(!format!("{report:?}").contains("@"));
    assert!(
        matches!(import(&source,&pg,&MirrorKey::PersonMailbox(unknown)).await,Err(MirrorError::Target(ref code)) if code=="23503")
    );
    let copied: i64 = sqlx::query_scalar("SELECT count(*) FROM person")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(copied, 0);
}

async fn details(source: &SurrealDb, holder: Uuid, subject: Uuid) -> Vec<MirrorKey> {
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
        let licensing = crate::jurisdictions::create(
            source,
            &crate::jurisdictions::NewJurisdiction::new("Captured Licensing State", "SYN", "state"),
        )
        .await
        .unwrap()
        .id;
        let credential = crate::credentials::grant(source, holder, licensing, "SYN-0001")
            .await
            .unwrap()
            .id;
        let token = crate::email_tokens::mint(
            source,
            holder,
            "captured@example.com",
            crate::email_tokens::PURPOSE_EMAIL_CONFIRM,
            "synthetic-plaintext-token",
            Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .unwrap()
        .id;
        let delegate = crate::delegations::grant(
            source,
            &NewDelegation {
                delegate_person_id: holder,
                subject_person_id: subject,
                basis: Basis::Assistant,
                instrument_reference: None,
                granted_by_person_id: holder,
                subject_notified_at: None,
                subject_notified_channel: None,
            },
        )
        .await
        .unwrap()
        .id;
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
    let source = mem_surreal().await;
    let holder = person(&source).await;
    let subject = person(&source).await;
    for id in [holder, subject] {
        import(&source, &pg, &MirrorKey::Person(id)).await.unwrap();
        import(&source, &pg, &MirrorKey::PersonMailbox(id))
            .await
            .unwrap();
    }
    let keys = details(&source, holder, subject).await;
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
    let source = mem_surreal().await;
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
    let delegate = super::super::from_source(crate::delegations::grant(
        &source,
        &NewDelegation {
            delegate_person_id: holder,
            subject_person_id: subject,
            basis: Basis::Assistant,
            instrument_reference: None,
            granted_by_person_id: holder,
            subject_notified_at: Some("yesterday".into()),
            subject_notified_channel: Some(NoticeChannel::InPerson),
        },
    ))
    .await
    .unwrap()
    .id;
    assert!(matches!(
        import(&source, &pg, &MirrorKey::PersonDelegate(delegate)).await,
        Err(MirrorError::SourceTimestamp)
    ));
}

#[tokio::test]
async fn scoped_comparisons_do_not_read_unregistered_catalog_or_graph_rows() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let source = crate::surreal::test_support::unmigrated().await;
    source.query("CREATE jurisdiction:malformed SET name=123; CREATE entity_type:malformed SET name=123; CREATE entity_role:malformed SET inserted_at='not-a-time'")
        .await.unwrap().check().unwrap();
    assert!(identity::scoped_parity(&source, &pg)
        .await
        .unwrap()
        .is_clean());
    assert!(scoped_parity(&source, &pg).await.unwrap().is_clean());
}
