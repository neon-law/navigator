use super::*;
use crate::posts::{import_source, SourceCaptions, SourcePost};
use crate::test_support;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use std::process::Command;
use surrealdb::types::SurrealValue;

const BRAND: &str = "synthetic-mirror";

fn quick_completion() -> CompletionPolicy {
    CompletionPolicy {
        attempts: 3,
        attempt_timeout: Duration::from_millis(10),
        timeout: Duration::from_secs(1),
        retry_delay: Duration::ZERO,
    }
}

#[tokio::test]
async fn completion_retries_transient_failures() {
    let mut attempts = 0;
    let result = complete(quick_completion(), || {
        attempts += 1;
        std::future::ready(if attempts < 3 {
            Err(MirrorError::Unavailable)
        } else {
            Ok(Mirrored::Written)
        })
    })
    .await;
    assert!(matches!(result, Ok(Mirrored::Written)));
    assert_eq!(attempts, 3);
}

#[tokio::test]
async fn completion_does_not_retry_invalid_source_or_constraints() {
    for error in [
        MirrorError::SourceTimestamp,
        MirrorError::UnmappedSourceFields {
            table: "contract_review",
            count: 1,
        },
        MirrorError::Target("23503".into()),
    ] {
        let mut error = Some(error);
        let mut attempts = 0;
        let result = complete(quick_completion(), || {
            attempts += 1;
            std::future::ready(Err(error.take().expect("only one attempt")))
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts, 1);
    }
}

#[tokio::test]
async fn completion_retries_timeout_without_replaying_the_source_write() {
    let mut attempts = 0;
    let result = complete(quick_completion(), || {
        attempts += 1;
        let attempt = attempts;
        async move {
            if attempt == 1 {
                std::future::pending::<()>().await;
            }
            Ok(Mirrored::Unchanged)
        }
    })
    .await;
    assert!(matches!(result, Ok(Mirrored::Unchanged)));
    assert_eq!(attempts, 2);
}

#[tokio::test]
async fn completion_bounds_permanent_outage_and_total_time() {
    let mut attempts = 0;
    let result = complete(quick_completion(), || {
        attempts += 1;
        std::future::ready(Err(MirrorError::Unavailable))
    })
    .await;
    assert!(matches!(result, Err(MirrorError::Unavailable)));
    assert_eq!(attempts, 3);

    let policy = CompletionPolicy {
        timeout: Duration::from_millis(5),
        retry_delay: Duration::from_secs(1),
        ..quick_completion()
    };
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        complete(policy, || std::future::ready(Err(MirrorError::Unavailable))),
    )
    .await
    .expect("total budget also bounds retry sleep");
    assert!(matches!(result, Err(MirrorError::Timeout)));
}

#[test]
fn social_account_selection_is_a_measured_mirror_with_a_private_stable_key() {
    let brand_key = "synthetic-brand";
    let provider = "facebook";
    let key = MirrorKey::SocialAccountSelection {
        brand_key: brand_key.into(),
        provider: provider.into(),
    };
    assert_eq!(key.table(), "social_account_selection");
    assert_eq!(
        key.to_string(),
        "social_account_selection:synthetic-brand:facebook"
    );

    let mirrors = MIRRORS.iter().map(|mirror| mirror.name).collect::<Vec<_>>();
    assert!(mirrors.contains(&"social_account_selections"));
}

#[derive(SurrealValue)]
struct SocialAccountBundle {
    ciphertext: String,
    wrapped_dek: String,
    kms_key_version: String,
    kms_context: String,
}

#[derive(SurrealValue)]
struct SocialAccountTokens {
    ciphertext: String,
    wrapped_dek: String,
}

pub(super) fn source<'a>(slug: &'a str, title: &'a str) -> SourcePost<'a> {
    SourcePost {
        brand_key: BRAND,
        slug,
        title,
        description: "Synthetic description",
        body_html: "<p>Synthetic</p>",
        body_markdown: Some("Synthetic"),
        status: "published",
        publish_at: "2026-01-02T17:00:00Z".parse().unwrap(),
        video_asset: None,
        vertical_video: None,
        tiktok_ai_generated: None,
        captions: SourceCaptions::default(),
        accounts: &[],
        social_links: &[],
    }
}

pub(crate) fn test_postgres() -> Option<(PgConnectOptions, String)> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
            Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    Some((
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .ssl_mode(PgSslMode::Disable),
        std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
    ))
}

/// A fresh database holding the canonical schema and one synthetic Brand.
async fn fresh_database() -> Option<(PgConnectOptions, String)> {
    let (options, database) = fresh_schema().await?;
    let setup = sqlx::PgPool::connect_with(options.clone().database(&database))
        .await
        .unwrap();
    insert_brand(&setup, BRAND).await;
    setup.close().await;
    Some((options, database))
}

