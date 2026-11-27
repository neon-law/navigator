use crate::postgres::PostgresDb;

/// The three fields owned together by the lifecycle ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LifecycleProjection {
    pub status: String,
    pub closed_at: Option<String>,
    pub closure_reason: Option<String>,
}

struct ProjectionEvent {
    from_status: Option<String>,
    state: LifecycleProjection,
    effective_at: Option<DateTime<Utc>>,
    recorded_at: Option<DateTime<Utc>>,
    reason: Option<String>,
    attesting_person_id: Option<Uuid>,
}

impl ProjectionEvent {
    fn read(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            from_status: row.try_get("from_status")?,
            state: LifecycleProjection {
                status: row.try_get("to_status")?,
                closed_at: row.try_get("closed_at")?,
                closure_reason: row.try_get("closure_reason")?,
            },
            effective_at: row.try_get("effective_at")?,
            recorded_at: row.try_get("recorded_at")?,
            reason: row.try_get("reason")?,
            attesting_person_id: row.try_get("attesting_person_id")?,
        })
    }
}

fn coherent_states(events: &[ProjectionEvent]) -> Option<Vec<LifecycleProjection>> {
    let mut states: Vec<LifecycleProjection> = Vec::with_capacity(events.len());
    for event in events {
        let state = &event.state;
        if !matches!(state.status.as_str(), "open" | "closed" | "archived")
            || (state.status == "open"
                && (state.closed_at.is_some() || state.closure_reason.is_some()))
        {
            return None;
        }
        if let Some(previous) = states.last() {
            if event.from_status.as_deref() != Some(previous.status.as_str())
                || event.effective_at.is_none()
                || event.recorded_at.is_none()
                || (previous.status == "archived" && state.status != "archived")
            {
                return None;
            }
            match state.status.as_str() {
                "open"
                    if previous.status == "open"
                        || event.reason.as_deref() != Some("new_matter_event")
                        || event.attesting_person_id.is_none() =>
                {
                    return None
                }
                "closed" | "archived"
                    if state
                        .closed_at
                        .as_deref()
                        .is_none_or(|value| DateTime::parse_from_rfc3339(value).is_err()) =>
                {
                    return None
                }
                "closed"
                    if event.reason != state.closure_reason
                        || (previous.status == "open" && state.closure_reason.is_none()) =>
                {
                    return None
                }
                "archived"
                    if event.reason.is_some()
                        || state.closure_reason != previous.closure_reason =>
                {
                    return None
                }
                _ => {}
            }
        } else if event.from_status.is_some() {
            return None;
        }
        states.push(state.clone());
    }
    Some(states)
}

/// Check the entire ordered ledger and its committed projection, without
/// inferring dates or changing historical baseline evidence.
pub(crate) async fn verified_projection_history(
    pg: &PostgresDb,
    project: Uuid,
    projection: &LifecycleProjection,
) -> Result<Option<Vec<LifecycleProjection>>, sqlx::Error> {
    let events = sqlx::query("SELECT from_status,to_status,closed_at,closure_reason,effective_at,recorded_at,reason,attesting_person_id FROM project_status_transition WHERE project_id=$1 ORDER BY sequence")
        .bind(project).fetch_all(pg.pool()).await?
        .iter().map(ProjectionEvent::read).collect::<Result<Vec<_>, _>>()?;
    Ok(coherent_states(&events)
        .filter(|states| states.last().is_none_or(|last| last == projection)))
}

use super::*;

async fn fixture() -> (PostgresDb, Uuid, Uuid) {
    let (options, database) = crate::postgres::fixtures::fresh_schema()
        .await
        .expect("required PostgreSQL");
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
    crate::postgres::fixtures::activate_application(&pg).await;
    let (person, notation, _) = crate::postgres::fixtures::workflow_fixture(&pg).await;
    let project = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(notation)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE project SET inserted_at='2026-01-01' WHERE id=$1")
        .bind(project)
        .execute(pg.pool())
        .await
        .unwrap();
    (pg, project, person)
}

async fn event(
    pg: &PostgresDb,
    project: Uuid,
    from: Option<&str>,
    to: &str,
    closed: Option<&str>,
    reason: Option<&str>,
    actor: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let closure = (to == "closed").then_some("matter_concluded");
    let effective = if to == "closed" {
        "2026-11-02T00:00:00Z"
    } else {
        "2026-11-01T00:00:00Z"
    };
    sqlx::query("INSERT INTO project_status_transition(project_id,from_status,to_status,effective_at,recorded_at,reason,attesting_person_id,closed_at,closure_reason) VALUES($1,$2,$3,$8::timestamptz,'2026-11-04',$4,$5,$6,$7)")
        .bind(project).bind(from).bind(to).bind(reason).bind(actor).bind(closed).bind(closure).bind(effective)
        .execute(pg.pool()).await?;
    Ok(())
}

