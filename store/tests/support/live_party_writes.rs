//! Native live-party references serialize with Person merge.
use serde_json::Value;
use std::time::Duration;
use store::{application_store::ApplicationStore, postgres::PostgresDb, project_references::Party};
use tokio::task::JoinSet;
use uuid::Uuid;

#[derive(Clone, Copy)]
enum Kind {
    Holding,
    Fiduciary,
}
impl Kind {
    fn table(self) -> &'static str {
        match self {
            Self::Holding => "cap_table_holding",
            Self::Fiduciary => "fiduciary_role",
        }
    }
    fn column(self) -> &'static str {
        match self {
            Self::Holding => "holder_id",
            Self::Fiduciary => "fiduciary_id",
        }
    }
}

async fn write(kind: Kind, project: Uuid, person: Uuid) -> Result<Value, (bool, String)> {
    let db = ApplicationStore::uninitialized();
    match kind {
        Kind::Holding => store::cap_table_holdings::insert(
            &db,
            project,
            &store::cap_table_holdings::CapTableHoldingFields {
                holder: Party::person(person),
                share_class: "synthetic concurrent preferred".into(),
                shares: 37,
                vesting_note: Some("Synthetic retained vesting".into()),
            },
        )
        .await
        .map(|row| serde_json::to_value(row).unwrap())
        .map_err(|error| {
            (
                matches!(
                    &error,
                    store::cap_table_holdings::CapTableHoldingError::Invalid(_)
                ),
                error.to_string(),
            )
        }),
        Kind::Fiduciary => store::fiduciary_roles::insert(
            &db,
            project,
            &store::fiduciary_roles::FiduciaryRoleFields {
                role: "trustee".into(),
                fiduciary: Party::person(person),
                successor_order: 1,
                notation_id: None,
            },
        )
        .await
        .map(|row| serde_json::to_value(row).unwrap())
        .map_err(|error| {
            (
                matches!(
                    &error,
                    store::fiduciary_roles::FiduciaryRoleError::Invalid(_)
                ),
                error.to_string(),
            )
        }),
    }
}

async fn observed(pg: &PostgresDb, sql: &str) -> bool {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(pg.pool())
        .await
        .unwrap()
}
async fn wait_for_writer(pg: &PostgresDb) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !observed(pg, "SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid WHERE a.datname=current_database() AND l.locktype='advisory' AND l.classid=153001 AND l.objid=1 AND NOT l.granted)").await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("native insert never reached its owned BEFORE INSERT gate");
}
enum Event {
    Written(Value),
    Merged,
}
async fn wait_for_merge(pg: &PostgresDb, jobs: &mut JoinSet<Event>) -> bool {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(result) = jobs.try_join_next() {
                assert!(matches!(result.unwrap(), Event::Merged));
                return true;
            }
            if observed(pg, "SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid WHERE a.datname=current_database() AND l.locktype='transactionid' AND NOT l.granted AND a.query LIKE '%FROM person WHERE id=ANY%FOR UPDATE')").await { return false; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("merge neither completed nor waited on the native party row")
}

async fn assert_stored(pg: &PostgresDb, kind: Kind, written: &Value, kept: Uuid) {
    let sql = format!("SELECT to_jsonb(r) FROM {} r WHERE id=$1", kind.table());
    let row: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(Uuid::parse_str(written["id"].as_str().unwrap()).unwrap())
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(row[kind.column()]["tb"], "person");
    assert_eq!(row[kind.column()]["id"]["Uuid"], kept.to_string());
    match kind {
        Kind::Holding => {
            assert_eq!(row["shares"], 37);
            assert_eq!(row["vesting_note"], "Synthetic retained vesting");
            assert_eq!(row["share_class"], "synthetic concurrent preferred");
        }
        Kind::Fiduciary => {
            assert_eq!(row["role"], "trustee");
            assert_eq!(row["successor_order"], 1);
            assert!(row["notation_id"].is_null());
        }
    }
}

async fn race(pg: PostgresDb, kind: Kind, project: Uuid, kept: Uuid, dropped: Uuid) {
    let mut gate = pg.pool().acquire().await.unwrap();
    // Closing this session also releases its lock on panic/cancellation.
    gate.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock(153001,1)")
        .execute(&mut *gate)
        .await
        .unwrap();
    let mut jobs = JoinSet::new();
    let writer_pg = pg.clone();
    jobs.spawn(async move {
        PostgresDb::with_application_db(writer_pg, async move {
            Event::Written(write(kind, project, dropped).await.unwrap())
        })
        .await
        .unwrap()
    });
    wait_for_writer(&pg).await;
    let merge_pg = pg.clone();
    jobs.spawn(async move {
        PostgresDb::with_application_db(merge_pg, async move {
            store::person_merge::merge(&ApplicationStore::uninitialized(), kept, dropped)
                .await
                .unwrap();
            Event::Merged
        })
        .await
        .unwrap()
    });
    // The writer holds KEY SHARE until its row commits, so merge must wait.
    let merge_finished = wait_for_merge(&pg, &mut jobs).await;
    sqlx::query("SELECT pg_advisory_unlock(153001,1)")
        .execute(&mut *gate)
        .await
        .unwrap();
    let mut written = None;
    let mut merged = merge_finished;
    while let Some(result) = jobs.join_next().await {
        match result.unwrap() {
            Event::Written(row) => written = Some(row),
            Event::Merged => merged = true,
        }
    }
    assert!(merged);
    assert_stored(&pg, kind, &written.unwrap(), kept).await;
    assert!(
        !merge_finished,
        "merge overtook a validated native party write"
    );
}

async fn concurrent(pg: &PostgresDb, kind: Kind, project: Uuid, kept: Uuid, dropped: Uuid) {
    let ddl = format!("CREATE FUNCTION test_live_party_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(153001,1); RETURN NEW; END $$; CREATE TRIGGER test_live_party_gate BEFORE INSERT ON {} FOR EACH ROW EXECUTE FUNCTION test_live_party_gate()", kind.table());
    // The table is one of the two fixed names in Kind, never caller input.
    sqlx::raw_sql(sqlx::AssertSqlSafe(ddl))
        .execute(pg.pool())
        .await
        .unwrap();
    // JoinSet aborts the owned driver if this fixture future is cancelled too.
    let mut driver = JoinSet::new();
    driver.spawn(race(pg.clone(), kind, project, kept, dropped));
    let outcome = tokio::time::timeout(Duration::from_secs(30), driver.join_next()).await;
    let outcome = if let Ok(result) = outcome {
        result
    } else {
        driver.abort_all();
        while driver.join_next().await.is_some() {}
        None
    };
    // Propagate assertions/panics only after removing this owned barrier.
    let cleanup = format!("DROP TRIGGER IF EXISTS test_live_party_gate ON {}; DROP FUNCTION IF EXISTS test_live_party_gate()", kind.table());
    tokio::time::timeout(
        Duration::from_secs(20),
        sqlx::raw_sql(sqlx::AssertSqlSafe(cleanup)).execute(pg.pool()),
    )
    .await
    .expect("owned party gate cleanup timed out")
    .unwrap();
    outcome
        .expect("owned writer/merge driver timed out")
        .unwrap();
}

async fn merge_first(pg: &PostgresDb, kind: Kind, project: Uuid, kept: Uuid, dropped: Uuid) {
    store::person_merge::merge(&ApplicationStore::uninitialized(), kept, dropped)
        .await
        .unwrap();
    let (invalid, message) = write(kind, project, dropped).await.unwrap_err();
    assert!(invalid, "{message}");
    assert_eq!(
        message,
        match kind {
            Kind::Holding => "the holder names no such Person",
            Kind::Fiduciary => "the fiduciary names no such Person",
        }
    );
    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {} WHERE project_id=$1",
        kind.table()
    )))
    .bind(project)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
}

