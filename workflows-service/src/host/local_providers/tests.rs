use super::*;
use chrono::{TimeZone, Utc};
use store::postgres::PostgresDb;
use workflows::dbos_engine::{self, billing_canary, dri_digest, general_nag, notation_reset};

#[path = "../../../tests/support/native_project.rs"]
mod native_project;

fn native(key: &str) -> Option<String> {
    use store::workflow_engine as engine;
    match key {
        "NAVIGATOR_CI_HARNESS" => Some("1".into()),
        "NAVIGATOR_STAGING" => Some("false".into()),
        engine::ENGINE_ENV => Some("dbos".into()),
        engine::COHOST_ENV | engine::DBOS_ENABLED_ENV => Some("true".into()),
        workflows::flags::BILLING_CANARY_SCHEDULE_ENABLED_ENV
        | workflows::flags::RECONCILE_INVOICES_SCHEDULE_ENABLED_ENV => Some("false".into()),
        name if engine::WORKFLOWS.iter().any(|item| item.flag == name) => Some("true".into()),
        _ => None,
    }
}

fn local() -> LocalProviders {
    LocalProviders::from_lookup(
        HostMode::Embedded,
        DeploymentEnvironment::Dev,
        true,
        &native,
    )
    .unwrap()
    .unwrap()
}

#[test]
fn local_capture_requires_the_actual_dev_embedded_simulated_harness() {
    for (mode, environment, simulated) in [
        (HostMode::Standalone, DeploymentEnvironment::Dev, true),
        (HostMode::Embedded, DeploymentEnvironment::Production, true),
        (HostMode::Embedded, DeploymentEnvironment::Dev, false),
    ] {
        assert!(
            LocalProviders::from_lookup(mode, environment, simulated, &native)
                .unwrap()
                .is_none()
        );
    }
    for ci in [None, Some("0"), Some("true"), Some(" 1")] {
        let get = |key: &str| {
            if key == "NAVIGATOR_CI_HARNESS" {
                ci.map(str::to_owned)
            } else {
                native(key)
            }
        };
        assert!(LocalProviders::from_lookup(
            HostMode::Embedded,
            DeploymentEnvironment::Dev,
            true,
            &get
        )
        .unwrap()
        .is_none());
    }
    for staging in ["true", "", "sometimes", " false"] {
        let get = |key: &str| {
            if key == "NAVIGATOR_STAGING" {
                Some(staging.into())
            } else {
                native(key)
            }
        };
        assert!(LocalProviders::from_lookup(
            HostMode::Embedded,
            DeploymentEnvironment::Dev,
            true,
            &get
        )
        .is_err());
    }
    assert!(LocalProviders::from_lookup(
        HostMode::Embedded,
        DeploymentEnvironment::Dev,
        true,
        &native
    )
    .unwrap()
    .is_some());
}

#[test]
fn partial_hosts_keep_strict_provider_selection_even_with_explicit_delivery() {
    use store::workflow_engine as engine;
    let keys = [
        engine::COHOST_ENV,
        engine::DBOS_ENABLED_ENV,
        engine::ENGINE_ENV,
    ];
    for key in keys
        .into_iter()
        .chain(engine::WORKFLOWS.iter().map(|item| item.flag))
    {
        for replacement in [
            None,
            Some(if key == engine::ENGINE_ENV {
                "restate"
            } else {
                "false"
            }),
        ] {
            let get = |name: &str| {
                if name == key {
                    replacement.map(str::to_owned)
                } else if name == "SLACK_BOT_TOKEN" {
                    Some("explicit-configuration".into())
                } else {
                    native(name)
                }
            };
            assert!(LocalProviders::from_lookup(
                HostMode::Embedded,
                DeploymentEnvironment::Dev,
                true,
                &get
            )
            .unwrap()
            .is_none());
        }
    }
    let invalid = |name: &str| {
        if name == engine::WORKFLOWS[0].flag {
            Some("sometimes".into())
        } else {
            native(name)
        }
    };
    assert!(LocalProviders::from_lookup(
        HostMode::Embedded,
        DeploymentEnvironment::Dev,
        true,
        &invalid
    )
    .is_err());
}

#[test]
fn explicit_delivery_configuration_is_never_silently_replaced() {
    for key in EXPLICIT_DELIVERY {
        for value in ["explicit-configuration", " "] {
            let get = |name: &str| {
                if name == *key {
                    Some(value.into())
                } else {
                    native(name)
                }
            };
            let error = LocalProviders::from_lookup(
                HostMode::Embedded,
                DeploymentEnvironment::Dev,
                true,
                &get,
            )
            .err()
            .unwrap();
            if !value.trim().is_empty() {
                assert!(
                    !format!("{error:#}").contains(value),
                    "configuration values stay private"
                );
            }
        }
    }
    let source = SurrealDb::uninitialized();
    assert!(general_nag::Context::from_lookup(source.clone(), true, |_| None).is_err());
    assert!(dri_digest::Context::from_lookup(
        source.clone(),
        Arc::new(CapturingNotifier::new()),
        true,
        |_| None
    )
    .is_err());
    assert!(notation_reset::Context::from_lookup(source, "Development", true, |_| None).is_err());
}

#[test]
fn local_canary_keeps_manual_registration_and_strict_timer_configuration() {
    let providers = local();
    assert_eq!(
        providers.canary(&native).unwrap().recipient,
        "billing-canary@example.test"
    );
    assert!(providers
        .canary(
            &|key| if key == workflows::flags::BILLING_CANARY_SCHEDULE_ENABLED_ENV {
                Some("sometimes".into())
            } else {
                native(key)
            }
        )
        .is_err());
}