/// A fresh database holding only the canonical schema.
pub(crate) async fn fresh_schema() -> Option<(PgConnectOptions, String)> {
    let (options, admin_database) = test_postgres()?;
    let admin = sqlx::PgPool::connect_with(options.clone().database(&admin_database))
        .await
        .unwrap();
    let database = format!("mirror_test_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let setup = sqlx::PgPool::connect_with(options.clone().database(&database))
        .await
        .unwrap();
    // The graph migration needs the AGE extension, which only a superuser
    // creates: in production the superuser init path (`roles.sql`) runs
    // before any migration, and these fixtures stand in for it. `LOAD` puts
    // the library in this session, the way `session_preload_libraries` does
    // for the application's roles.
    sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS age; LOAD 'age';")
        .execute(&setup)
        .await
        .unwrap();
    sqlx::raw_sql(crate::postgres_schema::canonical_schema())
        .execute(&setup)
        .await
        .unwrap();
    setup.close().await;
    // Production role settings (`roles.sql`) preload AGE into every
    // application session; test sessions connect as the superuser, so the
    // equivalent rides on the connect options.
    let options = options.options([("session_preload_libraries", "age")]);
    Some((options, database))
}

async fn insert_brand(pool: &sqlx::PgPool, brand_key: &str) {
    sqlx::query("INSERT INTO brand (name, brand_key) VALUES ($1, $2)")
        .bind(format!("Synthetic Brand {brand_key}"))
        .bind(brand_key)
        .execute(pool)
        .await
        .unwrap();
}

/// Insert placeholder Person and Entity rows into PostgreSQL under their
/// Surreal ids, with the synthetic reference rows `seed_entity` names. Those
/// references exist in no SurrealDB, so the identity mirror cannot supply an
/// Entity that names them; a tenancy test that uses `seed_entity` supplies
/// the rows here instead. A row already held is left alone.
pub(super) async fn mirror_identity(pg: &PostgresDb, entities: &[Uuid], people: &[Uuid]) {
    sqlx::query(
        "INSERT INTO entity_type (id, name) VALUES ($1, 'Synthetic Entity Type') \
         ON CONFLICT DO NOTHING",
    )
    .bind(test_support::SEED_ENTITY_TYPE_ID)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jurisdiction (id, name) VALUES ($1, 'Synthetic Jurisdiction') \
         ON CONFLICT DO NOTHING",
    )
    .bind(test_support::SEED_ENTITY_JURISDICTION_ID)
    .execute(&pg.0)
    .await
    .unwrap();
    for entity in entities {
        sqlx::query(
            "INSERT INTO entity (id, name, entity_type_id, jurisdiction_id) \
             VALUES ($1, 'Synthetic Entity', $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(entity)
        .bind(test_support::SEED_ENTITY_TYPE_ID)
        .bind(test_support::SEED_ENTITY_JURISDICTION_ID)
        .execute(&pg.0)
        .await
        .unwrap();
    }
    for person in people {
        sqlx::query(
            "INSERT INTO person (id, name, email) VALUES ($1, 'Synthetic Person', $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(person)
        .bind(format!("{person}@example.com"))
        .execute(&pg.0)
        .await
        .unwrap();
    }
}

pub(crate) async fn connect(options: &PgConnectOptions, database: &str) -> PostgresDb {
    PostgresDb::connect(options.clone().database(database))
        .await
        .unwrap()
}

#[test]
fn the_mirror_needs_both_the_opt_in_and_a_host() {
    assert!(enabled_by(Some("1"), Some("127.0.0.1")));
    assert!(!enabled_by(None, Some("127.0.0.1")));
    assert!(!enabled_by(Some("true"), Some("127.0.0.1")));
    assert!(!enabled_by(Some("1"), None));
    assert!(!enabled_by(Some("1"), Some(" ")));
}

#[test]
fn errors_and_keys_name_rows_without_values() {
    let post = Uuid::nil();
    assert_eq!(
        MirrorKey::Post(post).to_string(),
        "post:00000000-0000-0000-0000-000000000000"
    );
    assert_eq!(
        MirrorKey::SocialHandle {
            brand_key: "neon".into(),
            provider: "x".into()
        }
        .table(),
        "social_handle"
    );
    assert_eq!(
        MirrorError::Target("23503".into()).to_string(),
        "PostgreSQL refused the row (SQLSTATE 23503)"
    );
    assert_eq!(MirrorKey::Template(post).table(), "template");
    assert_eq!(
        MirrorKey::Template(post).to_string(),
        "template:00000000-0000-0000-0000-000000000000"
    );
    let social = MirrorKey::SocialAccount("social_account:synthetic-brand_facebook_hash".into());
    assert_eq!(social.table(), "social_account");
    let display = social.to_string();
    assert_eq!(display, "social_account:synthetic-brand_facebook_hash");
    assert!(!display.contains("synthetic-access-token"));
    assert!(!display.contains("synthetic-refresh-token"));
}

/// The hook reads the process environment and the process-wide pool, so each
/// scenario runs in its own child process with its own environment.
#[test]
fn the_write_hook_isolated() {
    let mut cases = vec!["disabled", "unreachable"];
    let database = test_postgres().map(|_| {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(fresh_database()).unwrap()
    });
    if database.is_some() {
        cases.extend(["off", "mirrored"]);
    }
    for case in cases {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "postgres_mirror::tests::write_hook_probe",
                "--nocapture",
            ])
            .env("NAVIGATOR_POSTGRES_MIRROR_TEST_CASE", case)
            .env(MIRROR_ENV, "1")
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env("NAVIGATOR_POSTGRES_PORT", "1")
            .env("NAVIGATOR_POSTGRES_USER", "synthetic-writer")
            .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
            .env("NAVIGATOR_POSTGRES_DATABASE", "synthetic")
            .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
            .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT");
        match case {
            "disabled" => {
                child.env_remove("NAVIGATOR_POSTGRES_HOST");
            }
            "off" | "mirrored" => {
                if case == "off" {
                    child.env_remove(MIRROR_ENV);
                }
                let (_, name) = database.as_ref().unwrap();
                child
                    .env(
                        "NAVIGATOR_POSTGRES_PORT",
                        std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap(),
                    )
                    .env("NAVIGATOR_POSTGRES_USER", "postgres")
                    .env("NAVIGATOR_POSTGRES_DATABASE", name);
            }
            _ => {}
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "write hook scenario {case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// A synthetic Entity whose Entity Type and Jurisdiction exist in
/// SurrealDB, so the identity mirror can supply all three.
pub(super) async fn synthetic_entity(surreal: &SurrealDb) -> Uuid {
    let suffix = Uuid::now_v7().simple().to_string();
    let jurisdiction = crate::jurisdictions::create(
        surreal,
        &crate::jurisdictions::NewJurisdiction::new(
            format!("Synthetic State {suffix}"),
            format!("S{}", &suffix[suffix.len() - 8..]),
            "state",
        ),
    )
    .await
    .unwrap()
    .id;
    let entity_type = crate::entity_types::create(surreal, &format!("Synthetic Type {suffix}"))
        .await
        .unwrap()
        .id;
    crate::entities::create(
        surreal,
        &crate::entities::NewEntity {
            name: format!("Synthetic Entity {suffix}"),
            entity_type_id: entity_type,
            jurisdiction_id: jurisdiction,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap()
    .id
}

/// A Firm with its Admin DRI and Entity, written through the store's own
/// commands so every write hook runs. Returns the Admin and the Entity.
async fn hooked_practice(surreal: &SurrealDb) -> (Uuid, Uuid, Uuid) {
    let entity = synthetic_entity(surreal).await;
    let admin = crate::persons::create(
        surreal,
        &crate::persons::NewPerson::with_role(
            "Synthetic Admin",
            "hooked-admin@example.com",
            crate::persons::Role::Admin,
        ),
    )
    .await
    .unwrap()
    .id;
    let firm = crate::firms::create(
        surreal,
        &crate::firms::NewFirm {
            name: "Synthetic Hooked Practice".into(),
            status: "active".into(),
            entity_id: entity,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap();
    (admin, entity, firm.id)
}

pub(super) async fn hooked_email(surreal: &SurrealDb) -> Uuid {
    let receipt = crate::email_receipts::ensure(
        surreal,
        &crate::email_receipts::NewEmailReceipt {
            receiving_mailbox: "synthetic-hook@example.test",
            deployment: "test",
            firm_id: None,
            provider_account: None,
            sender_authenticated: false,
            raw_digest: "hook-digest",
            source_message_id: Some("hook-message"),
            archive_key: "synthetic/hook.eml",
            letter_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    let receipt_id = receipt.receipt.id;
    let deduped = crate::email_receipts::ensure(
        surreal,
        &crate::email_receipts::NewEmailReceipt {
            receiving_mailbox: "synthetic-hook@example.test",
            deployment: "test",
            firm_id: None,
            provider_account: None,
            sender_authenticated: false,
            raw_digest: "hook-digest",
            source_message_id: Some("hook-message"),
            archive_key: "synthetic/hook.eml",
            letter_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    assert!(deduped.deduped);
    crate::email_deliveries::ensure(surreal, receipt_id, "synthetic-channel")
        .await
        .unwrap();
    crate::email_deliveries::admit_attempt(surreal, receipt_id, "corr-unknown")
        .await
        .unwrap();
    crate::email_deliveries::mark_unknown(surreal, receipt_id, "synthetic failure")
        .await
        .unwrap();
    crate::email_deliveries::authorize_resend(surreal, receipt_id, "synthetic-operator")
        .await
        .unwrap();
    crate::email_deliveries::admit_attempt(surreal, receipt_id, "corr-failed")
        .await
        .unwrap();
    crate::email_deliveries::mark_failed(surreal, receipt_id, "synthetic failure")
        .await
        .unwrap();
    crate::email_deliveries::authorize_resend(surreal, receipt_id, "synthetic-operator")
        .await
        .unwrap();
    crate::email_deliveries::admit_attempt(surreal, receipt_id, "corr-confirmed")
        .await
        .unwrap();
    crate::email_deliveries::mark_confirmed(
        surreal,
        receipt_id,
        "synthetic-channel",
        "1700000000.000001",
    )
    .await
    .unwrap();
    receipt_id
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "each hooked writer's scenario reads in order"
)]
async fn write_hook_probe() {
    let Ok(case) = std::env::var("NAVIGATOR_POSTGRES_MIRROR_TEST_CASE") else {
        return;
    };
    let surreal = test_support::mem_surreal().await;
    let started = std::time::Instant::now();
    // Every write succeeds whatever the mirror does.
    let post = import_source(&surreal, &source("hooked", "Hooked"), Utc::now())
        .await
        .unwrap()
        .post_id;
    let (admin, entity, firm_id) = hooked_practice(&surreal).await;
    let address = crate::addresses::create(
        &surreal,
        &crate::addresses::NewAddress {
            line1: "100 Synthetic Way".into(),
            city: "Reno".into(),
            region: "NV".into(),
            postal_code: "89501".into(),
            country: "US".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mailroom = crate::mailrooms::create(&surreal, "Synthetic HQ", address.id)
        .await
        .unwrap();
    let incoming_id = Uuid::new_v4();
    crate::letters::record_with_id(
        &surreal,
        incoming_id,
        &crate::letters::NewLetter {
            mailroom_id: mailroom.id,
            project_id: None,
            direction: "incoming".into(),
            sender: "Synthetic Sender".into(),
            recipient: "Synthetic Recipient".into(),
            summary: "Synthetic incoming summary".into(),
        },
    )
    .await
    .unwrap();
    let dangling_project_id = Uuid::new_v4();
    let outgoing = crate::letters::record(
        &surreal,
        &crate::letters::NewLetter {
            mailroom_id: mailroom.id,
            project_id: Some(dangling_project_id),
            direction: "outgoing".into(),
            sender: "Synthetic Sender".into(),
            recipient: "Synthetic Recipient".into(),
            summary: "Synthetic outgoing summary".into(),
        },
    )
    .await
    .unwrap();
    let brand = crate::brands::create(
        &surreal,
        crate::persons::Role::Admin,
        Some(admin),
        &crate::brands::NewBrand {
            name: "Synthetic Hooked Brand".into(),
            key: format!("hooked-{}", Uuid::now_v7().simple()),
            firm_id: Some(firm_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let kms = cloud::FakeKms::new("synthetic-social-account-key");
    let granted_scopes = vec!["pages_manage_posts".to_string()];
    let account = crate::social_accounts::connect(
        &surreal,
        crate::social_accounts::ConnectSocialAccount {
            actor_role: crate::persons::Role::Admin,
            actor_person_id: admin,
            brand_key: &brand.key,
            provider: crate::social_accounts::SocialProvider::Facebook,
            provider_account_id: "synthetic-hook-page",
            display_name: Some("Synthetic Hook Page"),
            scopes: &granted_scopes,
            expires_at: Some(Utc::now() + chrono::Duration::days(7)),
            access_token: "synthetic-hook-access-token",
            refresh_token: Some("synthetic-hook-refresh-token"),
        },
        &kms,
    )
    .await
    .unwrap();
    assert!(crate::social_accounts::connected_account(
        &surreal,
        &brand.key,
        crate::social_accounts::SocialProvider::Facebook,
        "synthetic-hook-page",
    )
    .await
    .unwrap()
    .is_some());
    crate::social_handles::set(&surreal, BRAND, "x", "SyntheticMirror")
        .await
        .unwrap();
    crate::social_publications::admit(&surreal, post, "x")
        .await
        .unwrap();
    assert!(crate::posts::find_by_id(&surreal, post)
        .await
        .unwrap()
        .is_some());
    let replay = after_write(&surreal, MirrorKey::Post(post)).await;
    match case.as_str() {
        "disabled" => {
            assert_eq!(replay, MirrorOutcome::Disabled);
        }
        "unreachable" => {
            assert_eq!(replay, MirrorOutcome::Failed);
            // Up to 20 source-hook/replay mirror attempts occur in this
            // combined probe; each must stay within its per-write timeout.
            assert!(started.elapsed() < MIRROR_TIMEOUT * 20 + Duration::from_secs(5));
        }
        "off" => {
            assert_eq!(replay, MirrorOutcome::Disabled);
            let pg = PostgresDb::from_env().await.unwrap();
            for table in ["person", "entity", "entity_type", "jurisdiction", "firm"] {
                let rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT count(*) FROM {table}"
                )))
                .fetch_one(&pg.0)
                .await
                .unwrap();
                assert_eq!(rows, 0, "{table} written with the mirror off");
            }
            let social_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM social_account")
                .fetch_one(&pg.0)
                .await
                .unwrap();
            assert_eq!(social_rows, 0, "social account written with mirror off");
            for table in ["mailroom", "letter"] {
                let rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT count(*) FROM {table}"
                )))
                .fetch_one(&pg.0)
                .await
                .unwrap();
                assert_eq!(rows, 0, "{table} written with mirror off");
            }
        }
        "mirrored" => {
            assert_eq!(replay, MirrorOutcome::Mirrored(Mirrored::Unchanged));
            let pg = PostgresDb::from_env().await.unwrap();
            let mail = mailrooms_letters::parity(&surreal, &pg).await.unwrap();
            assert!(mail.is_clean(), "{mail:?}");
            let stored: (Option<Uuid>, String) =
                sqlx::query_as("SELECT project_id,direction FROM letter WHERE id=$1")
                    .bind(outgoing.id)
                    .fetch_one(&pg.0)
                    .await
                    .unwrap();
            assert_eq!(stored, (Some(dangling_project_id), "outgoing".into()));
            assert!(mailrooms_letters::parity(&surreal, &pg)
                .await
                .unwrap()
                .is_clean());
            // Person and Entity writes mirror themselves; a Firm command
            // mirrors the Firm and its Admin DRI membership.
            crate::persons::edit(
                &surreal,
                admin,
                &crate::persons::PersonEdit {
                    given_name: Some(Some("Synthetic".into())),
                    ..crate::persons::PersonEdit::default()
                },
            )
            .await
            .unwrap();
            crate::entities::set_avatar_url(&surreal, entity, Some("/synthetic/avatar".into()))
                .await
                .unwrap();
            let identity = identity::parity(&surreal, &pg).await.unwrap();
            assert!(identity.is_clean(), "{identity:?}");
            assert_eq!(
                identity
                    .tables
                    .iter()
                    .map(|table| table.target_rows)
                    .collect::<Vec<_>>(),
                [1, 1, 1, 1]
            );
            let given: Option<String> =
                sqlx::query_scalar("SELECT given_name FROM person WHERE id = $1")
                    .bind(admin)
                    .fetch_one(&pg.0)
                    .await
                    .unwrap();
            assert_eq!(given.as_deref(), Some("Synthetic"));
            let tenancy = tenancy::parity(&surreal, &pg).await.unwrap();
            for table in &tenancy.tables {
                if matches!(table.table, "firm" | "person_firm_role") {
                    assert!(table.divergent.is_empty(), "{table:?}");
                    assert_eq!(table.target_rows, 1, "{table:?}");
                }
            }
            let id = account.id.clone();
            let expected_bundle: Option<SocialAccountBundle> = surreal
                .query("SELECT ciphertext, wrapped_dek, kms_key_version, kms_context FROM ONLY $id")
                .bind((
                    "id",
                    surrealdb::types::RecordId::new("social_account", account.id.clone()),
                ))
                .await
                .unwrap()
                .check()
                .unwrap()
                .take::<Option<SocialAccountBundle>>(0)
                .unwrap();
            let mirrored_bundle: (String, String, String, String) = sqlx::query_as(
                "SELECT ciphertext, wrapped_dek, kms_key_version, kms_context FROM social_account WHERE id=$1",
            ).bind(&id).fetch_one(&pg.0).await.unwrap();
            let expected_bundle = expected_bundle.unwrap();
            assert_eq!(mirrored_bundle.0, expected_bundle.ciphertext);
            assert_eq!(mirrored_bundle.1, expected_bundle.wrapped_dek);
            assert_eq!(mirrored_bundle.2, expected_bundle.kms_key_version);
            assert_eq!(mirrored_bundle.3, expected_bundle.kms_context);
            crate::social_accounts::update_refreshed_tokens(
                &surreal,
                crate::social_accounts::RefreshSocialAccount {
                    brand_key: &brand.key,
                    provider: crate::social_accounts::SocialProvider::Facebook,
                    provider_account_id: "synthetic-hook-page",
                    expires_at: Utc::now() + chrono::Duration::days(14),
                    access_token: "synthetic-refreshed-access",
                    refresh_token: Some("synthetic-refreshed-refresh"),
                },
                &kms,
            )
            .await
            .unwrap();
            let mirrored_refresh: (String, String) =
                sqlx::query_as("SELECT ciphertext, wrapped_dek FROM social_account WHERE id=$1")
                    .bind(&id)
                    .fetch_one(&pg.0)
                    .await
                    .unwrap();
            let source_refresh: Option<SocialAccountTokens> = surreal
                .query("SELECT ciphertext, wrapped_dek FROM ONLY $id")
                .bind((
                    "id",
                    surrealdb::types::RecordId::new("social_account", account.id.clone()),
                ))
                .await
                .unwrap()
                .check()
                .unwrap()
                .take::<Option<SocialAccountTokens>>(0)
                .unwrap();
            let source_refresh = source_refresh.unwrap();
            assert_eq!(mirrored_refresh.0, source_refresh.ciphertext);
            assert_eq!(mirrored_refresh.1, source_refresh.wrapped_dek);
            crate::social_accounts::revoke(
                &surreal,
                crate::persons::Role::Admin,
                admin,
                &brand.key,
                crate::social_accounts::SocialProvider::Facebook,
                "synthetic-hook-page",
            )
            .await
            .unwrap();
            let revoked: (String, String, String) = sqlx::query_as(
                "SELECT status, ciphertext, wrapped_dek FROM social_account WHERE id=$1",
            )
            .bind(&id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
            assert_eq!(revoked, ("revoked".into(), String::new(), String::new()));
            // An asset write mirrors itself, and so does its removal.
            let tmp = tempfile::tempdir().unwrap();
            let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
                cloud::FsStorage::new(tmp.path().to_path_buf())
                    .await
                    .unwrap(),
            );
            let kept = crate::assets::ingest_content(&surreal, &storage, b"kept", "text/plain")
                .await
                .unwrap();
            let dropped =
                crate::assets::ingest_content(&surreal, &storage, b"dropped", "text/plain")
                    .await
                    .unwrap();
            crate::assets::delete(&surreal, dropped).await.unwrap();
            let documents = documents::parity(&surreal, &pg).await.unwrap();
            assert!(documents.is_clean(), "{documents:?}");
            assert_eq!(documents.tables[0].target_rows, 1);
            assert!(held_by_target(&pg, &MirrorKey::Asset(kept)).await.unwrap());
            // A Notation, its journal, and its answers mirror themselves.
            let project = crate::projects::create(
                &surreal,
                &crate::projects::NewProject {
                    code: format!("synthetic-{}", Uuid::now_v7().simple()),
                    name: "Synthetic Hooked Matter".into(),
                    status: "open".into(),
                    entity_id: entity,
                    ..crate::projects::NewProject::default()
                },
            )
            .await
            .unwrap()
            .id;
            let template = crate::templates::save_version(
                &surreal,
                None,
                "onboarding__synthetic-hooked",
                crate::templates::Version {
                    title: "Synthetic Draft".into(),
                    respondent_type: "person".into(),
                    asset_id: None,
                    form_code: None,
                    kind: None,
                    source_commit_sha: None,
                },
            )
            .await
            .unwrap()
            .into_model()
            .id;
            let replacement = crate::templates::save_version(
                &surreal,
                None,
                "onboarding__synthetic-hooked",
                crate::templates::Version {
                    title: "Synthetic Draft v2".into(),
                    respondent_type: "person".into(),
                    asset_id: None,
                    form_code: None,
                    kind: None,
                    source_commit_sha: None,
                },
            )
            .await
            .unwrap()
            .into_model()
            .id;
            assert_ne!(template, replacement, "a changed spec appends a version");
            let versions: Vec<(Uuid, bool, Option<String>)> =
                sqlx::query_as("SELECT id, is_current, current_key FROM template ORDER BY id")
                    .fetch_all(&pg.0)
                    .await
                    .unwrap();
            assert_eq!(versions.len(), 2);
            assert_eq!(versions[0], (template, false, None));
            assert!(versions[1].0 == replacement && versions[1].1 && versions[1].2.is_some());
            let template_parity = templates::parity(&surreal, &pg).await.unwrap();
            assert!(template_parity.is_clean(), "{template_parity:?}");
            // Simulate a lost retirement hook: the older target row still
            // claims the source's current key. Reconcile must retire it first,
            // then restore the current version without dropping either row.
            let current_key: String =
                sqlx::query_scalar("SELECT current_key FROM template WHERE id = $1")
                    .bind(replacement)
                    .fetch_one(&pg.0)
                    .await
                    .unwrap();
            sqlx::query("UPDATE template SET current_key = NULL WHERE id = $1")
                .bind(replacement)
                .execute(&pg.0)
                .await
                .unwrap();
            sqlx::query("UPDATE template SET is_current = true, current_key = $2 WHERE id = $1")
                .bind(template)
                .bind(&current_key)
                .execute(&pg.0)
                .await
                .unwrap();
            let lost_retirement = templates::parity(&surreal, &pg).await.unwrap();
            assert_eq!(lost_retirement.tables[0].divergent.len(), 2);
            let repaired = templates::reconcile(&surreal, &pg).await.unwrap();
            assert!(repaired.is_clean(), "{repaired:?}");
            let retained: Vec<(Uuid, bool, Option<String>)> =
                sqlx::query_as("SELECT id, is_current, current_key FROM template ORDER BY id")
                    .fetch_all(&pg.0)
                    .await
                    .unwrap();
            assert_eq!(retained.len(), 2);
            assert_eq!(retained[0], (template, false, None));
            assert_eq!(retained[1], (replacement, true, Some(current_key)));
            let project_template = crate::templates::save_version(
                &surreal,
                Some(project),
                "synthetic-project-template",
                crate::templates::Version {
                    title: "Synthetic Project Template".into(),
                    respondent_type: "person".into(),
                    asset_id: Some(kept),
                    form_code: None,
                    kind: None,
                    source_commit_sha: Some("synthetic-commit".into()),
                },
            )
            .await
            .unwrap()
            .into_model();
            let scoped: (Uuid, Uuid, String) = sqlx::query_as(
                "SELECT project_id, asset_id, current_key FROM template WHERE id = $1",
            )
            .bind(project_template.id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
            assert_eq!(scoped.0, project);
            assert_eq!(scoped.1, kept);
            assert_eq!(
                scoped.2,
                format!("{project}\u{1f}synthetic-project-template")
            );
            let duplicate_key = sqlx::query(
                "INSERT INTO template (id, code, title, respondent_type, project_id, is_current, \
                 current_key, inserted_at, updated_at) \
                 VALUES ($1, 'duplicate', 'Duplicate', 'person', $2, true, $3, now(), now())",
            )
            .bind(Uuid::now_v7())
            .bind(project)
            .bind(&scoped.2)
            .execute(&pg.0)
            .await
            .expect_err("PostgreSQL enforces the source current-key uniqueness rule");
            assert_eq!(
                duplicate_key
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code)
                    .as_deref(),
                Some("23505")
            );
            let missing_project = sqlx::query(
                "INSERT INTO template (id, code, title, respondent_type, project_id, is_current, \
                 inserted_at, updated_at) VALUES ($1, 'missing-project', 'Missing project', \
                 'person', $2, true, now(), now())",
            )
            .bind(Uuid::now_v7())
            .bind(Uuid::now_v7())
            .execute(&pg.0)
            .await
            .expect_err("template Project references are enforced");
            assert_eq!(
                missing_project
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code)
                    .as_deref(),
                Some("23503")
            );
            let missing_asset = sqlx::query(
                "INSERT INTO template (id, code, title, respondent_type, project_id, is_current, \
                 asset_id, inserted_at, updated_at) VALUES ($1, 'missing-asset', 'Missing asset', \
                 'person', $2, true, $3, now(), now())",
            )
            .bind(Uuid::now_v7())
            .bind(project)
            .bind(Uuid::now_v7())
            .execute(&pg.0)
            .await
            .expect_err("template body Asset references are enforced");
            assert_eq!(
                missing_asset
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code)
                    .as_deref(),
                Some("23503")
            );
            let all_template_parity = templates::parity(&surreal, &pg).await.unwrap();
            assert!(all_template_parity.is_clean(), "{all_template_parity:?}");
            let divergent = MirrorKey::Template(replacement);
            sqlx::query("UPDATE template SET title = 'Divergent' WHERE id = $1")
                .bind(replacement)
                .execute(&pg.0)
                .await
                .unwrap();
            let before = templates::parity(&surreal, &pg).await.unwrap();
            assert_eq!(before.tables[0].divergent, [divergent]);
            let repaired = templates::reconcile(&surreal, &pg).await.unwrap();
            assert!(repaired.is_clean(), "{repaired:?}");
            let notation = crate::notations::create(
                &surreal,
                &crate::notations::NewNotation::new(replacement, admin, project, "BEGIN"),
            )
            .await
            .unwrap()
            .id;
            crate::notation_events::append_event(
                &surreal,
                crate::notation_events::TransitionRecord {
                    notation_id: notation,
                    acting_person_id: None,
                    machine_kind: crate::notation_events::MACHINE_QUESTIONNAIRE,
                    from_state: "BEGIN",
                    to_state: "END",
                    condition: "_",
                    payload_json: None,
                    recorded_at: "2026-01-02T00:00:00+00:00",
                },
            )
            .await
            .unwrap();
            crate::answers::record(
                &surreal,
                &crate::answers::NewAnswer::new(
                    crate::questions::create(
                        &surreal,
                        &crate::questions::NewQuestion::new(
                            "synthetic_hooked",
                            "Synthetic?",
                            "string",
                        ),
                    )
                    .await
                    .unwrap()
                    .id,
                    admin,
                    crate::answers::primitive("Synthetic"),
                )
                .in_notation(notation, "client_name"),
            )
            .await
            .unwrap();
            crate::notations::update_state(&surreal, notation, "END")
                .await
                .unwrap();

            // Exercise the public write paths with mirroring enabled. These
            // assertions intentionally go through the store APIs rather than
            // invoking the mirror directly, so a missed hook or early return
            // leaves parity visibly broken.
            let occurred_at = "2026-01-02T00:00:00Z";
            let ingest = crate::communications::IngestArgs {
                project_id: project,
                channel: crate::communications::channel::EMAIL_INBOUND,
                direction: crate::communications::direction::INBOUND,
                author_person_id: None,
                counterparty: Some("synthetic@example.test"),
                subject: Some("Synthetic replay"),
                body: "synthetic body",
                source_ref: Some("<hooked-message@example.test>"),
                asset_id: None,
                occurred_at,
            };
            let first = crate::communications::ingest(&surreal, &ingest)
                .await
                .unwrap();
            let replay = crate::communications::ingest(&surreal, &ingest)
                .await
                .unwrap();
            assert!(!first.deduped);
            assert!(replay.deduped);
            assert_eq!(replay.communication_id, first.communication_id);

            let other_project = crate::projects::create(
                &surreal,
                &crate::projects::NewProject {
                    code: format!("synthetic-moved-{}", Uuid::now_v7().simple()),
                    name: "Synthetic Move Target".into(),
                    status: "open".into(),
                    entity_id: entity,
                    ..crate::projects::NewProject::default()
                },
            )
            .await
            .unwrap()
            .id;
            crate::communications::move_to_project(&surreal, first.communication_id, other_project)
                .await
                .unwrap();
            let moved_project: Uuid =
                sqlx::query_scalar("SELECT project_id FROM communication WHERE id = $1")
                    .bind(first.communication_id)
                    .fetch_one(&pg.0)
                    .await
                    .unwrap();
            assert_eq!(moved_project, other_project);

            let call = crate::communications::record_call(
                &surreal,
                &crate::communications::NewCall {
                    project_id: project,
                    author_person_id: Some(admin),
                    counterparty: "Synthetic caller".into(),
                    subject: Some("Synthetic call".into()),
                    body: "Synthetic call note".into(),
                    at: occurred_at.into(),
                    call: crate::communications::CallDetails {
                        direction: "inbound".into(),
                        counterparty_role: crate::communications::counterparty_role::CLIENT.into(),
                        recorded: crate::communications::recorded::NONE.into(),
                        ..Default::default()
                    },
                    agreements: vec!["Synthetic agreement".into()],
                    client_visible: false,
                },
            )
            .await
            .unwrap();
            crate::communications::confirm_agreements(&surreal, project, call.id, admin)
                .await
                .unwrap();

            let token = format!("hooked-{}", Uuid::now_v7().simple());
            let conversation = crate::email_conversations::open(
                &surreal,
                &crate::email_conversations::NewConversation {
                    token: &token,
                    external_email: "synthetic@example.test",
                    external_name: Some("Synthetic Sender"),
                    subject: "Synthetic thread",
                    person_id: None,
                    notation_id: None,
                },
            )
            .await
            .unwrap();
            let unassigned = crate::email_conversations::by_id(&surreal, conversation)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(unassigned.firm_id, None);
            assert_eq!(unassigned.project_id, None);
            let project_row = crate::projects::find_by_id(&surreal, project)
                .await
                .unwrap()
                .unwrap();
            crate::email_conversations::set_scope(
                &surreal,
                conversation,
                project_row.firm_id,
                Some(project),
            )
            .await
            .unwrap()
            .unwrap();
            crate::email_conversations::set_status(
                &surreal,
                conversation,
                crate::email_conversations::STATUS_AWAITING_LAWYER,
            )
            .await
            .unwrap()
            .unwrap();
            crate::email_conversations::set_notation(&surreal, conversation, notation)
                .await
                .unwrap()
                .unwrap();
            let message = crate::email_conversations::NewMessage {
                conversation_id: conversation,
                direction: crate::email_conversations::DIRECTION_FROM_EXTERNAL,
                from_addr: "synthetic@example.test",
                to_addr: "lawyer@example.test",
                subject: "Synthetic thread",
                body_text: "Synthetic message body",
                provider_message_id: Some("provider-reused"),
                ..Default::default()
            };
            let first_hop = crate::email_conversations::append(&surreal, &message)
                .await
                .unwrap();
            let second_hop = crate::email_conversations::append(&surreal, &message)
                .await
                .unwrap();
            assert_ne!(first_hop, second_hop);
            let repeated_provider_id: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM email_conversation_message WHERE conversation_id = $1 AND provider_message_id = $2",
            )
            .bind(conversation)
            .bind("provider-reused")
            .fetch_one(&pg.0)
            .await
            .unwrap();
            assert_eq!(repeated_provider_id, 2);

            crate::communications::purge_for_project(&surreal, &storage, other_project)
                .await
                .unwrap();
            let purged: i64 =
                sqlx::query_scalar("SELECT count(*) FROM communication WHERE id = $1")
                    .bind(first.communication_id)
                    .fetch_one(&pg.0)
                    .await
                    .unwrap();
            assert_eq!(purged, 0);
            let conversations = conversations::parity(&surreal, &pg).await.unwrap();
            assert!(conversations.is_clean(), "{conversations:?}");
            let workflow = workflow::parity(&surreal, &pg).await.unwrap();
            assert!(workflow.is_clean(), "{workflow:?}");
            assert_eq!(
                workflow
                    .tables
                    .iter()
                    .map(|table| table.target_rows)
                    .collect::<Vec<_>>(),
                [1, 1, 1]
            );
            let report = parity(&surreal, &pg).await.unwrap();
            assert!(report.is_clean(), "{report:?}");
            assert_eq!(
                report
                    .tables
                    .iter()
                    .map(|table| table.target_rows)
                    .collect::<Vec<_>>(),
                [1, 1, 1]
            );
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one row lifecycle across all three tables reads in order"
)]
async fn create_update_and_delete_converge_and_replays_write_nothing() {
    let Some((options, database)) = fresh_database().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = test_support::mem_surreal().await;
    let now = Utc::now();

    let post = import_source(&surreal, &source("mirrored", "First"), now)
        .await
        .unwrap()
        .post_id;
    let key = MirrorKey::Post(post);
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Unchanged
    );

    import_source(&surreal, &source("mirrored", "Edited"), now)
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Written
    );
    let title: String = sqlx::query_scalar("SELECT title FROM post WHERE id = $1")
        .bind(post)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(title, "Edited");

    let handle = MirrorKey::SocialHandle {
        brand_key: BRAND.into(),
        provider: "x".into(),
    };
    crate::social_handles::set(&surreal, BRAND, "x", "SyntheticOne")
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &handle).await.unwrap(),
        Mirrored::Written
    );
    crate::social_handles::set(&surreal, BRAND, "x", "SyntheticTwo")
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &handle).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &handle).await.unwrap(),
        Mirrored::Unchanged
    );

    let publication = MirrorKey::SocialPublication {
        post_id: post,
        provider: "x".into(),
    };
    crate::social_publications::admit(&surreal, post, "x")
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &publication).await.unwrap(),
        Mirrored::Written
    );
    crate::social_publications::mark_failed(&surreal, post, "x", "synthetic failure")
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &publication).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &publication).await.unwrap(),
        Mirrored::Unchanged
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());

    surreal
        .query(
            "DELETE social_publication WHERE post_id = $post; DELETE $post; DELETE social_handle",
        )
        .bind(("post", crate::surreal::record_id("post", post)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Deleted
    );
    assert_eq!(
        mirror(&surreal, &pg, &publication).await.unwrap(),
        Mirrored::Unchanged
    );
    assert_eq!(
        mirror(&surreal, &pg, &handle).await.unwrap(),
        Mirrored::Deleted
    );
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Unchanged
    );
    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert!(report.tables.iter().all(|table| table.target_rows == 0));
}

