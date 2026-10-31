use super::*;
use crate::delegations::{Basis, NewDelegation, NoticeChannel};
use crate::external_identities::ExternalSystem;
use crate::persons::{NewPerson, PersonEdit, Role};
use crate::postgres_mirror::tests::{connect, fresh_schema, synthetic_entity, test_postgres};
use crate::test_support::mem_surreal;
use std::process::Command;

async fn person(surreal: &SurrealDb, label: &str) -> Uuid {
    crate::persons::create(
        surreal,
        &NewPerson::with_role(
            "Synthetic Person",
            format!("{label}-{}@example.com", Uuid::now_v7()),
            Role::Client,
        ),
    )
    .await
    .unwrap()
    .id
}

async fn jurisdiction(surreal: &SurrealDb) -> Uuid {
    let suffix = Uuid::now_v7().simple().to_string();
    crate::jurisdictions::create(
        surreal,
        &crate::jurisdictions::NewJurisdiction::new(
            format!("Synthetic Licensing State {suffix}"),
            format!("L{}", &suffix[suffix.len() - 8..]),
            "state",
        ),
    )
    .await
    .unwrap()
    .id
}

async fn mirrored(surreal: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) -> Mirrored {
    super::super::mirror(surreal, pg, key).await.unwrap()
}

/// Written, then a replay that changes nothing.
async fn converges(surreal: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) {
    assert_eq!(mirrored(surreal, pg, key).await, Mirrored::Written, "{key}");
    assert_eq!(
        mirrored(surreal, pg, key).await,
        Mirrored::Unchanged,
        "{key}"
    );
}

/// Deleted, then a replay that changes nothing.
async fn removed(surreal: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) {
    assert_eq!(mirrored(surreal, pg, key).await, Mirrored::Deleted, "{key}");
    assert_eq!(
        mirrored(surreal, pg, key).await,
        Mirrored::Unchanged,
        "{key}"
    );
}

async fn surreal_delete(surreal: &SurrealDb, table: &str, id: Uuid) {
    surreal
        .query("DELETE $id")
        .bind(("id", record_id(table, id)))
        .await
        .unwrap()
        .check()
        .unwrap();
}

async fn held(pg: &PostgresDb, sql: &'static str, id: Uuid) -> i64 {
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(&pg.0)
        .await
        .unwrap()
}

#[test]
fn identity_detail_keys_name_rows_without_values() {
    let id = Uuid::nil();
    let nil = "00000000-0000-0000-0000-000000000000";
    for (key, table) in [
        (MirrorKey::Address(id), "address"),
        (MirrorKey::Credential(id), "credential"),
        (MirrorKey::EmailToken(id), "email_token"),
        (MirrorKey::PersonDelegate(id), "person_delegate"),
        (MirrorKey::ExternalIdentity(id), "person_external_identity"),
        (MirrorKey::PersonMailbox(id), "person_mailbox"),
    ] {
        assert_eq!(key.table(), table);
        assert_eq!(key.to_string(), format!("{table}:{nil}"));
    }
    let tie = MirrorKey::EntityRole {
        person_id: id,
        entity_id: id,
        role: "manages".into(),
    };
    assert_eq!(tie.table(), "entity_role");
    assert_eq!(tie.to_string(), format!("entity_role:{nil}/{nil}/manages"));
}

