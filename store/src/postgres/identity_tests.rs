use super::*;
use crate::delegations::{Basis, NewDelegation, NoticeChannel};
use crate::external_identities::{ExternalIdentityError, ExternalSystem};
use crate::person_phones::PhoneLabel;
use crate::persons::{NewPerson, PersonError};
use crate::postgres_mirror::{MirrorKey, Mirrored};

const CASE: &str = "NAVIGATOR_POSTGRES_IDENTITY_TEST_CASE";
const NAME: &str =
    "postgres::identity::tests::people_authority_owns_identity_detail_aliases_and_merges";

/// Run this test again in a child process with People authority set against
/// its own migrated database, so the switch never leaks into other tests.
/// Returns `true` in the parent, which has nothing more to do.
async fn in_isolated_child() -> bool {
    if std::env::var(CASE).as_deref() == Ok("child") {
        return false;
    }
    let Some((admin_options, admin_database)) = crate::postgres_mirror::tests::test_postgres()
    else {
        return true;
    };
    let port = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").expect("fixture port");
    let admin = sqlx::PgPool::connect_with(admin_options.clone().database(&admin_database))
        .await
        .expect("connect to the synthetic admin database");
    let database = format!("identity_test_{}", Uuid::now_v7().simple());
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .expect("create the isolated database");
    let fixture = sqlx::PgPool::connect_with(admin_options.database(&database))
        .await
        .expect("connect to the isolated database");
    // Production creates AGE in the privileged init path before migrations.
    sqlx::query("CREATE EXTENSION IF NOT EXISTS age")
        .execute(&fixture)
        .await
        .expect("create AGE");
    fixture.close().await;
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", NAME, "--nocapture"])
        .env(CASE, "child")
        .env("NAVIGATOR_POSTGRES_PEOPLE_AUTHORITY", "1")
        .env("NAVIGATOR_TEST_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .output();
    sqlx::query(AssertSqlSafe(format!(
        "DROP DATABASE {database} WITH (FORCE)"
    )))
    .execute(&admin)
    .await
    .expect("drop the isolated database");
    admin.close().await;
    let output = output.expect("launch the isolated test");
    assert!(
        output.status.success(),
        "isolated identity authority test failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// One value SurrealDB's projection of a row holds.
async fn projected<T: surrealdb::types::SurrealValue>(
    surreal: &SurrealDb,
    field: &str,
    record: RecordId,
) -> Option<T> {
    surreal
        .query(format!("SELECT VALUE {field} FROM ONLY $id"))
        .bind(("id", record))
        .await
        .unwrap()
        .check()
        .unwrap()
        .take(0)
        .unwrap()
}

async fn client(surreal: &SurrealDb, label: &str) -> crate::persons::Person {
    crate::persons::create(
        surreal,
        &NewPerson::new(
            "Synthetic Client",
            format!("{label}-{}@example.test", Uuid::now_v7()),
        ),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one authority scenario writes every identity table, then merges across them"
)]
async fn people_authority_owns_identity_detail_aliases_and_merges() {
    if in_isolated_child().await {
        return;
    }
    let pg = PostgresDb::from_env().await.expect("connect to PostgreSQL");
    pg.migrate_embedded(None)
        .await
        .expect("apply the canonical schema");
    let surreal = crate::test_support::mem_surreal().await;
    let keep = client(&surreal, "keep").await;
    let dropped = client(&surreal, "drop").await;
    let subject = client(&surreal, "subject").await;

    // Aliases are PostgreSQL mailbox claims, projected beside the primary.
    let alias = format!("alias-{}@example.test", Uuid::now_v7());
    crate::persons::add_alias(&surreal, dropped.id, &alias.to_uppercase())
        .await
        .unwrap();
    crate::persons::add_alias(&surreal, dropped.id, &alias)
        .await
        .unwrap();
    for held in [alias.as_str(), dropped.email.as_str()] {
        assert!(matches!(
            crate::persons::add_alias(&surreal, keep.id, held).await,
            Err(PersonError::EmailTaken)
        ));
    }
    assert_eq!(
        crate::persons::aliases(&surreal, dropped.id).await.unwrap(),
        vec![alias.clone()]
    );
    assert_eq!(
        crate::persons::find_by_any_email(&surreal, &alias)
            .await
            .unwrap()
            .map(|person| person.id),
        Some(dropped.id)
    );
    let claim = RecordId::new("person_mailbox", alias.as_str());
    assert_eq!(
        projected::<RecordId>(&surreal, "person_id", claim.clone()).await,
        Some(record_id("person", dropped.id))
    );

    // The catalog stays mirrored from SurrealDB.
    let suffix = Uuid::now_v7().simple().to_string();
    let jurisdiction = crate::jurisdictions::create(
        &surreal,
        &crate::jurisdictions::NewJurisdiction::new(
            format!("Synthetic State {suffix}"),
            format!("S{}", &suffix[suffix.len() - 8..]),
            "state",
        ),
    )
    .await
    .unwrap()
    .id;
    let entity_type = crate::entity_types::create(&surreal, &format!("Synthetic Type {suffix}"))
        .await
        .unwrap()
        .id;
    for key in [
        MirrorKey::Jurisdiction(jurisdiction),
        MirrorKey::EntityType(entity_type),
    ] {
        crate::postgres_mirror::mirror(&surreal, &pg, &key)
            .await
            .unwrap();
    }

    // Entity: the unique index is the Firm anchor claim, and the projection
    // carries the claim SurrealDB readers resolve.
    let new_entity = |name: &str, anchor: Option<&str>| crate::entities::NewEntity {
        name: name.to_string(),
        entity_type_id: entity_type,
        jurisdiction_id: jurisdiction,
        phone: None,
        url: None,
        xero_id: None,
        firm_anchor_key: anchor.map(str::to_string),
    };
    let anchor = crate::entities::create(
        &surreal,
        &new_entity("Synthetic Firm", Some("synthetic-anchor")),
    )
    .await
    .unwrap();
    assert!(matches!(
        crate::entities::create(
            &surreal,
            &new_entity("Second Firm", Some("synthetic-anchor"))
        )
        .await,
        Err(crate::entities::EntityError::FirmAnchorTaken)
    ));
    assert_eq!(
        crate::entities::firm_anchor_holder(&surreal, "synthetic-anchor")
            .await
            .unwrap(),
        Some(anchor.id)
    );
    assert_eq!(
        projected::<RecordId>(
            &surreal,
            "entity_id",
            RecordId::new("firm_anchor", "synthetic-anchor")
        )
        .await,
        Some(record_id("entity", anchor.id))
    );
    assert_eq!(
        crate::entities::update(
            &surreal,
            anchor.id,
            &new_entity("Renamed Firm", Some("synthetic-anchor"))
        )
        .await
        .unwrap(),
        None,
        "a Firm anchor keeps its name"
    );
    crate::entities::seed_ein(&surreal, anchor.id, "00-0000000")
        .await
        .unwrap();
    let anchor = crate::entities::set_firm_anchor_key(&surreal, anchor.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        anchor.metadata,
        Some(serde_json::json!({"ein": "00-0000000"}))
    );
    assert!(
        !crate::entities::firm_anchor_exists(&surreal, "synthetic-anchor")
            .await
            .unwrap()
    );
    assert_eq!(
        projected::<RecordId>(
            &surreal,
            "entity_id",
            RecordId::new("firm_anchor", "synthetic-anchor")
        )
        .await,
        None
    );
    assert_eq!(
        projected::<String>(&surreal, "name", record_id("entity", anchor.id)).await,
        Some("Synthetic Firm".to_string())
    );
    assert_eq!(
        crate::entities::find_by_identity(&surreal, "Synthetic Firm", entity_type, jurisdiction)
            .await
            .unwrap()
            .map(|entity| entity.id),
        Some(anchor.id)
    );

    // Address.
    let address = crate::addresses::create(
        &surreal,
        &crate::addresses::NewAddress {
            person_id: Some(dropped.id),
            entity_id: Some(anchor.id),
            line1: "1 Synthetic Way".into(),
            city: "Example City".into(),
            region: "NV".into(),
            postal_code: "89000".into(),
            country: "US".into(),
            ..crate::addresses::NewAddress::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        crate::addresses::for_entity(&surreal, anchor.id)
            .await
            .unwrap(),
        vec![address.clone()]
    );
    assert_eq!(crate::addresses::count(&surreal).await.unwrap(), 1);
    assert_eq!(
        projected::<String>(&surreal, "line1", record_id("address", address.id)).await,
        Some("1 Synthetic Way".to_string())
    );

    // Credential: the kept Person holds the same licence, so the merge keeps
    // one row.
    crate::credentials::grant(&surreal, dropped.id, jurisdiction, "LIC-1")
        .await
        .unwrap();
    assert!(matches!(
        crate::credentials::grant(&surreal, dropped.id, jurisdiction, "LIC-2").await,
        Err(crate::credentials::CredentialError::AlreadyGranted)
    ));
    crate::credentials::find_or_grant(&surreal, keep.id, jurisdiction, "LIC-3")
        .await
        .unwrap();
    assert!(
        crate::credentials::is_licensed_in(&surreal, dropped.id, jurisdiction)
            .await
            .unwrap()
    );

    // Email token: single use and expiry are PostgreSQL predicates.
    let now = chrono::Utc::now();
    let token = crate::email_tokens::mint(
        &surreal,
        dropped.id,
        &dropped.email,
        crate::email_tokens::PURPOSE_EMAIL_CONFIRM,
        "synthetic-plaintext",
        now + chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    assert!(crate::email_tokens::has_live_token_since(
        &surreal,
        dropped.id,
        crate::email_tokens::PURPOSE_EMAIL_CONFIRM,
        now - chrono::Duration::minutes(1),
        now,
    )
    .await
    .unwrap());
    let found = crate::email_tokens::validate(
        &surreal,
        "synthetic-plaintext",
        crate::email_tokens::PURPOSE_EMAIL_CONFIRM,
        now,
    )
    .await
    .unwrap();
    assert_eq!(found.map(|row| row.id), Some(token.id));
    crate::email_tokens::consume(&surreal, token.id, now)
        .await
        .unwrap();
    assert!(crate::email_tokens::validate(
        &surreal,
        "synthetic-plaintext",
        crate::email_tokens::PURPOSE_EMAIL_CONFIRM,
        now,
    )
    .await
    .unwrap()
    .is_none());

    // External identities: both Persons hold GitHub, so the dropped one's
    // goes in the merge; Slack moves.
    crate::external_identities::link(&surreal, keep.id, ExternalSystem::Github, "gh-keep", None)
        .await
        .unwrap();
    crate::external_identities::link(
        &surreal,
        dropped.id,
        ExternalSystem::Github,
        "gh-drop",
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        crate::external_identities::link(
            &surreal,
            subject.id,
            ExternalSystem::Github,
            "gh-drop",
            None
        )
        .await,
        Err(ExternalIdentityError::AccountTaken { .. })
    ));
    crate::external_identities::set_for_person(
        &surreal,
        dropped.id,
        ExternalSystem::Slack,
        Some("U-OLD"),
    )
    .await
    .unwrap();
    let slack = crate::external_identities::set_for_person(
        &surreal,
        dropped.id,
        ExternalSystem::Slack,
        Some("U-NEW"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        projected::<String>(
            &surreal,
            "external_id",
            record_id("person_external_identity", slack.id)
        )
        .await,
        Some("U-NEW".to_string())
    );

    // Phones: the first is primary; a new primary demotes it in both stores.
    let first = crate::person_phones::set_phone(
        &surreal,
        dropped.id,
        "(510) 555-0100",
        PhoneLabel::Mobile,
        false,
    )
    .await
    .unwrap();
    assert!(first.is_primary);
    let second = crate::person_phones::set_phone(
        &surreal,
        dropped.id,
        "+1 510 555 0101",
        PhoneLabel::Work,
        true,
    )
    .await
    .unwrap();
    let listed = crate::person_phones::list_phones(&surreal, dropped.id)
        .await
        .unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|phone| (phone.id, phone.is_primary))
            .collect::<Vec<_>>(),
        vec![(second.id, true), (first.id, false)]
    );
    assert_eq!(
        projected::<bool>(&surreal, "is_primary", record_id("person_phone", first.id)).await,
        Some(false)
    );
    assert_eq!(
        crate::person_phones::remove_phone(&surreal, dropped.id, "510-555-0101")
            .await
            .unwrap()
            .as_deref(),
        Some("+15105550101")
    );
    assert_eq!(
        projected::<String>(&surreal, "e164", record_id("person_phone", second.id)).await,
        None
    );

    // Delegation: instants are timestamps here and RFC 3339 text in the
    // projection.
    let delegation = crate::delegations::grant(
        &surreal,
        &NewDelegation {
            delegate_person_id: dropped.id,
            subject_person_id: subject.id,
            basis: Basis::Assistant,
            instrument_reference: None,
            granted_by_person_id: keep.id,
            subject_notified_at: None,
            subject_notified_channel: None,
        },
    )
    .await
    .unwrap();
    crate::delegations::record_subject_notice(&surreal, delegation.id, NoticeChannel::Email)
        .await
        .unwrap();
    assert_eq!(
        crate::delegations::live_for_subject(&surreal, subject.id)
            .await
            .unwrap()
            .len(),
        1
    );
    let granted_at: Option<String> = projected(
        &surreal,
        "granted_at",
        record_id("person_delegate", delegation.id),
    )
    .await;
    assert!(granted_at.is_some_and(|at| chrono::DateTime::parse_from_rfc3339(&at).is_ok()));

    // The legacy mirror never copies a projection back over PostgreSQL.
    for key in [
        MirrorKey::Entity(anchor.id),
        MirrorKey::Address(address.id),
        MirrorKey::PersonMailbox(dropped.id),
        MirrorKey::Record {
            table: "person_phone",
            id: first.id,
        },
    ] {
        assert_eq!(
            crate::postgres_mirror::mirror(&surreal, &pg, &key)
                .await
                .unwrap(),
            Mirrored::Unchanged,
            "{key}"
        );
    }

    // The merge moves every PostgreSQL link and keeps every address.
    let report = crate::person_merge::merge(&surreal, keep.id, dropped.id)
        .await
        .unwrap();
    let mut expected = vec![
        keep.email.to_lowercase(),
        dropped.email.to_lowercase(),
        alias.clone(),
    ];
    expected.sort();
    assert_eq!(report.emails, expected);
    assert!(crate::persons::find_by_id(&surreal, dropped.id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        crate::persons::find_by_any_email(&surreal, &dropped.email)
            .await
            .unwrap()
            .map(|person| person.id),
        Some(keep.id)
    );
    assert_eq!(
        projected::<RecordId>(&surreal, "person_id", claim).await,
        Some(record_id("person", keep.id))
    );
    assert_eq!(
        projected::<String>(&surreal, "email", record_id("person", dropped.id)).await,
        None
    );
    let held = |sql: &'static str| {
        let pg = pg.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .bind(keep.id)
                .fetch_one(&pg.0)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        held("SELECT count(*) FROM credential WHERE person_id = $1").await,
        1
    );
    assert_eq!(
        held("SELECT count(*) FROM address WHERE person_id = $1").await,
        1
    );
    assert_eq!(
        held("SELECT count(*) FROM email_token WHERE person_id = $1").await,
        1
    );
    assert_eq!(
        held("SELECT count(*) FROM person_phone WHERE person_id = $1").await,
        1
    );
    assert_eq!(
        held("SELECT count(*) FROM person_delegate WHERE delegate_person_id = $1").await,
        1
    );
    let systems: Vec<String> = sqlx::query_scalar(
        "SELECT system || ':' || external_id FROM person_external_identity \
         WHERE person_id = $1 ORDER BY system",
    )
    .bind(keep.id)
    .fetch_all(&pg.0)
    .await
    .unwrap();
    assert_eq!(systems, vec!["github:gh-keep", "slack:U-NEW"]);
    pg.0.close().await;
}