#[tokio::test]
async fn a_one_sided_failure_is_reported_by_parity_and_reconciled() {
    let Some((options, database)) = fresh_database().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = test_support::mem_surreal().await;

    // PostgreSQL does not hold this Brand yet, so the mirror is refused while
    // the Surreal write stands.
    let mut orphan = source("orphan", "Orphan");
    orphan.brand_key = "synthetic-unmirrored";
    let post = import_source(&surreal, &orphan, Utc::now())
        .await
        .unwrap()
        .post_id;
    let error = mirror(&surreal, &pg, &MirrorKey::Post(post))
        .await
        .unwrap_err();
    assert!(matches!(error, MirrorError::Target(ref code) if code == "23503"));

    // A row PostgreSQL holds that SurrealDB does not is divergent too.
    sqlx::query(
        "INSERT INTO social_handle (brand_key, provider, handle) VALUES ($1, 'x', 'Stale')",
    )
    .bind(BRAND)
    .execute(&pg.0)
    .await
    .unwrap();

    let report = parity(&surreal, &pg).await.unwrap();
    assert_eq!(
        report.divergent().cloned().collect::<Vec<_>>(),
        [
            MirrorKey::Post(post),
            MirrorKey::SocialHandle {
                brand_key: BRAND.into(),
                provider: "x".into()
            }
        ]
    );

    insert_brand(&pg.0, "synthetic-unmirrored").await;
    let report = reconcile(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.tables[0].target_rows, 1);
    assert_eq!(report.tables[1].target_rows, 0);
}