async fn seed_invoice(source: &SurrealDb, providers: &LocalProviders) -> String {
    let matter = native_project::matter(source).await;
    let project = store::projects::find_by_id(source, matter.project)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(project.firm_id, Some(matter.firm));
    assert_eq!(project.brand, matter.brand);
    assert_eq!(project.entity_id, matter.entity);
    assert!(store::persons::find_by_id(source, matter.admin)
        .await
        .unwrap()
        .is_some());
    let invoice = format!("synthetic-invoice-{}", uuid::Uuid::new_v4());
    store::xero_invoices::upsert(
        source,
        &store::xero_invoices::UpsertXeroInvoice {
            project_id: matter.project,
            xero_invoice_id: invoice.clone(),
            reference: format!("Matter {}", matter.project),
            status: "AUTHORISED".into(),
            amount_cents: 1200,
            currency: "USD".into(),
            issued_at: Utc::now(),
            due_at: None,
        },
    )
    .await
    .unwrap();
    providers.billing.set_invoice_status(
        &invoice,
        billing::InvoiceStatus {
            status: "PAID".into(),
            amount_paid_cents: 1200,
            amount_cents: Some(1200),
            currency: Some("USD".into()),
            issued_at: Some(Utc::now()),
            due_at: None,
        },
    );
    invoice
}

#[tokio::test]
async fn detached_native_handlers_use_captured_effects_and_the_owned_postgres_store() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let providers = local();
        let source = SurrealDb::uninitialized();
        let invoice = seed_invoice(&source, &providers).await;
        let nag = providers.general_nag(source.clone(), pg.clone());
        assert_eq!(
            nag.counts.read().await.unwrap(),
            (0, 0),
            "foreign-brand counts stay excluded"
        );
        let dri = providers.dri_digest(source.clone(), pg.clone());
        let reconcile = providers
            .reconcile(source.clone(), pg.clone(), &native)
            .unwrap();
        assert!(!reconcile.schedule_enabled());
        let canary = providers.canary(&native).unwrap();
        let runtime = Arc::new(dbos::DBOS::new(dbos_engine::config(
            &database_url(&pg),
            "native-local-providers",
        )));
        let nag = general_nag::register(&runtime, nag).unwrap();
        let dri = dri_digest::register(&runtime, dri).unwrap();
        let canary = billing_canary::register(&runtime, canary).unwrap();
        let reconcile = crate::dbos_reconcile::register(&runtime, reconcile).unwrap();
        let _reset =
            notation_reset::register(&runtime, providers.notation_reset(source, "Development"))
                .unwrap();
        let (slack, channel, enabled) = providers.archives();
        let _archives =
            archives::workflow::register_dbos(&runtime, slack, channel, enabled).unwrap();
        let running = runtime.clone();
        let checked = tokio::spawn(async move {
            running.launch().await.unwrap();
            let slot = Utc.with_ymd_and_hms(2026, 11, 19, 1, 11, 0).unwrap();
            let nag_report = result(general_nag::fire(&nag, slot).await.unwrap().result()).await;
            assert!(!nag_report.posted_followup);
            assert_eq!(
                result(dri_digest::fire(&dri, slot).await.unwrap().result())
                    .await
                    .projects,
                1
            );
            assert!(
                result(billing_canary::fire(&canary, slot).await.unwrap().result())
                    .await
                    .idempotent
            );
            let report = result(
                crate::dbos_reconcile::fire(&reconcile, slot)
                    .await
                    .unwrap()
                    .result(),
            )
            .await;
            assert_eq!((report.checked, report.updated), (1, 1));
            let replay = result(
                crate::dbos_reconcile::fire(&reconcile, slot)
                    .await
                    .unwrap()
                    .result(),
            )
            .await;
            assert_eq!(replay, report);
            let paid: (String, i64) =
                sqlx::query_as("SELECT status,amount_paid_cents FROM xero_invoice WHERE id=$1")
                    .bind(invoice)
                    .fetch_one(pg.pool())
                    .await
                    .unwrap();
            assert_eq!(paid, ("PAID".into(), 1200));
            assert_eq!(providers.billing.contact_calls().len(), 2);
            assert_eq!(providers.email.captured().len(), 1);
            assert_eq!(providers.notifier.captured().len(), 1);
            assert_eq!(providers.slack.posted_messages().len(), 2);
            // The SDK uses its own pool. Closing the injected application pool
            // must refuse the next business operation rather than use another.
            pg.pool().close().await;
            let refused = crate::dbos_reconcile::fire(&reconcile, slot + chrono::Duration::days(1))
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(60), refused.result())
                    .await
                    .unwrap()
                    .is_err()
            );
            assert_eq!(providers.slack.posted_messages().len(), 2);
        })
        .await;
        runtime.shutdown().await;
        if let Err(error) = checked {
            std::panic::resume_unwind(error.into_panic());
        }
    })
    .await;
}

#[tokio::test]
async fn injected_store_refuses_damaged_history_and_closed_pool_without_fallback() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let providers = local();
        let context = providers.general_nag(SurrealDb::uninitialized(), pg.clone());
        sqlx::query("DELETE FROM mirror_backfill_checkpoint")
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(context.counts.read().await.is_err());
        pg.pool().close().await;
        assert!(context.counts.read().await.is_err());
        assert!(providers.slack.posted_messages().is_empty());
    })
    .await;
}

async fn result<T, E: std::fmt::Debug>(
    future: impl std::future::Future<Output = Result<T, E>>,
) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(60), future)
        .await
        .unwrap()
        .unwrap()
}

fn database_url(pg: &PostgresDb) -> String {
    let options = pg.pool().connect_options();
    format!(
        "postgres://postgres:synthetic@127.0.0.1:{}/{}",
        options.get_port(),
        options.get_database().unwrap()
    )
}
