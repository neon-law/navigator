//! Actual SDK checkpoints; the captured counter is outside the restored database.
use dbos::{WorkflowRef, DBOS};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use store::postgres::PostgresDb;
use tokio::sync::Notify;
use workflows::dbos_engine;

pub(super) const VERSION: &str = "synthetic-physical-recovery-v1";
pub(super) const EXECUTOR: &str = "synthetic-physical-recovery-owner";

#[derive(Clone)]
pub(super) struct Probe {
    pub pg: PostgresDb,
    pub pause: bool,
    pub posts: Arc<AtomicU32>,
    pub attempts: Arc<AtomicU32>,
    pub reached: Arc<Notify>,
}

impl Probe {
    pub fn new(pg: PostgresDb) -> Self {
        Self {
            pg,
            pause: true,
            posts: Arc::new(AtomicU32::new(0)),
            attempts: Arc::new(AtomicU32::new(0)),
            reached: Arc::new(Notify::new()),
        }
    }

    async fn write(self, hold: bool) -> dbos::Result<()> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        let key = dbos_engine::effect_key().expect("real SDK step effect key");
        sqlx::query(
            "INSERT INTO synthetic_physical_effect(effect_key) VALUES($1) ON CONFLICT DO NOTHING",
        )
        .bind(key)
        .execute(self.pg.pool())
        .await
        .expect("keyed effect");
        if hold && self.pause {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

pub(super) fn register(worker: &DBOS, probe: Probe) -> WorkflowRef<bool, (), dbos::EngineOnly> {
    worker.register_workflow("SyntheticPhysicalRecovery", move |hold: bool| {
        let probe = probe.clone();
        async move {
            dbos::step("provider", || {
                let probe = probe.clone();
                async move {
                    probe.posts.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, dbos::Error>(())
                }
            }).await?;
            dbos::step("keyed-effect", || probe.clone().write(hold)).await?;
            dbos::step("receipt", || {
                let pg = probe.pg.clone();
                async move {
                    let id = dbos::workflow_id().expect("workflow identity");
                    sqlx::query("INSERT INTO synthetic_physical_receipt(workflow_id) VALUES($1) ON CONFLICT DO NOTHING")
                        .bind(id).execute(pg.pool()).await.expect("PG-only receipt");
                    Ok::<_, dbos::Error>(())
                }
            }).await?;
            Ok::<_, dbos::Error>(())
        }
    }).expect("register synthetic physical acceptance workflow")
}
