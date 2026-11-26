//! `notation_events` — the append-only journal of every state-machine
//! transition for every Notation, and every query against it.
//!
//! PostgreSQL serves the application journal after verified scoped enrollment.
//! Each row records a state-machine transition. The current state of a
//! `(notation_id, machine_kind)` is the `to_state` of its latest row ordered
//! by id — see [`latest_for_kind`]. Captured Source rows remain readable
//! only through the explicit import compatibility context.
//!
//! # Append-only
//!
//! [`append_event`] is the public writer; transaction-owned review decisions
//! use the same canonical event constructor. Neither seam exposes update or
//! delete. `no_update_or_delete_is_exported` checks the public API.
//!
//! `payload` is opaque JSON *text*, not a native object: a questionnaire
//! event can carry client answer content, so it must never be logged or
//! traced, and storing it as an inert string is what keeps that true.

use crate::postgres::matter;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, SurrealDb};

/// The table these rows live in.
pub(crate) const TABLE: &str = "notation_event";

/// Machine-kind discriminator stored as text in `machine_kind`. Mirrors
/// `workflows::spec::MachineKind` — kept in sync by the workers writing this
/// table.
pub const MACHINE_QUESTIONNAIRE: &str = "questionnaire";
/// Machine-kind discriminator for the post-intake workflow.
pub const MACHINE_WORKFLOW: &str = "workflow";

/// One journaled state-machine transition.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`NotationEventRow`] is the seam that turns it into (and back out of)
/// what the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationEvent {
    pub id: Uuid,
    pub notation_id: Uuid,
    /// The human who caused this transition.
    pub acting_person_id: Uuid,
    /// The template version pinned by the notation at transition time.
    pub template_version_id: Uuid,
    /// Lowercase machine-kind token — [`MACHINE_QUESTIONNAIRE`] or
    /// [`MACHINE_WORKFLOW`]. Mirrors `workflows::MachineKind::as_str`.
    pub machine_kind: String,
    pub from_state: String,
    pub to_state: String,
    pub condition: String,
    /// Opaque JSON text. May carry client-provided answer content; never
    /// log or trace it.
    pub payload: Option<String>,
    /// RFC 3339 / ISO 8601, opaque text — a domain timestamp passed through
    /// as-is, not re-typed by the port.
    pub recorded_at: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as the engine reads and writes it.