#[test]
fn every_upsert_keys_on_id_and_writes_only_a_differing_row() {
    for (table, columns) in [
        (AddressImage::TABLE, AddressImage::COLUMNS),
        (CredentialImage::TABLE, CredentialImage::COLUMNS),
        (EmailTokenImage::TABLE, EmailTokenImage::COLUMNS),
        (DelegateImage::TABLE, DelegateImage::COLUMNS),
        (ExternalIdentityImage::TABLE, ExternalIdentityImage::COLUMNS),
    ] {
        assert_eq!(columns[0], "id", "{table}");
        let sql = upsert_sql(table, columns);
        assert!(sql.contains("ON CONFLICT (id)"), "{sql}");
        assert!(sql.contains("IS DISTINCT FROM"), "{sql}");
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one lifecycle per identity-detail table reads in order"
)]
async fn each_identity_detail_row_converges_through_its_lifecycle() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let entity = synthetic_entity(&surreal).await;
    let holder = person(&surreal, "holder").await;
    let licensing = jurisdiction(&surreal).await;

    // Nothing above has been mirrored: each dependent brings the Person,
    // Entity, and Jurisdiction it names before its own row.
    let address = crate::addresses::create(
        &surreal,
        &crate::addresses::NewAddress {
            person_id: Some(holder),
            entity_id: Some(entity),
            line1: "1 Synthetic Way".into(),
            city: "Synthetic City".into(),
            region: "SC".into(),
            postal_code: "00000".into(),
            country: "US".into(),
            ..crate::addresses::NewAddress::default()
        },
    )
    .await
    .unwrap()
    .id;
    let address_key = MirrorKey::Address(address);
    converges(&surreal, &pg, &address_key).await;
    let identity = identity::parity(&surreal, &pg).await.unwrap();
    for table in &identity.tables {
        if matches!(table.table, "person" | "entity") {
            assert_eq!(table.divergent.len(), table.source_rows - 1, "{table:?}");
        }
    }
    surreal
        .query("UPDATE $id SET city = 'Synthetic Town'")
        .bind(("id", record_id("address", address)))
        .await
        .unwrap()
        .check()
        .unwrap();
    converges(&surreal, &pg, &address_key).await;
    surreal_delete(&surreal, "address", address).await;
    removed(&surreal, &pg, &address_key).await;

    // Credential: granted, then revoked.
    let credential = crate::credentials::grant(&surreal, holder, licensing, "SYN-0001")
        .await
        .unwrap()
        .id;
    let credential_key = MirrorKey::Credential(credential);
    converges(&surreal, &pg, &credential_key).await;
    crate::credentials::revoke(&surreal, credential)
        .await
        .unwrap();
    removed(&surreal, &pg, &credential_key).await;

    // Email Token: minted, then consumed. Only the hash reaches PostgreSQL.
    let plaintext = "synthetic-token-plaintext";
    let token = crate::email_tokens::mint(
        &surreal,
        holder,
        "holder@example.com",
        crate::email_tokens::PURPOSE_EMAIL_CONFIRM,
        plaintext,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap()
    .id;
    let token_key = MirrorKey::EmailToken(token);
    converges(&surreal, &pg, &token_key).await;
    crate::email_tokens::consume(&surreal, token, Utc::now())
        .await
        .unwrap();
    converges(&surreal, &pg, &token_key).await;
    let (hash, used): (String, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT token_hash, used_at FROM email_token WHERE id = $1")
            .bind(token)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(hash, crate::git_access_tokens::hash_token(plaintext));
    assert!(used.is_some());
    surreal_delete(&surreal, "email_token", token).await;
    removed(&surreal, &pg, &token_key).await;

    // Entity Role: keyed by its tie.
    let tie = crate::entity_roles::grant(&surreal, holder, entity, "manages")
        .await
        .unwrap();
    let tie_key = MirrorKey::EntityRole {
        person_id: tie.person_id,
        entity_id: tie.entity_id,
        role: tie.role.clone(),
    };
    converges(&surreal, &pg, &tie_key).await;
    surreal
        .query("DELETE entity_role WHERE in = $person AND out = $entity")
        .bind(("person", record_id("person", holder)))
        .bind(("entity", record_id("entity", entity)))
        .await
        .unwrap()
        .check()
        .unwrap();
    removed(&surreal, &pg, &tie_key).await;

    // Person Delegate: granted, the subject told, then revoked.
    let subject = person(&surreal, "subject").await;
    let granter = person(&surreal, "granter").await;
    let delegation = crate::delegations::grant(
        &surreal,
        &NewDelegation {
            delegate_person_id: holder,
            subject_person_id: subject,
            basis: Basis::Assistant,
            instrument_reference: None,
            granted_by_person_id: granter,
            subject_notified_at: None,
            subject_notified_channel: None,
        },
    )
    .await
    .unwrap()
    .id;
    let delegation_key = MirrorKey::PersonDelegate(delegation);
    converges(&surreal, &pg, &delegation_key).await;
    crate::delegations::record_subject_notice(&surreal, delegation, NoticeChannel::Telephone)
        .await
        .unwrap();
    converges(&surreal, &pg, &delegation_key).await;
    crate::delegations::revoke(&surreal, delegation, granter)
        .await
        .unwrap();
    converges(&surreal, &pg, &delegation_key).await;
    let channel: Option<String> = sqlx::query_scalar(
        "SELECT subject_notified_channel FROM person_delegate WHERE id = $1 \
         AND revoked_at IS NOT NULL",
    )
    .bind(delegation)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(channel.as_deref(), Some("telephone"));

    // Person External Identity: linked, repointed, unlinked.
    let account = crate::external_identities::link(
        &surreal,
        holder,
        ExternalSystem::Github,
        "1001",
        Some("synthetic-handle"),
    )
    .await
    .unwrap()
    .id;
    let account_key = MirrorKey::ExternalIdentity(account);
    converges(&surreal, &pg, &account_key).await;
    crate::external_identities::set_for_person(
        &surreal,
        holder,
        ExternalSystem::Github,
        Some("1002"),
    )
    .await
    .unwrap();
    converges(&surreal, &pg, &account_key).await;
    crate::external_identities::unlink(&surreal, account)
        .await
        .unwrap();
    removed(&surreal, &pg, &account_key).await;

    // Person Mailbox: the claim follows an email edit.
    let mailbox_key = MirrorKey::PersonMailbox(holder);
    converges(&surreal, &pg, &mailbox_key).await;
    let moved = format!("moved-{}@example.com", Uuid::now_v7());
    crate::persons::edit(
        &surreal,
        holder,
        &PersonEdit {
            email: Some(moved.clone()),
            ..PersonEdit::default()
        },
    )
    .await
    .unwrap();
    converges(&surreal, &pg, &mailbox_key).await;
    let claims: Vec<String> =
        sqlx::query_scalar("SELECT id FROM person_mailbox WHERE person_id = $1")
            .bind(holder)
            .fetch_all(&pg.0)
            .await
            .unwrap();
    assert_eq!(claims, [moved.to_lowercase()]);

    let report = parity(&surreal, &pg).await.unwrap();
    for table in &report.tables {
        if table.table == "person_mailbox" {
            // The in-memory engine seeds People whose claims were never
            // mirrored; this Person's is not among the divergent.
            assert!(!table.divergent.contains(&mailbox_key), "{table:?}");
        } else {
            assert!(table.divergent.is_empty(), "{table:?}");
        }
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one seeded-drift comparison and repair reads in order"
)]
async fn parity_names_divergent_rows_and_reconcile_repairs_them() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let entity = synthetic_entity(&surreal).await;
    let holder = person(&surreal, "drift").await;
    let licensing = jurisdiction(&surreal).await;

    // Mirrored, then changed without its mirror: differing.
    let address = crate::addresses::create(
        &surreal,
        &crate::addresses::NewAddress {
            entity_id: Some(entity),
            line1: "2 Synthetic Way".into(),
            city: "Synthetic City".into(),
            region: "SC".into(),
            postal_code: "00000".into(),
            country: "US".into(),
            ..crate::addresses::NewAddress::default()
        },
    )
    .await
    .unwrap()
    .id;
    mirrored(&surreal, &pg, &MirrorKey::Address(address)).await;
    surreal
        .query("UPDATE $id SET line2 = 'Suite 1'")
        .bind(("id", record_id("address", address)))
        .await
        .unwrap()
        .check()
        .unwrap();
    // Never mirrored: missing.
    let credential = crate::credentials::grant(&surreal, holder, licensing, "SYN-0002")
        .await
        .unwrap()
        .id;
    crate::entity_roles::grant(&surreal, holder, entity, "member")
        .await
        .unwrap();
    let account = crate::external_identities::link(
        &surreal,
        holder,
        ExternalSystem::Linear,
        "synthetic-linear",
        None,
    )
    .await
    .unwrap()
    .id;
    // Held only by PostgreSQL: extra. A stale claim on a mailbox the
    // Person no longer holds, and a stale token.
    mirrored(&surreal, &pg, &MirrorKey::Person(holder)).await;
    sqlx::query("INSERT INTO person_mailbox (id, person_id) VALUES ($1, $2)")
        .bind(format!("stale-{}@example.com", Uuid::now_v7()))
        .bind(holder)
        .execute(&pg.0)
        .await
        .unwrap();
    let stale_token = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO email_token (id, person_id, email, purpose, token_hash, expires_at) \
         VALUES ($1, $2, 'stale@example.com', 'password_reset', $3, now())",
    )
    .bind(stale_token)
    .bind(holder)
    .bind(crate::git_access_tokens::hash_token("stale"))
    .execute(&pg.0)
    .await
    .unwrap();

    let report = parity(&surreal, &pg).await.unwrap();
    let divergent: Vec<MirrorKey> = report.divergent().cloned().collect();
    for key in [
        MirrorKey::Address(address),
        MirrorKey::Credential(credential),
        MirrorKey::EmailToken(stale_token),
        MirrorKey::EntityRole {
            person_id: holder,
            entity_id: entity,
            role: "member".into(),
        },
        MirrorKey::ExternalIdentity(account),
        MirrorKey::PersonMailbox(holder),
    ] {
        assert!(divergent.contains(&key), "{key} in {divergent:?}");
    }
    // Keys and counts only: never an email, a line of an address, or a hash.
    let rendered = format!("{report:?}");
    for value in [
        "example.com",
        "Synthetic Way",
        "SYN-0002",
        "synthetic-linear",
    ] {
        assert!(!rendered.contains(value), "{value} in {rendered}");
    }
    let tokens = &report.tables[2];
    assert_eq!(
        super::super::drift(&pg, tokens).await.unwrap(),
        super::super::Drift {
            extra: 1,
            ..super::super::Drift::default()
        }
    );

    // Reconcile repairs every row and a replay finds nothing to do.
    let repaired = reconcile(&surreal, &pg).await.unwrap();
    assert!(repaired.is_clean(), "{repaired:?}");
    assert_eq!(
        held(
            &pg,
            "SELECT count(*) FROM email_token WHERE id = $1",
            stale_token
        )
        .await,
        0
    );
    let line2: Option<String> = sqlx::query_scalar("SELECT line2 FROM address WHERE id = $1")
        .bind(address)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(line2.as_deref(), Some("Suite 1"));
    assert_eq!(reconcile(&surreal, &pg).await.unwrap(), repaired);
}

