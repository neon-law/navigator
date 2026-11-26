//! Historical Notation family import and parity.
//!
//! A matching pending scoped import may refresh its frozen source rows. The
//! first global copy inserts only missing identities before enrollment. Completed
//! enrollment refuses source reads and mutations, including deletion replay.
//!
//! Events retain their actor and Template pins; answers keep their respondent
//! separate from their author. Reports contain keys, never payloads.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use serde_json::Value as Json;
use sqlx::types::Json as Jsonb;
use sqlx::Row;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use super::tenancy::{compare, images, remirror, select_sql, source, target, upsert_sql};
use super::{changed, identity, micros, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::postgres::PostgresDb;
use crate::postgres_backfill::family::{self, ImportMode};
use crate::surreal::{record_id, record_uuid, SurrealDb};

pub(super) const NOTATION_COLUMNS: &[&str] = &[
    "id",
    "template_id",
    "package_id",
    "person_id",
    "entity_id",
    "project_id",
    "state",
    "delivery",
    "questionnaire_snapshot",
    "git_commit_sha",
    "template_source_commit_sha",
    "template_body_sha256",
    "inserted_at",
    "updated_at",
];
pub(super) const EVENT_COLUMNS: &[&str] = &[
    "id",
    "notation_id",
    "acting_person_id",
    "template_version_id",
    "machine_kind",
    "from_state",
    "to_state",
    "condition",
    "payload",
    "recorded_at",
    "inserted_at",
    "updated_at",
];
pub(super) const ANSWER_COLUMNS: &[&str] = &[
    "id",
    "question_id",
    "person_id",
    "notation_id",
    "state_name",
    "value",
    "source",
    "authored_by_person_id",
    "inserted_at",
    "updated_at",
];

static NOTATION_UPSERT: LazyLock<String> =
    LazyLock::new(|| upsert_sql("notation", NOTATION_COLUMNS));
static EVENT_UPSERT: LazyLock<String> =
    LazyLock::new(|| upsert_sql("notation_event", EVENT_COLUMNS));
static ANSWER_UPSERT: LazyLock<String> = LazyLock::new(|| upsert_sql("answer", ANSWER_COLUMNS));
static NOTATION_SELECT: LazyLock<String> =
    LazyLock::new(|| select_sql("notation", NOTATION_COLUMNS));
static EVENT_SELECT: LazyLock<String> =
    LazyLock::new(|| select_sql("notation_event", EVENT_COLUMNS));
static ANSWER_SELECT: LazyLock<String> = LazyLock::new(|| select_sql("answer", ANSWER_COLUMNS));

const NOTATION_PRESENT: &str = "SELECT EXISTS (SELECT 1 FROM notation WHERE id = $1)";
const PROJECT_PRESENT: &str = "SELECT EXISTS (SELECT 1 FROM project WHERE id = $1)";
const PACKAGE_PRESENT: &str = "SELECT EXISTS (SELECT 1 FROM notation_package WHERE id = $1)";
const QUESTION_PRESENT: &str = "SELECT EXISTS (SELECT 1 FROM question WHERE id = $1)";

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotationImage {
    id: Uuid,
    template_id: Uuid,
    package_id: Option<Uuid>,
    person_id: Uuid,
    entity_id: Option<Uuid>,
    project_id: Uuid,
    state: String,
    delivery: String,
    questionnaire_snapshot: Option<Json>,
    git_commit_sha: Option<String>,
    template_source_commit_sha: Option<String>,
    template_body_sha256: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl NotationImage {
    fn from_source(row: &crate::notations::Notation) -> Self {
        Self {
            id: row.id,
            template_id: row.template_id,
            package_id: row.package_id,
            person_id: row.person_id,
            entity_id: row.entity_id,
            project_id: row.project_id,
            state: row.state.clone(),
            delivery: row.delivery.clone(),
            questionnaire_snapshot: row.questionnaire_snapshot.clone(),
            git_commit_sha: row.git_commit_sha.clone(),
            template_source_commit_sha: row.template_source_commit_sha.clone(),
            template_body_sha256: row.template_body_sha256.clone(),
            inserted_at: micros(row.inserted_at),
            updated_at: micros(row.updated_at),
        }
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        let snapshot: Option<Jsonb<Json>> = row.try_get("questionnaire_snapshot")?;
        Ok(Self {
            id: row.try_get("id")?,
            template_id: row.try_get("template_id")?,
            package_id: row.try_get("package_id")?,
            person_id: row.try_get("person_id")?,
            entity_id: row.try_get("entity_id")?,
            project_id: row.try_get("project_id")?,
            state: row.try_get("state")?,
            delivery: row.try_get("delivery")?,
            questionnaire_snapshot: snapshot.map(|value| value.0),
            git_commit_sha: row.try_get("git_commit_sha")?,
            template_source_commit_sha: row.try_get("template_source_commit_sha")?,
            template_body_sha256: row.try_get("template_body_sha256")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

const EVENT_SOURCE_FIELDS: &str = "id, notation_id, acting_person_id, template_version_id, \
    machine_kind, from_state, to_state, condition, payload, recorded_at, inserted_at, updated_at";

/// The Notation Event row as SurrealDB stores it.
#[derive(SurrealValue)]
struct EventSource {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventImage {
    id: Uuid,
    notation_id: Uuid,
    acting_person_id: Uuid,
    template_version_id: Uuid,
    machine_kind: String,
    from_state: String,
    to_state: String,
    condition: String,
    payload: Option<String>,
    recorded_at: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// A link must name a native UUID key; one that does not has no PostgreSQL
/// identity and makes the row unmirrorable.
fn link(record: &surrealdb::types::RecordId) -> Result<Uuid, MirrorError> {
    record_uuid(record).ok_or(MirrorError::Source)
}

impl EventImage {
    fn from_source(row: &EventSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: link(&row.id)?,
            notation_id: link(&row.notation_id)?,
            acting_person_id: link(&row.acting_person_id)?,
            template_version_id: link(&row.template_version_id)?,
            machine_kind: row.machine_kind.clone(),
            from_state: row.from_state.clone(),
            to_state: row.to_state.clone(),
            condition: row.condition.clone(),
            payload: row.payload.clone(),
            recorded_at: row.recorded_at.clone(),
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            notation_id: row.try_get("notation_id")?,
            acting_person_id: row.try_get("acting_person_id")?,
            template_version_id: row.try_get("template_version_id")?,
            machine_kind: row.try_get("machine_kind")?,
            from_state: row.try_get("from_state")?,
            to_state: row.try_get("to_state")?,
            condition: row.try_get("condition")?,
            payload: row.try_get("payload")?,
            recorded_at: row.try_get("recorded_at")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

const ANSWER_SOURCE_FIELDS: &str = "id, question_id, person_id, notation_id, state_name, value, \
    source, authored_by_person_id, inserted_at, updated_at";

/// The answer row as SurrealDB stores it. `value` is `TYPE any`; a row
/// written without one reads as absent and is refused by PostgreSQL's
/// `NOT NULL`.
#[derive(SurrealValue)]
struct AnswerSource {
    id: surrealdb::types::RecordId,
    question_id: surrealdb::types::RecordId,
    person_id: surrealdb::types::RecordId,
    notation_id: Option<surrealdb::types::RecordId>,
    state_name: Option<String>,
    value: Option<Json>,
    source: String,
    authored_by_person_id: Option<surrealdb::types::RecordId>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnswerImage {
    id: Uuid,
    question_id: Uuid,
    person_id: Uuid,
    notation_id: Option<Uuid>,
    state_name: Option<String>,
    value: Option<Json>,
    source: String,
    authored_by_person_id: Option<Uuid>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl AnswerImage {
    fn from_source(row: &AnswerSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: link(&row.id)?,
            question_id: link(&row.question_id)?,
            person_id: link(&row.person_id)?,
            notation_id: row.notation_id.as_ref().map(link).transpose()?,
            state_name: row.state_name.clone(),
            value: row.value.clone(),
            source: row.source.clone(),
            authored_by_person_id: row.authored_by_person_id.as_ref().map(link).transpose()?,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        let value: Option<Jsonb<Json>> = row.try_get("value")?;
        Ok(Self {
            id: row.try_get("id")?,
            question_id: row.try_get("question_id")?,
            person_id: row.try_get("person_id")?,
            notation_id: row.try_get("notation_id")?,
            state_name: row.try_get("state_name")?,
            value: value.map(|value| value.0),
            source: row.try_get("source")?,
            authored_by_person_id: row.try_get("authored_by_person_id")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

async fn source_rows<T: SurrealValue>(
    surreal: &SurrealDb,
    fields: &str,
    table: &str,
) -> Result<Vec<T>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {fields} FROM {table}"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?;
    response.take(0).map_err(source)
}

async fn source_row<T: SurrealValue>(
    surreal: &SurrealDb,
    fields: &str,
    table: &str,
    id: Uuid,
) -> Result<Option<T>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {fields} FROM ONLY $id LIMIT 1"))
        .bind(("id", record_id(table, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?;
    response.take(0).map_err(source)
}

async fn present(pg: &PostgresDb, sql: &'static str, id: Uuid) -> Result<bool, MirrorError> {
    Ok(sqlx::query_scalar(sql).bind(id).fetch_one(&pg.0).await?)
}

/// Select permission before reading any compatibility row.
async fn source_write_mode(pg: &PostgresDb, table: &str) -> Result<ImportMode, MirrorError> {
    match family::import_mode(pg, table).await? {
        ImportMode::Refuse => Err(MirrorError::MatterAuthority),
        mode => Ok(mode),
    }
}

/// Make PostgreSQL's copy of one workflow row match SurrealDB's.
pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    match key {
        MirrorKey::Notation(id) => mirror_notation(surreal, pg, *id).await,
        MirrorKey::NotationEvent(id) => mirror_event(surreal, pg, *id).await,
        MirrorKey::Answer(id) => mirror_answer(surreal, pg, *id).await,
        // `super::mirror` routes only workflow keys here.
        _ => Err(MirrorError::Source),
    }
}

async fn mirror_notation(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let mode = source_write_mode(pg, "notation").await?;
    let notation = super::from_source(crate::notations::find_by_id(surreal, id))
        .await
        .map_err(source)?
        .ok_or(MirrorError::Source)?;
    upsert_notation(surreal, pg, &NotationImage::from_source(&notation), mode).await
}

/// Import a Notation. Scoped refresh requires its out-of-family parents
/// already to exist under their own PostgreSQL authorities.
async fn upsert_notation(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    notation: &NotationImage,
    mode: ImportMode,
) -> Result<Mirrored, MirrorError> {
    if mode == ImportMode::InsertMissing {
        if !present(pg, PROJECT_PRESENT, notation.project_id).await? {
            super::tenancy::mirror(surreal, pg, &MirrorKey::Project(notation.project_id)).await?;
        }
        if let Some(package) = notation.package_id {
            if !present(pg, PACKAGE_PRESENT, package).await? {
                // Only the genuine first global import may supply this parent.
                let key = MirrorKey::NotationPackage(package);
                super::catalog::mirror(surreal, pg, &key).await?;
            }
        }
        ensure_template(surreal, pg, notation.template_id).await?;
        identity::ensure(surreal, pg, MirrorKey::Person(notation.person_id)).await?;
        if let Some(entity) = notation.entity_id {
            identity::ensure(surreal, pg, MirrorKey::Entity(entity)).await?;
        }
    }
    let written = sqlx::query(super::held_write(
        mode == ImportMode::InsertMissing,
        NOTATION_UPSERT.as_str(),
    ))
    .bind(notation.id)
    .bind(notation.template_id)
    .bind(notation.package_id)
    .bind(notation.person_id)
    .bind(notation.entity_id)
    .bind(notation.project_id)
    .bind(&notation.state)
    .bind(&notation.delivery)
    .bind(notation.questionnaire_snapshot.as_ref().map(Jsonb))
    .bind(&notation.git_commit_sha)
    .bind(&notation.template_source_commit_sha)
    .bind(&notation.template_body_sha256)
    .bind(notation.inserted_at)
    .bind(notation.updated_at)
    .execute(&pg.0)
    .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

/// Import a missing same-family Notation through the selected import mode.
async fn ensure_notation(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<(), MirrorError> {
    if present(pg, NOTATION_PRESENT, id).await? {
        return Ok(());
    }
    mirror_notation(surreal, pg, id).await?;
    Ok(())
}

async fn ensure_template(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<(), MirrorError> {
    if !present(
        pg,
        "SELECT EXISTS (SELECT 1 FROM template WHERE id = $1)",
        id,
    )
    .await?
    {
        super::templates::mirror(surreal, pg, id).await?;
    }
    Ok(())
}

async fn mirror_event(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let mode = source_write_mode(pg, "notation_event").await?;
    let row: Option<EventSource> = source_row(
        surreal,
        EVENT_SOURCE_FIELDS,
        crate::notation_events::TABLE,
        id,
    )
    .await?;
    let Some(row) = row else {
        return Err(MirrorError::Source);
    };
    let event = EventImage::from_source(&row)?;
    ensure_notation(surreal, pg, event.notation_id).await?;
    if mode == ImportMode::InsertMissing {
        ensure_template(surreal, pg, event.template_version_id).await?;
        identity::ensure(surreal, pg, MirrorKey::Person(event.acting_person_id)).await?;
    }
    let written = sqlx::query(super::held_write(
        mode == ImportMode::InsertMissing,
        EVENT_UPSERT.as_str(),
    ))
    .bind(event.id)
    .bind(event.notation_id)
    .bind(event.acting_person_id)
    .bind(event.template_version_id)
    .bind(&event.machine_kind)
    .bind(&event.from_state)
    .bind(&event.to_state)
    .bind(&event.condition)
    .bind(&event.payload)
    .bind(&event.recorded_at)
    .bind(event.inserted_at)
    .bind(event.updated_at)
    .execute(&pg.0)
    .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

async fn mirror_answer(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let mode = source_write_mode(pg, "answer").await?;
    let row: Option<AnswerSource> =
        source_row(surreal, ANSWER_SOURCE_FIELDS, crate::answers::TABLE, id).await?;
    let Some(row) = row else {
        return Err(MirrorError::Source);
    };
    let answer = AnswerImage::from_source(&row)?;
    if mode == ImportMode::InsertMissing
        && !present(pg, QUESTION_PRESENT, answer.question_id).await?
    {
        // After global completion, a missing native parent must remain a refusal.
        let key = MirrorKey::Question(answer.question_id);
        super::catalog::mirror(surreal, pg, &key).await?;
    }
    if let Some(notation) = answer.notation_id {
        ensure_notation(surreal, pg, notation).await?;
    }
    if mode == ImportMode::InsertMissing {
        identity::ensure(surreal, pg, MirrorKey::Person(answer.person_id)).await?;
        if let Some(author) = answer.authored_by_person_id {
            identity::ensure(surreal, pg, MirrorKey::Person(author)).await?;
        }
    }
    let written = sqlx::query(super::held_write(
        mode == ImportMode::InsertMissing,
        ANSWER_UPSERT.as_str(),
    ))
    .bind(answer.id)
    .bind(answer.question_id)
    .bind(answer.person_id)
    .bind(answer.notation_id)
    .bind(&answer.state_name)
    .bind(answer.value.as_ref().map(Jsonb))
    .bind(&answer.source)
    .bind(answer.authored_by_person_id)
    .bind(answer.inserted_at)
    .bind(answer.updated_at)
    .execute(&pg.0)
    .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

/// One table's source rows keyed for comparison, skipping a record whose own
/// key is not a native UUID: it has no PostgreSQL identity.
fn keyed<S, I>(
    rows: &[S],
    id: impl Fn(&S) -> &surrealdb::types::RecordId,
    key: impl Fn(Uuid) -> MirrorKey,
    image: impl Fn(&S) -> Result<I, MirrorError>,
) -> BTreeMap<MirrorKey, Option<I>> {
    rows.iter()
        .filter_map(|row| Some((key(record_uuid(id(row))?), image(row).ok())))
        .collect()
}

/// Compare every Notation, Notation Event, and answer in both stores,
/// Notations first.
///
/// # Errors
///
/// Either store could not be read.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    if family::active_for_table(pg, "notation").await? {
        return Ok(ParityReport { tables: Vec::new() });
    }
    let importing = family::importing("notation");
    if !importing && !super::compares_authoritative(pg).await? {
        return Ok(ParityReport { tables: Vec::new() });
    }
    let notations = super::from_source(crate::notations::list_all(surreal))
        .await
        .map_err(source)?;
    let events: Vec<EventSource> =
        source_rows(surreal, EVENT_SOURCE_FIELDS, crate::notation_events::TABLE).await?;
    let answers: Vec<AnswerSource> =
        source_rows(surreal, ANSWER_SOURCE_FIELDS, crate::answers::TABLE).await?;

    let mut tables = vec![
        compare(
            "notation",
            &images(
                &notations,
                |row| MirrorKey::Notation(row.id),
                |row| Ok(NotationImage::from_source(row)),
            ),
            &target(
                pg,
                NOTATION_SELECT.as_str(),
                NotationImage::from_row,
                |row| MirrorKey::Notation(row.id),
            )
            .await?,
        ),
        compare(
            "notation_event",
            &keyed(
                &events,
                |row| &row.id,
                MirrorKey::NotationEvent,
                EventImage::from_source,
            ),
            &target(pg, EVENT_SELECT.as_str(), EventImage::from_row, |row| {
                MirrorKey::NotationEvent(row.id)
            })
            .await?,
        ),
        compare(
            "answer",
            &keyed(
                &answers,
                |row| &row.id,
                MirrorKey::Answer,
                AnswerImage::from_source,
            ),
            &target(pg, ANSWER_SELECT.as_str(), AnswerImage::from_row, |row| {
                MirrorKey::Answer(row.id)
            })
            .await?,
        ),
    ];
    if !importing {
        for report in &mut tables {
            *report = super::only_missing(pg, report.clone()).await?;
        }
    }
    Ok(ParityReport { tables })
}

/// Re-mirror every divergent workflow row from SurrealDB, then compare
/// again. A row that fails both passes stays in the returned report.
///
/// # Errors
///
/// Either store could not be read for the comparison.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    remirror(surreal, pg, &before).await;
    parity(surreal, pg).await
}

/// Verify pins of the exact immutable Template versions referenced by Source
/// Notations. A historical same-ID pin repair must reach PostgreSQL before
/// opening native Notations from that version.
pub(crate) async fn template_pin_parity(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<crate::postgres_backfill::family::TemplatePinProof, MirrorError> {
    #[derive(SurrealValue)]
    struct Pins {
        id: surrealdb::types::RecordId,
        source_commit_sha: Option<String>,
        body_sha256: Option<String>,
    }
    let source_links: Vec<surrealdb::types::RecordId> = surreal
        .query("SELECT VALUE template_id FROM notation")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?
        .take(0)
        .map_err(source)?;
    let ids = source_links
        .iter()
        .map(|id| record_uuid(id).ok_or(MirrorError::Source))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    let record_keys: Vec<_> = ids.iter().map(|id| record_id("template", *id)).collect();
    let rows: Vec<Pins> = surreal
        .query("SELECT id,source_commit_sha,body_sha256 FROM template WHERE id IN $ids")
        .bind(("ids", record_keys))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?
        .take(0)
        .map_err(source)?;
    let source_pins = rows
        .into_iter()
        .map(|row| {
            Ok((
                record_uuid(&row.id).ok_or(MirrorError::Source)?,
                (row.source_commit_sha, row.body_sha256),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, MirrorError>>()?;
    let target: Vec<(Uuid, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT id,source_commit_sha,body_sha256 FROM template WHERE id=ANY($1)")
            .bind(ids.iter().copied().collect::<Vec<_>>())
            .fetch_all(pg.pool())
            .await?;
    let target: BTreeMap<_, _> = target
        .into_iter()
        .map(|(id, commit, body)| (id, (commit, body)))
        .collect();
    let differences = ids
        .iter()
        .filter(|id| !source_pins.contains_key(id) || source_pins.get(id) != target.get(id))
        .count();
    Ok(crate::postgres_backfill::family::TemplatePinProof {
        compared: ids.len(),
        differences,
    })
}

/// Compare reset protection for each captured Source Notation, including parents
/// not copied yet. PG-only Notations remain subject to strict table parity.
/// Activation requires the differences to be empty.
/// Signature evidence stays on its existing PostgreSQL authority.
pub(crate) async fn reset_fact_differences(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<(Vec<Uuid>, crate::postgres_backfill::family::ResetUnionProof), MirrorError> {
    let mut differences = Vec::new();
    let mut proof = crate::postgres_backfill::family::ResetUnionProof {
        compared: 0,
        retained_native_protections: 0,
    };
    let mut start = 0usize;
    loop {
        let ids: Vec<surrealdb::types::RecordId> = surreal
            .query("SELECT VALUE id FROM notation ORDER BY id LIMIT 256 START $start")
            .bind(("start", start))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(source)?
            .take(0)
            .map_err(source)?;
        if ids.is_empty() {
            break;
        }
        for record in &ids {
            let id = record_uuid(record).ok_or(MirrorError::Source)?;
            let (source_document, _) = crate::notation_reset::source_facts(surreal, id)
                .await
                .map_err(source)?;
            let (target_document, _) = crate::postgres::notations::draft_facts(pg, id).await?;
            proof.compared += 1;
            if source_document != target_document {
                if target_document && super::documents::native_protects(pg, id).await? {
                    proof.retained_native_protections += 1;
                } else {
                    differences.push(id);
                }
            }
        }
        start = start.checked_add(ids.len()).ok_or(MirrorError::Source)?;
    }
    Ok((differences, proof))
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "notation_event_import_tests.rs"]
mod notation_event_import_tests;