#[tokio::test]
async fn projection_retry_and_recovery_converge_create_update_delete_and_lost_ack() {
    let Some((options, database)) = fresh_database().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = test_support::mem_surreal().await;
    let post = import_source(&surreal, &source("recovery", "First"), Utc::now())
        .await
        .unwrap()
        .post_id;
    let key = MirrorKey::Post(post);
    let policy = CompletionPolicy {
        attempt_timeout: Duration::from_secs(10),
        timeout: Duration::from_secs(30),
        retry_delay: Duration::ZERO,
        ..COMPLETION
    };

    // SQL commits but its acknowledgement is lost. Retrying the projection
    // keeps one identity and never repeats the source mutation.
    let mut attempts = 0;
    let result = complete(policy, || {
        attempts += 1;
        let attempt = attempts;
        let (surreal, pg, key) = (&surreal, &pg, &key);
        async move {
            let result = mirror(surreal, pg, key).await?;
            if attempt == 1 {
                Err(MirrorError::Unavailable)
            } else {
                Ok(result)
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(result, Mirrored::Unchanged);
    assert_eq!(attempts, 2);
    assert_eq!(
        parity(&surreal, &pg).await.unwrap().tables[0].target_rows,
        1
    );

    // A newer source update wins before the retry reads the row again.
    surreal
        .query("UPDATE $post SET title = 'Second'")
        .bind(("post", crate::surreal::record_id("post", post)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let mut attempts = 0;
    let result = complete(policy, || {
        attempts += 1;
        let attempt = attempts;
        let (surreal, pg, key) = (&surreal, &pg, &key);
        async move {
            if attempt == 1 {
                Err(MirrorError::Timeout)
            } else {
                mirror(surreal, pg, key).await
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(result, Mirrored::Written);
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());

    pg.0.close().await;
    let pg = connect(&options, &database).await;

    // Model a stale in-flight projection finishing after a newer source
    // update. A fresh scan has no dependency on in-process retry state.
    surreal
        .query("UPDATE $post SET title = 'Third'")
        .bind(("post", crate::surreal::record_id("post", post)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(!parity(&surreal, &pg).await.unwrap().is_clean());
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());

    surreal
        .query("DELETE $post")
        .bind(("post", crate::surreal::record_id("post", post)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(!parity(&surreal, &pg).await.unwrap().is_clean());
    let repaired = reconcile(&surreal, &pg).await.unwrap();
    assert!(repaired.is_clean());
    assert_eq!(repaired.tables[0].target_rows, 0);
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Unchanged
    );
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
}

#[tokio::test]
async fn a_reused_slug_replaces_the_stale_postgres_post() {
    let Some((options, database)) = fresh_database().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = test_support::mem_surreal().await;
    let first = import_source(&surreal, &source("reused", "First"), Utc::now())
        .await
        .unwrap()
        .post_id;
    mirror(&surreal, &pg, &MirrorKey::Post(first))
        .await
        .unwrap();
    crate::social_publications::admit(&surreal, first, "x")
        .await
        .unwrap();
    mirror(
        &surreal,
        &pg,
        &MirrorKey::SocialPublication {
            post_id: first,
            provider: "x".into(),
        },
    )
    .await
    .unwrap();
    surreal
        .query("DELETE social_publication; DELETE $post")
        .bind(("post", crate::surreal::record_id("post", first)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let second = import_source(&surreal, &source("reused", "Second"), Utc::now())
        .await
        .unwrap()
        .post_id;
    assert_ne!(first, second);
    assert_eq!(
        mirror(&surreal, &pg, &MirrorKey::Post(second))
            .await
            .unwrap(),
        Mirrored::Written
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
}

/// Rows PostgreSQL lacks, a row whose column differs, a stale Person held by
/// a stale membership, and a stale Brand held by a stale Social Handle: each
/// stale parent belongs to an earlier mirror than its stale child. Parity
/// reports the drift without writing, reconcile repairs it, and a second
/// reconcile writes nothing.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one seeded-drift operator run reads in order"
)]
async fn every_mirror_reports_reconciles_and_settles_seeded_drift() {
    use crate::firms::{FirmEdit, NewFirm};
    use crate::persons::{NewPerson, Role};

    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = test_support::mem_surreal().await;
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();
    let entity = synthetic_entity(&surreal).await;
    let new_person = |label: &str| {
        NewPerson::with_role(
            "Synthetic Operator",
            format!("{label}-{}@example.com", Uuid::now_v7()),
            Role::Admin,
        )
    };
    let admin = crate::persons::create(&surreal, &new_person("operator"))
        .await
        .unwrap()
        .id;
    for key in [MirrorKey::Entity(entity), MirrorKey::Person(admin)] {
        mirror(&surreal, &pg, &key).await.unwrap();
    }
    // A Person PostgreSQL has never seen: missing.
    let late = crate::persons::create(&surreal, &new_person("late"))
        .await
        .unwrap()
        .id;

    // Seed an actual source-domain record, mirror its initial state, then
    // change the source so this group reports and settles whole-row drift.
    let route_count = Uuid::now_v7();
    let at = "2026-10-21T01:02:03.123456789+00:00";
    surreal
        .query(
            "CREATE $id SET bucket_date = type::datetime($at), country_code = 'ZZ', \
             route_pattern = '/synthetic', source = 'fixture', locale = 'en', \
             status_class = '2xx', visits = 17, inserted_at = type::datetime($at), \
             updated_at = type::datetime($at)",
        )
        .bind((
            "id",
            crate::surreal::record_id("visitor_route_count", route_count),
        ))
        .bind(("at", at))
        .await
        .unwrap()
        .check()
        .unwrap();
    mirror(
        &surreal,
        &pg,
        &MirrorKey::Record {
            table: "visitor_route_count",
            id: route_count,
        },
    )
    .await
    .unwrap();
    surreal
        .query("UPDATE $id SET visits = 18")
        .bind((
            "id",
            crate::surreal::record_id("visitor_route_count", route_count),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();

    // A Firm mirrored once, then renamed without its mirror: differing.
    let firm = crate::firms::create(
        &surreal,
        &NewFirm {
            name: "Synthetic Practice".into(),
            status: "active".into(),
            entity_id: entity,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap();
    mirror(&surreal, &pg, &MirrorKey::Firm(firm.id))
        .await
        .unwrap();
    crate::firms::update(
        &surreal,
        Role::Owner,
        None,
        firm.id,
        &FirmEdit {
            name: Some("Synthetic Practice Renamed".into()),
            ..FirmEdit::default()
        },
    )
    .await
    .unwrap();
    // A Brand and a Post PostgreSQL has never seen: missing.
    let brand = crate::brands::create(
        &surreal,
        Role::Admin,
        Some(admin),
        &crate::brands::NewBrand {
            name: "Synthetic Operator Brand".into(),
            key: BRAND.into(),
            firm_id: Some(firm.id),
            ..crate::brands::NewBrand::default()
        },
    )
    .await
    .unwrap();
    let post = import_source(&surreal, &source("operator", "Operator"), Utc::now())
        .await
        .unwrap()
        .post_id;
    // A stale Brand held by a stale Social Handle: extra in both mirrors.
    insert_brand(&pg.0, "synthetic-stale").await;
    sqlx::query(
        "INSERT INTO social_handle (brand_key, provider, handle) \
         VALUES ('synthetic-stale', 'x', 'Stale')",
    )
    .execute(&pg.0)
    .await
    .unwrap();
    // A stale Person held by a stale membership in the Firm: extra in the
    // identity and tenancy mirrors.
    let stale_person = Uuid::new_v4();
    sqlx::query("INSERT INTO person (id, name, email) VALUES ($1, 'Stale Person', $2)")
        .bind(stale_person)
        .bind(format!("{stale_person}@example.com"))
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO person_firm_role (person_id, firm_id, membership) VALUES ($1, $2, 'clerk')",
    )
    .bind(stale_person)
    .bind(firm.id)
    .execute(&pg.0)
    .await
    .unwrap();

    let table = |reports: &[MirrorParity], name: &str| -> TableParity {
        reports
            .iter()
            .flat_map(|mirror| mirror.report.tables.iter())
            .find(|table| table.table == name)
            .cloned()
            .unwrap()
    };
    let before = parity_all(&surreal, &pg).await.unwrap();
    assert_eq!(
        before
            .iter()
            .map(|mirror| mirror.mirror)
            .collect::<Vec<_>>(),
        [
            "identity",
            "tenancy",
            "social_accounts",
            "social_account_selections",
            "firm_integration_secrets",
            "documents",
            "post",
            "blog_videos",
            "identity_detail",
            "mailrooms_letters",
            "leads",
            "catalog",
            "workflow",
            "notation_clauses",
            "attestations",
            "templates",
            "communications",
            "deadlines",
            "sent_email",
            "project_services",
            "project_references",
            "communications_conversations",
            "inbox",
            "email_deliveries",
            "matter_pages_captions",
            "project_records",
            "notation_inputs",
            "git_access",
            "source_domain_records"
        ]
    );
    let route_counts = table(&before, "visitor_route_count");
    assert_eq!(
        route_counts.divergent,
        [MirrorKey::Record {
            table: "visitor_route_count",
            id: route_count
        }]
    );
    assert_eq!(
        drift(&pg, &route_counts).await.unwrap(),
        Drift {
            differing: 1,
            ..Drift::default()
        }
    );
    // The in-memory engine seeds its own People, so only the two named rows
    // are asserted by key; the stale one is the table's only extra row.
    let people = table(&before, "person");
    assert!(people.divergent.contains(&MirrorKey::Person(late)));
    assert!(people.divergent.contains(&MirrorKey::Person(stale_person)));
    assert!(!people.divergent.contains(&MirrorKey::Person(admin)));
    let people_drift = drift(&pg, &people).await.unwrap();
    assert_eq!((people_drift.extra, people_drift.differing), (1, 0));
    assert!(people_drift.missing >= 1);
    assert_eq!(
        table(&before, "person_firm_role").divergent,
        [
            MirrorKey::FirmMembership {
                person_id: admin,
                firm_id: firm.id
            },
            MirrorKey::FirmMembership {
                person_id: stale_person,
                firm_id: firm.id
            }
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
    let firms = table(&before, "firm");
    assert_eq!(firms.divergent, [MirrorKey::Firm(firm.id)]);
    assert_eq!(
        drift(&pg, &firms).await.unwrap(),
        Drift {
            differing: 1,
            ..Drift::default()
        }
    );
    let brands = table(&before, "brand");
    assert!(brands.divergent.contains(&MirrorKey::Brand(brand.id)));
    assert_eq!(
        drift(&pg, &brands).await.unwrap(),
        Drift {
            missing: 1,
            extra: 1,
            differing: 0
        }
    );
    assert_eq!(table(&before, "post").divergent, [MirrorKey::Post(post)]);
    assert_eq!(
        drift(&pg, &table(&before, "post")).await.unwrap(),
        Drift {
            missing: 1,
            ..Drift::default()
        }
    );
    assert_eq!(
        drift(&pg, &table(&before, "social_handle")).await.unwrap(),
        Drift {
            extra: 1,
            ..Drift::default()
        }
    );

    // Comparing writes nothing: the same drift stands.
    assert_eq!(parity_all(&surreal, &pg).await.unwrap(), before);

    // Reconcile converges, deleting each stale child before its parent.
    let after = reconcile_all(&surreal, &pg).await.unwrap();
    assert!(
        after.iter().all(|mirror| mirror.report.is_clean()),
        "{after:?}"
    );
    let settled_visits: i64 =
        sqlx::query_scalar("SELECT visits FROM visitor_route_count WHERE id = $1")
            .bind(route_count)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(settled_visits, 18);
    let stale: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM brand WHERE brand_key = $1) \
         + (SELECT count(*) FROM person WHERE id = $2)",
    )
    .bind("synthetic-stale")
    .bind(stale_person)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(stale, 0);
    let held: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM person WHERE id = $1)")
        .bind(late)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert!(held);
    let name: String = sqlx::query_scalar("SELECT name FROM firm WHERE id = $1")
        .bind(firm.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(name, "Synthetic Practice Renamed");

    // A second reconcile has nothing to mirror and changes no row.
    let stamps = "SELECT string_agg(id::text || ' ' || updated_at::text, ',' ORDER BY id) \
                  FROM (SELECT id, updated_at FROM person UNION ALL \
                  SELECT id, updated_at FROM firm UNION ALL \
                  SELECT id, updated_at FROM brand UNION ALL \
                  SELECT id, updated_at FROM post) AS stamped";
    let settled: String = sqlx::query_scalar(stamps).fetch_one(&pg.0).await.unwrap();
    assert_eq!(reconcile_all(&surreal, &pg).await.unwrap(), after);
    let again: String = sqlx::query_scalar(stamps).fetch_one(&pg.0).await.unwrap();
    assert_eq!(again, settled);
}

#[tokio::test]
async fn receipt_backed_cost_accepts_normal_project_document_and_replays_idempotently() {
    async fn project(surreal: &SurrealDb, name: &str) -> Uuid {
        let entity_id = synthetic_entity(surreal).await;
        crate::projects::create(
            surreal,
            &crate::projects::NewProject {
                code: crate::projects::code_from_name(name, Uuid::new_v4()),
                name: name.to_owned(),
                status: "open".to_owned(),
                entity_id,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = test_support::mem_surreal().await;
    let project_id = project(&surreal, "Costs Synthetic Matter").await;
    let actor_id = test_support::dri_person(&surreal).await;
    let temp = tempfile::tempdir().unwrap();
    let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
        cloud::FsStorage::new(temp.path().to_path_buf())
            .await
            .unwrap(),
    );
    let document = crate::documents::ingest_bytes(
        &surreal,
        &storage,
        &crate::documents::IngestArgs {
            project_id,
            source: crate::documents::source::UPLOAD,
            filename: "receipt.pdf",
            kind: "unclassified",
            content_type: "application/pdf",
            description: Some("Synthetic receipt"),
            secondary_storage_key: None,
            visibility: crate::documents::visibility::INTERNAL,
        },
        b"synthetic receipt bytes",
    )
    .await
    .unwrap();
    let input = crate::project_costs::NewProjectCost {
        occurred_on: chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
        payee: "Synthetic Vendor".into(),
        description: "Filing copy".into(),
        amount_cents: 1250,
        currency: "USD".into(),
        amount_status: "estimated".into(),
        receipt_document_id: document.asset_id,
        idempotency_key: "receipt:synthetic-1".into(),
    };
    let other_project = project(&surreal, "Other Costs Synthetic Matter").await;
    let mut foreign_receipt = input.clone();
    foreign_receipt.idempotency_key = "receipt:foreign-project".into();
    assert!(matches!(
        crate::project_costs::create(&surreal, &pg, other_project, actor_id, &foreign_receipt)
            .await,
        Err(crate::project_costs::ProjectCostError::Invalid(
            "receipt must be a live Document on this Project"
        ))
    ));
    let first = crate::project_costs::create(&surreal, &pg, project_id, actor_id, &input)
        .await
        .unwrap();
    assert_eq!(first.amount_status, "estimated");
    let replay = crate::project_costs::create(&surreal, &pg, project_id, actor_id, &input)
        .await
        .unwrap();
    assert_eq!(replay.id, first.id);
    let (left, right) = tokio::join!(
        crate::project_costs::create(&surreal, &pg, project_id, actor_id, &input),
        crate::project_costs::create(&surreal, &pg, project_id, actor_id, &input),
    );
    assert_eq!(left.unwrap().id, first.id);
    assert_eq!(right.unwrap().id, first.id);
    let mut changed = input.clone();
    changed.description = "Different synthetic payload".into();
    assert!(matches!(
        crate::project_costs::create(&surreal, &pg, project_id, actor_id, &changed).await,
        Err(crate::project_costs::ProjectCostError::Conflict)
    ));
    let confirmed = crate::project_costs::confirm_amount(&pg, first.id, actor_id, 1500)
        .await
        .unwrap();
    assert_eq!(confirmed.amount_status, "confirmed");
    assert_eq!(confirmed.amount_cents, 1500);
    assert_invoice_link_contract(&surreal, &pg, project_id, other_project, actor_id, first.id)
        .await;
    assert_receipt_visibility_and_audit(&surreal, &pg, project_id, document.asset_id, first.id)
        .await;
}

async fn assert_invoice_link_contract(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    project_id: Uuid,
    other_project: Uuid,
    actor_id: Uuid,
    cost_id: Uuid,
) {
    let invoice = |project_id, id: &str, status: &str, currency: &str| {
        crate::xero_invoices::UpsertXeroInvoice {
            project_id,
            xero_invoice_id: id.to_owned(),
            reference: format!("Synthetic {id}"),
            status: status.to_owned(),
            amount_cents: 1500,
            currency: currency.to_owned(),
            issued_at: chrono::Utc::now(),
            due_at: None,
        }
    };
    for value in [
        invoice(project_id, "synthetic-wrong-currency", "AUTHORISED", "CAD"),
        invoice(project_id, "synthetic-voided", "VOIDED", "USD"),
        invoice(
            other_project,
            "synthetic-foreign-project",
            "AUTHORISED",
            "USD",
        ),
    ] {
        crate::xero_invoices::upsert(surreal, &value).await.unwrap();
    }
    for (id, expected) in [
        (
            "synthetic-wrong-currency",
            "invoice currency does not match the cost currency",
        ),
        ("synthetic-voided", "invoice is voided"),
        (
            "synthetic-foreign-project",
            "invoice belongs to another Project",
        ),
    ] {
        assert!(matches!(
            crate::project_costs::link_invoice(surreal, pg, cost_id, actor_id, id).await,
            Err(crate::project_costs::ProjectCostError::Invalid(message)) if message == expected
        ));
    }
    crate::xero_invoices::upsert(
        surreal,
        &invoice(project_id, "synthetic-valid-invoice", "AUTHORISED", "USD"),
    )
    .await
    .unwrap();
    let linked = crate::project_costs::link_invoice(
        surreal,
        pg,
        cost_id,
        actor_id,
        "synthetic-valid-invoice",
    )
    .await
    .unwrap();
    assert_eq!(linked.billing_status, "invoiced");
}

async fn assert_receipt_visibility_and_audit(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    project_id: Uuid,
    document_id: Uuid,
    cost_id: Uuid,
) {
    assert!(
        crate::project_costs::list_client_visible(surreal, pg, project_id)
            .await
            .unwrap()
            .is_empty()
    );
    crate::assets::set_visibility(
        surreal,
        project_id,
        document_id,
        crate::documents::visibility::CLIENT,
    )
    .await
    .unwrap();
    assert_eq!(
        crate::project_costs::list_client_visible(surreal, pg, project_id)
            .await
            .unwrap()
            .len(),
        1
    );
    crate::assets::set_visibility(
        surreal,
        project_id,
        document_id,
        crate::documents::visibility::INTERNAL,
    )
    .await
    .unwrap();
    assert!(
        crate::project_costs::list_client_visible(surreal, pg, project_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        sqlx::query("UPDATE project_costs SET description='unaudited' WHERE id=$1")
            .bind(cost_id)
            .execute(&pg.0)
            .await
            .is_err()
    );
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM project_cost_events WHERE project_cost_id=$1")
            .bind(cost_id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(events, 3);
}

/// One key of every shape the mirror registry names.
#[allow(clippy::too_many_lines)] // One line per key variant keeps the inventory auditable.
fn every_key_shape(id: Uuid, other: Uuid) -> Vec<MirrorKey> {
    vec![
        MirrorKey::SentEmail(id),
        MirrorKey::EmailReceipt(id),
        MirrorKey::EmailDelivery(id),
        MirrorKey::EmailDeliveryAttempt(id),
        MirrorKey::Post(id),
        MirrorKey::BlogVideo(id),
        MirrorKey::SocialHandle {
            brand_key: BRAND.into(),
            provider: "x".into(),
        },
        MirrorKey::SocialPublication {
            post_id: id,
            provider: "x".into(),
        },
        MirrorKey::SocialAccount(format!("social_account:{id}")),
        MirrorKey::SocialAccountSelection {
            brand_key: BRAND.into(),
            provider: "facebook".into(),
        },
        MirrorKey::FirmIntegrationSecrets(id),
        MirrorKey::Firm(id),
        MirrorKey::Brand(id),
        MirrorKey::FirmBrand {
            brand_key: BRAND.into(),
        },
        MirrorKey::Project(id),
        MirrorKey::ProjectReference {
            project_id: id,
            party_kind: "person".into(),
            party_id: other,
            reason: "conflict_check".into(),
        },
        MirrorKey::FirmMembership {
            person_id: other,
            firm_id: id,
        },
        MirrorKey::ProjectParticipation(id),
        MirrorKey::Jurisdiction(id),
        MirrorKey::EntityType(id),
        MirrorKey::Entity(id),
        MirrorKey::Person(id),
        MirrorKey::Authority(id),
        MirrorKey::GlossaryTerm(id),
        MirrorKey::Question(id),
        MirrorKey::NotationPackage(id),
        MirrorKey::NotationPackageTemplate(id),
        MirrorKey::NotationRate(id),
        MirrorKey::Asset(id),
        MirrorKey::Notation(id),
        MirrorKey::NotationEvent(id),
        MirrorKey::Answer(id),
        MirrorKey::NotationClause(id),
        MirrorKey::Attestation(id),
        MirrorKey::Address(id),
        MirrorKey::Credential(id),
        MirrorKey::EmailToken(id),
        MirrorKey::EntityRole {
            person_id: other,
            entity_id: id,
            role: "member".into(),
        },
        MirrorKey::PersonDelegate(id),
        MirrorKey::ExternalIdentity(id),
        MirrorKey::PersonMailbox(id),
        MirrorKey::SummarySenderApproval(id),
        MirrorKey::SummarySenderApprovalEvent(id),
        MirrorKey::ProjectDeadline(id),
        MirrorKey::ProjectService(id),
        MirrorKey::Communication(id),
        MirrorKey::EmailConversation(id),
        MirrorKey::EmailConversationMessage(id),
        MirrorKey::InboxConversation(id),
        MirrorKey::InboxMessage(id),
        MirrorKey::InboxIngress(id),
        MirrorKey::InboxAssignment(id),
        MirrorKey::InboxDeliveryAttempt(id),
        MirrorKey::Mailroom(id),
        MirrorKey::Letter(id),
        MirrorKey::Lead(id),
        MirrorKey::LeadConsent(id),
        MirrorKey::Template(id),
        MirrorKey::MatterPages(id),
        MirrorKey::MatterCaption(id),
        MirrorKey::Record {
            table: "testimonial",
            id,
        },
        MirrorKey::Record {
            table: "firm_anchor",
            id,
        },
        MirrorKey::RecordString {
            table: "iolta_account",
            id: "synthetic-account".into(),
        },
        MirrorKey::RecordString {
            table: "firm_anchor",
            id: "synthetic-anchor".into(),
        },
    ]
}

#[test]
fn every_key_names_its_table_and_reports_only_uuid_identifiers() {
    let (id, other) = (Uuid::now_v7(), Uuid::now_v7());
    for key in every_key_shape(id, other) {
        let rendered = key.to_string();
        assert!(!rendered.is_empty());
        let table = key.table();
        assert!(!table.is_empty());
        if let Some(row) = row_id(&key) {
            assert_eq!(row, id, "{rendered}");
            assert!(rendered.contains(&id.to_string()), "{rendered}");
        }
        match key.report_id() {
            Some(report) => {
                assert!(report.contains(&id.to_string()), "{rendered}");
                assert!(!report.contains(BRAND), "{rendered}");
            }
            None => assert!(
                row_id(&key).is_none()
                    || matches!(
                        key,
                        MirrorKey::Record { .. }
                            | MirrorKey::EmailReceipt(_)
                            | MirrorKey::EmailDelivery(_)
                            | MirrorKey::EmailDeliveryAttempt(_)
                            | MirrorKey::NotationEvent(_)
                            | MirrorKey::Answer(_)
                            | MirrorKey::NotationClause(_)
                            | MirrorKey::SummarySenderApproval(_)
                            | MirrorKey::SummarySenderApprovalEvent(_)
                            | MirrorKey::ProjectDeadline(_)
                            | MirrorKey::ProjectService(_)
                            | MirrorKey::InboxConversation(_)
                            | MirrorKey::InboxMessage(_)
                            | MirrorKey::InboxIngress(_)
                            | MirrorKey::InboxAssignment(_)
                            | MirrorKey::InboxDeliveryAttempt(_)
                            | MirrorKey::Mailroom(_)
                            | MirrorKey::Letter(_)
                            | MirrorKey::Lead(_)
                            | MirrorKey::LeadConsent(_)
                    ),
                "{rendered}"
            ),
        }
    }
    assert_eq!(
        MirrorKey::FirmMembership {
            person_id: other,
            firm_id: id
        }
        .report_id(),
        Some(format!("{id}/{other}"))
    );
    assert_eq!(
        MirrorKey::RecordString {
            table: "iolta_account",
            id: "synthetic-account".into()
        }
        .to_string(),
        "iolta_account:synthetic-account"
    );
}

#[tokio::test]
async fn an_empty_target_holds_no_key_of_any_shape() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (id, other) = (Uuid::now_v7(), Uuid::now_v7());
    for key in every_key_shape(id, other) {
        assert!(!held_by_target(&pg, &key).await.unwrap(), "{}", key.table());
    }
    let table = TableParity {
        table: "person",
        source_rows: 1,
        target_rows: 0,
        divergent: vec![MirrorKey::Person(id)],
    };
    assert_eq!(
        missing(&pg, &table).await.unwrap(),
        vec![MirrorKey::Person(id)]
    );
    assert_eq!(
        drift(&pg, &table).await.unwrap(),
        Drift {
            missing: 1,
            extra: 0,
            differing: 0
        }
    );
}