#[tokio::test]
async fn a_row_naming_a_person_surreal_lacks_is_refused_and_reported() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let licensing = jurisdiction(&surreal).await;
    // The engine accepts a link to a Person that was never written.
    let credential = Uuid::now_v7();
    surreal
        .query("CREATE $id SET person_id = $person, jurisdiction_id = $j, license_number = 'X'")
        .bind(("id", record_id("credential", credential)))
        .bind(("person", record_id("person", Uuid::now_v7())))
        .bind(("j", record_id("jurisdiction", licensing)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let error = super::super::mirror(&surreal, &pg, &MirrorKey::Credential(credential))
        .await
        .unwrap_err();
    assert!(matches!(error, MirrorError::Target(ref code) if code == "23503"));
    // The Jurisdiction it named was mirrored first, and stays.
    assert_eq!(
        held(
            &pg,
            "SELECT count(*) FROM jurisdiction WHERE id = $1",
            licensing
        )
        .await,
        1
    );
    let report = reconcile(&surreal, &pg).await.unwrap();
    assert_eq!(
        report.tables[1].divergent,
        [MirrorKey::Credential(credential)]
    );
    assert_eq!(report.tables[1].target_rows, 0);
}

#[tokio::test]
async fn an_unparseable_delegation_instant_is_refused_not_substituted() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let delegate = person(&surreal, "delegate").await;
    let subject = person(&surreal, "subject").await;
    let delegation = crate::delegations::grant(
        &surreal,
        &NewDelegation {
            delegate_person_id: delegate,
            subject_person_id: subject,
            basis: Basis::Assistant,
            instrument_reference: None,
            granted_by_person_id: delegate,
            subject_notified_at: Some("yesterday".into()),
            subject_notified_channel: Some(NoticeChannel::InPerson),
        },
    )
    .await
    .unwrap()
    .id;
    let error = super::super::mirror(&surreal, &pg, &MirrorKey::PersonDelegate(delegation))
        .await
        .unwrap_err();
    assert!(matches!(error, MirrorError::SourceTimestamp));
    let report = parity(&surreal, &pg).await.unwrap();
    assert_eq!(
        report.tables[4].divergent,
        [MirrorKey::PersonDelegate(delegation)]
    );
}