async fn run(kind: Kind, raced: bool) {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = ApplicationStore::uninitialized();
        let parent = super::native_project::create("synthetic-live-party-boundary").await;
        let mut ids = Vec::new();
        for name in ["Synthetic retained party", "Synthetic merged party"] {
            ids.push(
                store::persons::create(
                    &db,
                    &store::persons::NewPerson::new(
                        name,
                        format!("party-{}@example.test", Uuid::new_v4()),
                    ),
                )
                .await
                .unwrap()
                .id,
            );
        }
        let (kept, dropped) = (ids[0], ids[1]);
        if raced {
            concurrent(&pg, kind, parent.id, kept, dropped).await;
        } else {
            merge_first(&pg, kind, parent.id, kept, dropped).await;
        }
        assert!(store::persons::find_by_id(&db, dropped)
            .await
            .unwrap()
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn holding_write_serializes_with_merge_and_preserves_the_committed_line() {
    run(Kind::Holding, true).await;
}
#[tokio::test]
async fn fiduciary_write_serializes_with_merge_and_preserves_the_committed_role() {
    run(Kind::Fiduciary, true).await;
}
#[tokio::test]
async fn completed_merge_refuses_a_new_holding_for_the_missing_person() {
    run(Kind::Holding, false).await;
}
#[tokio::test]
async fn completed_merge_refuses_a_new_fiduciary_for_the_missing_person() {
    run(Kind::Fiduciary, false).await;
}

#[tokio::test]
async fn duplicate_lines_and_slots_remain_invalid_without_changing_existing_rows() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = ApplicationStore::uninitialized();
        let project = super::native_project::create("synthetic-party-duplicate-controls").await;
        let person = store::persons::create(
            &db,
            &store::persons::NewPerson::new(
                "Synthetic duplicate control",
                "party-control@example.test",
            ),
        )
        .await
        .unwrap();
        for kind in [Kind::Holding, Kind::Fiduciary] {
            write(kind, project.id, person.id).await.unwrap();
            let before = super::merge_fixture::snapshot(&pg).await;
            let (invalid, error) = write(kind, project.id, person.id).await.unwrap_err();
            assert!(
                invalid,
                "duplicate party writes must remain Invalid: {error}"
            );
            assert_eq!(super::merge_fixture::snapshot(&pg).await, before);
        }
    })
    .await;
}