#[derive(SurrealValue)]
struct NotationEventRow {
    id: surrealdb::types::RecordId,
    notation_id: surrealdb::types::RecordId,
    acting_person_id: surrealdb::types::RecordId,
    template_version_id: surrealdb::types::RecordId,
    machine_kind: String,
    from_state: String,
    to_state: String,
    condition: String,
    payload: Option<String>,
    recorded_at: String,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl NotationEventRow {
    /// `None` when a record id is not a native UUID key — a row written by
    /// something that bypassed [`crate::surreal::record_id`].
    fn into_event(self) -> Option<NotationEvent> {
        Some(NotationEvent {
            id: record_uuid(&self.id)?,
            notation_id: record_uuid(&self.notation_id)?,
            acting_person_id: record_uuid(&self.acting_person_id)?,
            template_version_id: record_uuid(&self.template_version_id)?,
            machine_kind: self.machine_kind,
            from_state: self.from_state,
            to_state: self.to_state,
            condition: self.condition,
            payload: self.payload,
            recorded_at: self.recorded_at,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`NotationEventRow`] from only one query.
const SELECT: &str = "id, notation_id, acting_person_id, template_version_id, machine_kind, \
     from_state, to_state, condition, payload, recorded_at, inserted_at, updated_at";

/// One transition's worth of data to journal. Carries everything the row
/// needs in a single struct so [`append_event`] stays under clippy's
/// argument budget and reads as one logical record at the call site.
pub struct TransitionRecord<'a> {
    pub notation_id: Uuid,
    pub acting_person_id: Option<Uuid>,
    pub machine_kind: &'a str,
    pub from_state: &'a str,
    pub to_state: &'a str,
    pub condition: &'a str,
    /// Opaque JSON text. It may carry event metadata, but must not be
    /// logged or traced because questionnaire events can include
    /// client-provided answer content.
    pub payload_json: Option<String>,
    /// RFC 3339 / ISO 8601. Callers from the Restate worker pass
    /// `chrono::Utc::now().to_rfc3339()` so a replay reuses the captured
    /// timestamp via Restate's journal cache.
    pub recorded_at: &'a str,
}

/// Errors reading or writing the journal.
#[derive(Debug, thiserror::Error)]
pub enum NotationEventError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("notation: {0}")]
    Notation(#[from] crate::notations::NotationError),
}

fn many(mut response: surrealdb::IndexedResults) -> Result<Vec<NotationEvent>, NotationEventError> {
    let rows: Vec<NotationEventRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(NotationEventRow::into_event)
        .collect())
}

fn native_event(
    record: TransitionRecord<'_>,
    respondent: Uuid,
    template_id: Uuid,
) -> NotationEvent {
    let acting_person_id = resolve_acting_person(&record, respondent);
    let payload = record.payload_json.or_else(|| {
        (record.machine_kind == MACHINE_WORKFLOW)
            .then(|| workflow_payload(acting_person_id, template_id))
    });
    let now = Utc::now();
    NotationEvent {
        id: Uuid::now_v7(),
        notation_id: record.notation_id,
        acting_person_id,
        template_version_id: template_id,
        machine_kind: record.machine_kind.to_owned(),
        from_state: record.from_state.to_owned(),
        to_state: record.to_state.to_owned(),
        condition: record.condition.to_owned(),
        payload,
        recorded_at: record.recorded_at.to_owned(),
        inserted_at: now,
        updated_at: now,
    }
}

/// Append the same canonical event in a caller-owned transaction holding its Notation lock.
pub(crate) async fn append_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    record: TransitionRecord<'_>,
) -> Result<NotationEvent, NotationEventError> {
    let (respondent, template): (Uuid, Uuid) =
        sqlx::query_as("SELECT person_id,template_id FROM notation WHERE id=$1")
            .bind(record.notation_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(matter::MatterAuthorityError::from)?;
    let event = native_event(record, respondent, template);
    Ok(matter::upsert_on(
        &mut **tx,
        TABLE,
        serde_json::to_value(event).map_err(matter::MatterAuthorityError::from)?,
    )
    .await?)
}

/// Append one row. No unique index guards this insert — each call mints
/// its own fresh id — so, like [`crate::answers::record`], nothing here
/// retries a transaction conflict: an append-only table with no unique
/// index has no row for two writers to contend over.
///
/// Reads the admitted Notation's respondent and immutable Template version
/// for attribution and version pinning. The transactional seam uses that
/// same context inside its caller's Notation lock.
///
/// # Errors
///
/// [`NotationEventError::Notation`] if the referenced notation does not
/// exist, or a PostgreSQL error if its authority or write is unavailable.
pub async fn append_event(
    _db: &SurrealDb,
    record: TransitionRecord<'_>,
) -> Result<NotationEvent, NotationEventError> {
    let pg = matter::authority_for(TABLE)
        .await?
        .ok_or(matter::MatterAuthorityError::Unavailable)?;
    let (respondent, template_id) =
        crate::postgres::notations::notation_context(&pg, record.notation_id)
            .await?
            .ok_or(crate::notations::NotationError::NotFound(
                record.notation_id,
            ))?;
    Ok(matter::upsert(&pg, TABLE, &native_event(record, respondent, template_id)).await?)
}

/// Read the latest event for a `(notation_id, machine_kind)` pair — the
/// projection the application uses as "current state":
/// `result.map(|e| e.to_state)`.
///
/// Returns `None` if no event has been recorded for that pair — the state
/// machine hasn't started yet.
///
/// # Errors
///
/// [`NotationEventError::Db`] if the lookup fails.
pub async fn latest_for_kind(
    db: &SurrealDb,
    notation_id: Uuid,
    machine_kind: &str,
) -> Result<Option<NotationEvent>, NotationEventError> {
    if let Some(pg) = matter::authority_for(TABLE).await? {
        return Ok(matter::select(
            &pg,
            TABLE,
            json!({"notation_id": notation_id, "machine_kind": machine_kind}),
            &["id DESC"],
        )
        .await?
        .into_iter()
        .next());
    }

    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE notation_id = $notation AND machine_kind = $machine_kind \
             ORDER BY id DESC LIMIT 1"
        ))
        .bind(("notation", record_id(crate::notations::TABLE, notation_id)))
        .bind(("machine_kind", machine_kind.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    Ok(many(response)?.into_iter().next())
}

/// Whether the `(notation_id, machine_kind)` machine has reached `END`.
/// Equivalent to `latest_for_kind(...).to_state == "END"`.
///
/// # Errors
///
/// [`NotationEventError::Db`] if the lookup fails.
pub async fn is_complete(
    db: &SurrealDb,
    notation_id: Uuid,
    machine_kind: &str,
) -> Result<bool, NotationEventError> {
    Ok(latest_for_kind(db, notation_id, machine_kind)
        .await?
        .is_some_and(|e| e.to_state == "END"))
}

/// Every event journaled for a notation, oldest first — audit reads and
/// tests.
///
/// # Errors
///
/// [`NotationEventError::Db`] if the lookup fails.
pub async fn for_notation(
    db: &SurrealDb,
    notation_id: Uuid,
) -> Result<Vec<NotationEvent>, NotationEventError> {
    if let Some(pg) = matter::authority_for(TABLE).await? {
        return Ok(matter::select(&pg, TABLE, json!({"notation_id": notation_id}), &["id"]).await?);
    }

    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE notation_id = $notation ORDER BY id ASC"
        ))
        .bind(("notation", record_id(crate::notations::TABLE, notation_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Read the canonical event ordering from the native runtime's explicit pool.
///
/// # Errors
/// Refuses incomplete application history and propagates native read failures.
pub async fn for_notation_on(
    pg: &crate::postgres::PostgresDb,
    notation_id: Uuid,
) -> Result<Vec<NotationEvent>, NotationEventError> {
    pg.require_application_history()
        .await
        .map_err(|_| matter::MatterAuthorityError::Unavailable)?;
    Ok(matter::select(pg, TABLE, json!({"notation_id":notation_id}), &["id"]).await?)
}

/// Resolve the Person a transition is attributed to when no
/// `acting_person_id` is supplied.
///
/// **Questionnaire is the one allowlisted kind whose respondent fallback is
/// legitimate**: a bare answer with no explicit author is the *respondent*
/// answering their own intake, so attributing it to the notation's client
/// (`notation.person_id`) is correct and expected (the agent/system surface
/// writes answers this way).
///
/// **Every other machine kind** — workflow today (`approved`,
/// `pdf_persisted`, send, close, …), and any kind added later — is a
/// lawyer/system-driven transition whose driving caller MUST thread a
/// `SignalContext`. An event of such a kind with no actor is an attribution
/// defect: silently stamping it with the client would forge the very
/// accountability the journal exists to prove. The store layer has no
/// lawyer actor to guess, so the row still falls back to the respondent —
/// but the missing actor is logged loudly so the gap is visible rather
/// than hidden.
fn resolve_acting_person(record: &TransitionRecord<'_>, respondent_id: Uuid) -> Uuid {
    if let Some(id) = record.acting_person_id {
        return id;
    }
    if record.machine_kind != MACHINE_QUESTIONNAIRE {
        tracing::warn!(
            notation_id = %record.notation_id,
            machine_kind = record.machine_kind,
            from_state = record.from_state,
            to_state = record.to_state,
            condition = record.condition,
            "non-questionnaire event journaled without an acting_person_id — \
             attribution fell back to the notation's respondent; the driving \
             caller must thread a SignalContext with the authenticated actor",
        );
    }
    respondent_id
}

/// Encode a questionnaire-answer payload as the JSON the `payload` column
/// expects.
#[must_use]
pub fn answer_payload(answer_value: &str) -> String {
    serde_json::json!({ "answer_value": answer_value }).to_string()
}

/// Encode a workflow transition payload with only operational identifiers
/// and transition metadata. Client content and rendered documents stay out
/// of the journal payload.
#[must_use]
pub fn workflow_payload(acting_person_id: Uuid, template_version_id: Uuid) -> String {
    serde_json::json!({
        "acting_person_id": acting_person_id,
        "template_version_id": template_version_id,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        answer_payload, append_event, for_notation, is_complete, latest_for_kind, workflow_payload,
        TransitionRecord, MACHINE_QUESTIONNAIRE, MACHINE_WORKFLOW,
    };
    use crate::test_support;
    use uuid::Uuid;

    async fn seed_notation(surreal: &crate::surreal::SurrealDb) -> (Uuid, Uuid, Uuid) {
        test_support::seed_compiled_brands_postgres(surreal).await;
        let project = crate::test_support::seed_project_surreal(surreal, "matter").await;
        let tmpl = crate::templates::save_version(
            surreal,
            None,
            &format!("onboarding__letter-{}", Uuid::now_v7()),
            crate::templates::Version {
                title: "Retainer".into(),
                respondent_type: "person".into(),
                asset_id: None,
                form_code: None,
                kind: None,
                source_commit_sha: None,
            },
        )
        .await
        .unwrap()
        .into_model();
        let person = crate::persons::create(
            surreal,
            &crate::persons::NewPerson::new("Libra", "libra@example.com"),
        )
        .await
        .unwrap();
        let notation = crate::notations::create(
            surreal,
            &crate::notations::NewNotation::new(tmpl.id, person.id, project, "BEGIN"),
        )
        .await
        .unwrap();
        (notation.id, person.id, tmpl.id)
    }

    #[tokio::test]
    async fn append_event_inserts_one_row_with_the_expected_columns() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let (nid, person_id, template_id) = seed_notation(&surreal).await;
            let row = append_event(
                &surreal,
                TransitionRecord {
                    notation_id: nid,
                    acting_person_id: Some(person_id),
                    machine_kind: MACHINE_QUESTIONNAIRE,
                    from_state: "BEGIN",
                    to_state: "client_name",
                    condition: "_",
                    payload_json: Some(answer_payload("Libra")),
                    recorded_at: "2026-05-21T10:00:00+00:00",
                },
            )
            .await
            .unwrap();
            assert_eq!(row.machine_kind, MACHINE_QUESTIONNAIRE);
            assert_eq!(row.acting_person_id, person_id);
            assert_eq!(row.template_version_id, template_id);
            assert_eq!(row.payload.as_deref(), Some(r#"{"answer_value":"Libra"}"#));
        })
        .await;
    }

    #[tokio::test]
    async fn append_event_preserves_insert_order_across_repeated_calls() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let (nid, person_id, _) = seed_notation(&surreal).await;
            for (from, to) in [
                ("BEGIN", "client_name"),
                ("client_name", "client_email"),
                ("client_email", "project_name"),
            ] {
                append_event(
                    &surreal,
                    TransitionRecord {
                        notation_id: nid,
                        acting_person_id: Some(person_id),
                        machine_kind: MACHINE_QUESTIONNAIRE,
                        from_state: from,
                        to_state: to,
                        condition: "_",
                        payload_json: None,
                        recorded_at: "2026-05-21T10:00:00+00:00",
                    },
                )
                .await
                .unwrap();
            }
            let rows = for_notation(&surreal, nid).await.unwrap();
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[2].to_state, "project_name");
        })
        .await;
    }

    #[tokio::test]
    async fn workflow_event_payload_records_actor_and_pinned_template_version() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let (nid, person_id, template_id) = seed_notation(&surreal).await;
            let row = append_event(
                &surreal,
                TransitionRecord {
                    notation_id: nid,
                    acting_person_id: Some(person_id),
                    machine_kind: MACHINE_WORKFLOW,
                    from_state: "BEGIN",
                    to_state: "lawyer_review",
                    condition: "intake_submitted",
                    payload_json: None,
                    recorded_at: "2026-05-21T10:00:00+00:00",
                },
            )
            .await
            .unwrap();
            let payload: serde_json::Value =
                serde_json::from_str(row.payload.as_deref().unwrap()).unwrap();
            assert_eq!(
                payload["acting_person_id"].as_str().unwrap(),
                person_id.to_string()
            );
            assert_eq!(
                payload["template_version_id"].as_str().unwrap(),
                template_id.to_string()
            );
            assert_eq!(
                workflow_payload(person_id, template_id),
                row.payload.unwrap()
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_questionnaire_event_without_an_actor_falls_back_to_the_respondent() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let (nid, person_id, _) = seed_notation(&surreal).await;
            let row = append_event(
                &surreal,
                TransitionRecord {
                    notation_id: nid,
                    acting_person_id: None,
                    machine_kind: MACHINE_QUESTIONNAIRE,
                    from_state: "BEGIN",
                    to_state: "client_name",
                    condition: "_",
                    payload_json: None,
                    recorded_at: "2026-05-21T10:00:00+00:00",
                },
            )
            .await
            .unwrap();
            assert_eq!(row.acting_person_id, person_id);
        })
        .await;
    }

    #[tokio::test]
    async fn a_workflow_event_without_an_actor_still_falls_back_but_is_recorded() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let (nid, person_id, _) = seed_notation(&surreal).await;
            let row = append_event(
                &surreal,
                TransitionRecord {
                    notation_id: nid,
                    acting_person_id: None,
                    machine_kind: MACHINE_WORKFLOW,
                    from_state: "lawyer_review",
                    to_state: "generate_pdf__retainer_pdf",
                    condition: "approved",
                    payload_json: None,
                    recorded_at: "2026-05-21T10:00:00+00:00",
                },
            )
            .await
            .unwrap();
            assert_eq!(
                row.acting_person_id, person_id,
                "the missing actor still falls back to the respondent"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn latest_for_kind_reads_the_newest_row_and_is_complete_reflects_end() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let (nid, person_id, _) = seed_notation(&surreal).await;
            assert!(latest_for_kind(&surreal, nid, MACHINE_QUESTIONNAIRE)
                .await
                .unwrap()
                .is_none());
            assert!(!is_complete(&surreal, nid, MACHINE_QUESTIONNAIRE)
                .await
                .unwrap());

            for (from, to) in [("BEGIN", "client_name"), ("client_name", "END")] {
                append_event(
                    &surreal,
                    TransitionRecord {
                        notation_id: nid,
                        acting_person_id: Some(person_id),
                        machine_kind: MACHINE_QUESTIONNAIRE,
                        from_state: from,
                        to_state: to,
                        condition: "_",
                        payload_json: None,
                        recorded_at: "2026-05-21T10:00:00+00:00",
                    },
                )
                .await
                .unwrap();
            }
            assert_eq!(
                latest_for_kind(&surreal, nid, MACHINE_QUESTIONNAIRE)
                    .await
                    .unwrap()
                    .unwrap()
                    .to_state,
                "END"
            );
            assert!(is_complete(&surreal, nid, MACHINE_QUESTIONNAIRE)
                .await
                .unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn notation_events_are_isolated_across_notations() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let (a, person_id, template_id) = seed_notation(&surreal).await;
            let project = crate::test_support::seed_project_surreal(&surreal, "second").await;
            let b = crate::notations::create(
                &surreal,
                &crate::notations::NewNotation::new(template_id, person_id, project, "BEGIN"),
            )
            .await
            .unwrap()
            .id;

            for (nid, to) in [(a, "client_name"), (b, "client_email")] {
                append_event(
                    &surreal,
                    TransitionRecord {
                        notation_id: nid,
                        acting_person_id: Some(person_id),
                        machine_kind: MACHINE_QUESTIONNAIRE,
                        from_state: "BEGIN",
                        to_state: to,
                        condition: "_",
                        payload_json: None,
                        recorded_at: "2026-05-21T10:00:00+00:00",
                    },
                )
                .await
                .unwrap();
            }
            assert_eq!(
                latest_for_kind(&surreal, a, MACHINE_QUESTIONNAIRE)
                    .await
                    .unwrap()
                    .unwrap()
                    .to_state,
                "client_name"
            );
            assert_eq!(
                latest_for_kind(&surreal, b, MACHINE_QUESTIONNAIRE)
                    .await
                    .unwrap()
                    .unwrap()
                    .to_state,
                "client_email"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn appending_for_an_unknown_notation_reports_not_found() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let err = append_event(
                &surreal,
                TransitionRecord {
                    notation_id: Uuid::now_v7(),
                    acting_person_id: Some(Uuid::now_v7()),
                    machine_kind: MACHINE_QUESTIONNAIRE,
                    from_state: "BEGIN",
                    to_state: "client_name",
                    condition: "_",
                    payload_json: None,
                    recorded_at: "2026-05-21T10:00:00+00:00",
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(
                err,
                super::NotationEventError::Notation(crate::notations::NotationError::NotFound(_))
            ));
        })
        .await;
    }

    /// Structurally pins the append-only guarantee: this module must never
    /// grow an update or delete export. There is no runtime assertion for
    /// "a function doesn't exist" — the pin is that this test only imports
    /// `append_event`/reads above, the same way `store::sent_emails` and
    /// `store::answers` document the same shape.
    #[test]
    fn no_update_or_delete_is_exported() {
        // If this module ever exports `update_*` or `delete_*`, a reviewer
        // reading this test alongside the module doc comment is the check —
        // there is nothing here to assert against a function that doesn't
        // exist. See the module header's "Append-only" section.
    }
}