/// The write hooks read the process environment and pool, so the probe
/// runs in a child process with the mirror switched on.
#[test]
fn identity_detail_write_hooks_isolated() {
    if test_postgres().is_none() {
        return;
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (_, database) = runtime.block_on(fresh_schema()).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "postgres_mirror::identity_detail::tests::identity_detail_write_hook_probe",
            "--nocapture",
        ])
        .env("NAVIGATOR_POSTGRES_MIRROR_DETAIL_PROBE", "1")
        .env(super::super::MIRROR_ENV, "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env(
            "NAVIGATOR_POSTGRES_PORT",
            std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap(),
        )
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "every identity-detail writer runs once, in order"
)]
async fn identity_detail_write_hook_probe() {
    if std::env::var("NAVIGATOR_POSTGRES_MIRROR_DETAIL_PROBE").is_err() {
        return;
    }
    let surreal = mem_surreal().await;
    let pg = PostgresDb::from_env().await.unwrap();
    // Each store command mirrors its own row; nothing here calls the mirror.
    let entity = synthetic_entity(&surreal).await;
    let holder = person(&surreal, "hooked").await;
    let subject = person(&surreal, "hooked-subject").await;
    let licensing = jurisdiction(&surreal).await;
    crate::addresses::create(
        &surreal,
        &crate::addresses::NewAddress {
            person_id: Some(holder),
            line1: "3 Synthetic Way".into(),
            city: "Synthetic City".into(),
            region: "SC".into(),
            postal_code: "00000".into(),
            country: "US".into(),
            ..crate::addresses::NewAddress::default()
        },
    )
    .await
    .unwrap();
    let revoked = crate::credentials::grant(&surreal, holder, licensing, "SYN-0003")
        .await
        .unwrap()
        .id;
    crate::credentials::revoke(&surreal, revoked).await.unwrap();
    crate::credentials::grant(&surreal, holder, licensing, "SYN-0004")
        .await
        .unwrap();
    let token = crate::email_tokens::mint(
        &surreal,
        holder,
        "hooked@example.com",
        crate::email_tokens::PURPOSE_PASSWORD_RESET,
        "hooked-plaintext",
        Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap()
    .id;
    crate::email_tokens::consume(&surreal, token, Utc::now())
        .await
        .unwrap();
    crate::entity_roles::grant(&surreal, holder, entity, "owns")
        .await
        .unwrap();
    let delegation = crate::delegations::grant(
        &surreal,
        &NewDelegation {
            delegate_person_id: holder,
            subject_person_id: subject,
            basis: Basis::Assistant,
            instrument_reference: None,
            granted_by_person_id: subject,
            subject_notified_at: None,
            subject_notified_channel: None,
        },
    )
    .await
    .unwrap()
    .id;
    crate::delegations::record_subject_notice(&surreal, delegation, NoticeChannel::Email)
        .await
        .unwrap();
    crate::delegations::revoke(&surreal, delegation, subject)
        .await
        .unwrap();
    let unlinked =
        crate::external_identities::link(&surreal, holder, ExternalSystem::Slack, "U1", None)
            .await
            .unwrap()
            .id;
    crate::external_identities::unlink(&surreal, unlinked)
        .await
        .unwrap();
    crate::external_identities::set_for_person(
        &surreal,
        holder,
        ExternalSystem::Notion,
        Some("synthetic-notion"),
    )
    .await
    .unwrap();
    crate::external_identities::set_for_person(
        &surreal,
        holder,
        ExternalSystem::Notion,
        Some("synthetic-notion-2"),
    )
    .await
    .unwrap();
    crate::persons::edit(
        &surreal,
        subject,
        &PersonEdit {
            email: Some(format!("hooked-moved-{}@example.com", Uuid::now_v7())),
            ..PersonEdit::default()
        },
    )
    .await
    .unwrap();
    let removed = person(&surreal, "hooked-removed").await;
    crate::persons::delete(&surreal, removed).await.unwrap();

    let report = parity(&surreal, &pg).await.unwrap();
    for table in &report.tables {
        if table.table == "person_mailbox" {
            // Only the People this probe wrote are mirrored; the in-memory
            // engine's own seeded claims are not.
            for person in [holder, subject, removed] {
                assert!(
                    !table.divergent.contains(&MirrorKey::PersonMailbox(person)),
                    "{table:?}"
                );
            }
        } else if table.table == "entity_role" {
            // Under AGE authority the Surreal graph tables are out of the
            // write path (docs/age-graph-runbook.md): the roles the probe
            // granted exist only in PostgreSQL, so parity reports them as
            // target-only divergence by design, not mirror drift.
            assert_eq!(table.source_rows, 0, "{table:?}");
            assert!(table.target_rows > 0, "{table:?}");
            for key in &table.divergent {
                assert!(matches!(key, MirrorKey::EntityRole { .. }), "{table:?}");
            }
        } else {
            assert!(table.divergent.is_empty(), "{table:?}");
            assert!(table.target_rows > 0, "{table:?}");
        }
    }
    assert_eq!(
        held(
            &pg,
            "SELECT count(*) FROM person_mailbox WHERE person_id = $1",
            subject
        )
        .await,
        1
    );
    assert_eq!(
        held(&pg, "SELECT count(*) FROM person WHERE id = $1", removed).await,
        0
    );
}
