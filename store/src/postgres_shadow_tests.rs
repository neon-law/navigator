use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::*;

#[test]
fn shadow_reads_are_off_until_a_store_is_named() {
    let config = ShadowConfig::parse(None, None).unwrap();
    assert_eq!(config, ShadowConfig::off());
    assert!(ShadowStore::ALL.iter().all(|store| !config.covers(*store)));
    assert_eq!(
        ShadowConfig::parse(Some(" , "), None).unwrap(),
        ShadowConfig::off()
    );
}

#[test]
fn named_stores_and_sample_parse() {
    let config = ShadowConfig::parse(Some("projects, firms"), Some("0.25")).unwrap();
    assert!(config.covers(ShadowStore::Projects));
    assert!(config.covers(ShadowStore::Firms));
    assert!(!config.covers(ShadowStore::Entities));
    assert!((config.sample() - 0.25).abs() < f64::EPSILON);
    let every = ShadowConfig::parse(Some("entities,notations"), Some("1")).unwrap();
    assert!((every.sample() - 1.0).abs() < f64::EPSILON);
    let default = ShadowConfig::parse(Some("projects"), Some(" ")).unwrap();
    assert!((default.sample() - DEFAULT_SAMPLE).abs() < f64::EPSILON);
}

#[test]
fn malformed_settings_are_refused_by_name() {
    assert_eq!(
        ShadowConfig::parse(Some("projects,persons"), None),
        Err(ShadowConfigError::UnknownStore("persons".into()))
    );
    for sample in ["0", "-0.5", "1.5", "NaN", "inf", "often"] {
        assert_eq!(
            ShadowConfig::parse(Some("projects"), Some(sample)),
            Err(ShadowConfigError::Sample),
            "{sample}"
        );
    }
    assert!(ShadowConfigError::Sample.to_string().contains(SAMPLE_ENV));
}

#[test]
fn a_read_is_shadowed_only_inside_the_sample_of_a_named_store() {
    let config = ShadowConfig::parse(Some("projects"), Some("0.25")).unwrap();
    assert!(config.sampled(ShadowStore::Projects, 0.0));
    assert!(config.sampled(ShadowStore::Projects, 0.249));
    assert!(!config.sampled(ShadowStore::Projects, 0.25));
    assert!(!config.sampled(ShadowStore::Firms, 0.0));
}

/// Collects formatted log lines for one test.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn a_mismatch_is_logged_and_counted_by_table_and_id_only() {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let id = Uuid::now_v7();

    // The comparison knows the row's contents; the verdict must not repeat them.
    let secret = "Synthetic Privileged Matter Name";
    let verdict = judge("shadow_test_mismatch", None, SHADOW_TIMEOUT, async move {
        let surreal = Some(secret.to_string());
        let postgres = Some(format!("{secret} (stale)"));
        Ok(Comparison {
            id: Some(id),
            equal: surreal == postgres,
        })
    })
    .await;
    assert_eq!(verdict, Verdict::Mismatch);

    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("shadow_test_mismatch"), "{log}");
    assert!(log.contains(&id.to_string()), "{log}");
    assert!(log.contains("mismatch"), "{log}");
    assert!(log.contains(LOG_TARGET), "{log}");
    assert!(!log.contains("Synthetic Privileged"), "{log}");
    assert_eq!(
        counts().get("shadow_test_mismatch"),
        Some(&ShadowCounts {
            matched: 0,
            mismatched: 1,
            unavailable: 0
        })
    );
}

#[tokio::test]
async fn a_match_counts_and_a_postgres_error_or_timeout_is_unavailable() {
    let id = Some(Uuid::now_v7());
    let matched = judge("shadow_test_verdicts", id, SHADOW_TIMEOUT, async {
        Ok(Comparison { id, equal: true })
    })
    .await;
    assert_eq!(matched, Verdict::Match);
    let refused = judge("shadow_test_verdicts", id, SHADOW_TIMEOUT, async {
        Err(MirrorError::Unavailable)
    })
    .await;
    assert_eq!(refused, Verdict::Unavailable);
    let slow = judge(
        "shadow_test_verdicts",
        id,
        Duration::from_millis(5),
        async { std::future::pending::<Result<Comparison, MirrorError>>().await },
    )
    .await;
    assert_eq!(slow, Verdict::Unavailable);
    assert_eq!(
        counts().get("shadow_test_verdicts"),
        Some(&ShadowCounts {
            matched: 1,
            mismatched: 0,
            unavailable: 2
        })
    );
}

#[tokio::test]
async fn an_unshadowed_read_never_reaches_postgres() {
    let called = Arc::new(AtomicBool::new(false));
    let probe = |called: Arc<AtomicBool>| {
        move |_pg: PostgresDb| async move {
            called.store(true, Ordering::SeqCst);
            Ok(Comparison {
                id: None,
                equal: true,
            })
        }
    };
    let off = ShadowConfig::off();
    assert!(observe_with(
        &off,
        0.0,
        ShadowStore::Projects,
        "shadow_test_off",
        None,
        probe(called.clone())
    )
    .is_none());
    let firms = ShadowConfig::parse(Some("firms"), Some("0.5")).unwrap();
    assert!(observe_with(
        &firms,
        0.0,
        ShadowStore::Projects,
        "shadow_test_off",
        None,
        probe(called.clone())
    )
    .is_none());
    assert!(observe_with(
        &firms,
        0.75,
        ShadowStore::Firms,
        "shadow_test_off",
        None,
        probe(called.clone())
    )
    .is_none());
    tokio::task::yield_now().await;
    assert!(!called.load(Ordering::SeqCst));
    assert!(!counts().contains_key("shadow_test_off"));
}

#[tokio::test]
async fn a_shadowed_read_runs_detached_and_settles_without_failing_the_caller() {
    let config = ShadowConfig::parse(Some("firms"), Some("1")).unwrap();
    let handle = observe_with(
        &config,
        0.5,
        ShadowStore::Firms,
        "shadow_test_detached",
        Some(Uuid::nil()),
        |_pg: PostgresDb| async {
            Ok(Comparison {
                id: None,
                equal: true,
            })
        },
    )
    .expect("a sampled read of a named store is shadowed");
    // With or without a configured PostgreSQL, the task settles on a verdict.
    let verdict = handle.await.unwrap();
    assert!(matches!(verdict, Verdict::Match | Verdict::Unavailable));
    assert_eq!(
        counts()
            .get("shadow_test_detached")
            .map(|tally| tally.matched + tally.unavailable),
        Some(1)
    );
}
