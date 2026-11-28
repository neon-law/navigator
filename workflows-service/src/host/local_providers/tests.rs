use super::*;
use chrono::{TimeZone, Utc};
use store::postgres::PostgresDb;
use workflows::dbos_engine::{self, billing_canary, dri_digest, general_nag, notation_reset};

#[path = "../../../tests/support/native_project.rs"]
mod native_project;

fn native(key: &str) -> Option<String> {
    match key {
        "NAVIGATOR_CI_HARNESS" => Some("1".into()),
        "NAVIGATOR_STAGING"
        | workflows::flags::BILLING_CANARY_SCHEDULE_ENABLED_ENV
        | workflows::flags::RECONCILE_INVOICES_SCHEDULE_ENABLED_ENV => Some("false".into()),
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
    let source = ApplicationStore::uninitialized();
    let bot = || Arc::new(CapturingSlackBot::new()) as Arc<dyn workflows::SlackBot>;
    assert!(general_nag::Context::from_lookup(source.clone(), bot(), true, |_| None).is_err());
    assert!(dri_digest::Context::from_lookup(source.clone(), bot(), true, |_| None).is_err());
    assert!(
        notation_reset::Context::from_lookup(source, bot(), "Development", true, |_| None).is_err()
    );
}

async fn seed_matter(source: &ApplicationStore) -> String {
    let (matter, admin) = native_project::matter(source).await;
    let firm = matter
        .firm_id
        .expect("the native Project fixture has a Firm");
    let project = store::projects::find_by_id(source, matter.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(project.firm_id, Some(firm));
    assert_eq!(project.brand, matter.brand);
    assert_eq!(project.entity_id, matter.entity_id);
    assert!(store::persons::find_by_id(source, admin)
        .await
        .unwrap()
        .is_some());
    format!("Matter {}", matter.id)
}

#[tokio::test]
async fn detached_native_handlers_use_captured_effects_and_the_owned_postgres_store() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let providers = local();
        let source = ApplicationStore::uninitialized();
        let _matter_reference = seed_matter(&source).await;
        let nag = providers.general_nag(source.clone(), pg.clone());
        assert_eq!(
            nag.counts
                .read()
                .await
                .unwrap()
                .iter()
                .map(|line| line.open)
                .sum::<usize>(),
            0,
            "another Firm's matters stay excluded"
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
            let replay = result(
                crate::dbos_reconcile::fire(&reconcile, slot)
                    .await
                    .unwrap()
                    .result(),
            )
            .await;
            assert_eq!(replay, report);
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
        let context = providers.general_nag(ApplicationStore::uninitialized(), pg.clone());
        let damaged_schema = sqlx::query("UPDATE public._sqlx_migrations SET success=false WHERE version=(SELECT max(version) FROM public._sqlx_migrations)")
            .execute(pg.pool())
            .await
            .unwrap();
        assert_eq!(damaged_schema.rows_affected(), 1, "the owned native schema attestation must actually be damaged");
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

#[tokio::test]
async fn native_billing_digest_without_exports_registers_without_provider_effects() {
    use workflows::dbos_engine::billing_digest;

    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let context =
            billing_digest::Context::from_lookup(Arc::new(CapturingSlackBot::new()), native)
                .unwrap();
        let runtime = Arc::new(dbos::DBOS::new(dbos_engine::config(
            &database_url(&pg),
            "native-billing-digest",
        )));
        let workflow = billing_digest::register(&runtime, context).unwrap();
        let running = runtime.clone();
        let checked = tokio::spawn(async move {
            running.launch().await.unwrap();
            let slot = Utc.with_ymd_and_hms(2026, 11, 19, 13, 0, 0).unwrap();
            let first = result(
                billing_digest::fire(&workflow, slot)
                    .await
                    .unwrap()
                    .result(),
            )
            .await;
            assert!(!first.sent);
            assert_eq!(
                (first.cost_total, first.services, first.projects),
                (0.0, 0, 0)
            );
            let replay = result(
                billing_digest::fire(&workflow, slot)
                    .await
                    .unwrap()
                    .result(),
            )
            .await;
            assert_eq!(replay, first);
            let steps = running
                .list_workflow_steps(&billing_digest::workflow_id(slot))
                .await
                .unwrap();
            assert!(steps
                .iter()
                .all(|step| step.step_name != "query" && step.step_name != "email"));
        })
        .await;
        runtime.shutdown().await;
        if let Err(error) = checked {
            std::panic::resume_unwind(error.into_panic());
        }
    })
    .await;
}