fn state(status: &str, closed_at: Option<&str>) -> LifecycleProjection {
    LifecycleProjection {
        status: status.into(),
        closed_at: closed_at.map(str::to_owned),
        closure_reason: (status == "closed").then(|| "matter_concluded".into()),
    }
}

#[tokio::test]
async fn lifecycle_requires_a_baseline_and_database_refuses_broken_chain() {
    let (pg, project, _) = fixture().await;
    let closed = "2026-11-02T12:00:00Z";
    event(
        &pg,
        project,
        Some("open"),
        "closed",
        Some(closed),
        Some("matter_concluded"),
        None,
    )
    .await
    .unwrap();
    assert!(
        verified_projection_history(&pg, project, &state("closed", Some(closed)))
            .await
            .unwrap()
            .is_none()
    );
    let (pg, project, _) = fixture().await;
    event(&pg, project, None, "open", None, None, None)
        .await
        .unwrap();
    assert!(event(
        &pg,
        project,
        Some("closed"),
        "closed",
        Some(closed),
        Some("matter_concluded"),
        None,
    )
    .await
    .is_err());
    assert_eq!(
        verified_projection_history(&pg, project, &state("open", None))
            .await
            .unwrap()
            .unwrap(),
        vec![state("open", None)]
    );
}

#[tokio::test]
async fn lifecycle_preserves_backdated_attestation_and_checks_terminal_projection() {
    let (pg, project, actor) = fixture().await;
    let closed = "2026-11-02T12:00:00Z";
    event(&pg, project, None, "open", None, None, None)
        .await
        .unwrap();
    event(
        &pg,
        project,
        Some("open"),
        "closed",
        Some(closed),
        Some("matter_concluded"),
        None,
    )
    .await
    .unwrap();
    event(
        &pg,
        project,
        Some("closed"),
        "open",
        None,
        Some("new_matter_event"),
        Some(actor),
    )
    .await
    .unwrap();
    let history = verified_projection_history(&pg, project, &state("open", None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        history,
        vec![
            state("open", None),
            state("closed", Some(closed)),
            state("open", None)
        ]
    );
    assert!(
        verified_projection_history(&pg, project, &state("closed", Some(closed)))
            .await
            .unwrap()
            .is_none()
    );
}

async fn historical_state(pg: &PostgresDb, project: Uuid, status: &str, closed: &str) {
    let projection = state(status, Some(closed));
    sqlx::query("UPDATE project SET status=$2,closed_at=$3,closure_reason=$4 WHERE id=$1")
        .bind(project)
        .bind(status)
        .bind(closed)
        .bind(projection.closure_reason)
        .execute(pg.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn lifecycle_refuses_unattested_reopen_and_database_keeps_archiving_terminal() {
    let (pg, project, _) = fixture().await;
    historical_state(&pg, project, "closed", "2026-11-02T12:00:00Z").await;
    event(
        &pg,
        project,
        None,
        "closed",
        Some("2026-11-02T12:00:00Z"),
        Some("matter_concluded"),
        None,
    )
    .await
    .unwrap();
    event(
        &pg,
        project,
        Some("closed"),
        "open",
        None,
        Some("new_matter_event"),
        None,
    )
    .await
    .unwrap();
    assert!(
        verified_projection_history(&pg, project, &state("open", None))
            .await
            .unwrap()
            .is_none()
    );
    let (pg, project, _) = fixture().await;
    historical_state(&pg, project, "archived", "2026-11-02T12:00:00Z").await;
    event(
        &pg,
        project,
        None,
        "archived",
        Some("2026-11-02T12:00:00Z"),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(event(
        &pg,
        project,
        Some("archived"),
        "closed",
        Some("2026-11-03T12:00:00Z"),
        Some("matter_concluded"),
        None,
    )
    .await
    .is_err());
}

#[test]
fn lifecycle_decoder_rejects_discontinuous_untrusted_history() {
    let row = |from: Option<&str>, status| ProjectionEvent {
        from_status: from.map(str::to_owned),
        state: state(status, (status != "open").then_some("2026-11-02T12:00:00Z")),
        effective_at: Some(Utc::now()),
        recorded_at: Some(Utc::now()),
        reason: (status == "closed").then(|| "matter_concluded".into()),
        attesting_person_id: None,
    };
    assert!(coherent_states(&[row(None, "open"), row(Some("open"), "closed")]).is_some());
    assert!(coherent_states(&[row(None, "open"), row(Some("closed"), "archived")]).is_none());
    assert!(coherent_states(&[row(None, "archived"), row(Some("archived"), "closed")]).is_none());
}
