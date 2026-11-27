//! Project Deadline MCP tools.
//!
//! `list_deadlines` stays on the statutory workflow clock. These tools read
//! and write `project_deadlines`. A caller who does not participate in the
//! Project is refused. Each write returns the row the store read back.

use serde::Deserialize;
use serde_json::{json, Value};
use store::persons::Role;
use store::project_deadlines::{
    AuthorityKind, CalendarRole, Category, DeadlineIdentity, DueOnInput, Hearing,
};
use store::project_deadlines::{Deadline, DeadlineError};
use uuid::Uuid;

use super::ToolError;

pub(super) async fn dispatch(
    name: &str,
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    match name {
        "list_project_deadlines" => list_call(application_store, person_id, role, arguments).await,
        "queue_court_date" => queue_call(application_store, person_id, role, arguments).await,
        "create_deadline" => create_call(application_store, person_id, role, arguments).await,
        "update_deadline" => update_call(application_store, person_id, role, arguments).await,
        "complete_deadline" => complete_call(application_store, person_id, role, arguments).await,
        "cancel_deadline" => cancel_call(application_store, person_id, role, arguments).await,
        "reopen_deadline" => reopen_call(application_store, person_id, role, arguments).await,
        other => Err(ToolError::Unknown(other.to_owned())),
    }
}

#[must_use]
pub fn list_descriptor() -> Value {
    json!({
        "name": "list_project_deadlines",
        "description": "List the deadlines stored on one Project the caller participates in. \
                        Each row includes its title, stored due instant, authority, calendar role, and status.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": { "type": "string", "format": "uuid" }
            },
            "required": ["project_id"],
            "additionalProperties": false
        }
    })
}

fn due_on_schema() -> Value {
    json!({"type": "object", "required": ["date"], "additionalProperties": false,
           "properties": {"date": {"type":"string","format":"date"}, "timezone":{"type":"string","description":"IANA time zone"},
           "cutoff":{"type":"string","description":"Optional local cutoff, HH:MM:SS"}}})
}

/// Agents can prepare a revision. Releasing it requires the lawyer's interactive review.
#[must_use]
pub fn queue_descriptor() -> Value {
    let rect = json!({"type":"object","required":["x","y","width","height"],"properties":{
        "x":{"type":"number"},"y":{"type":"number"},"width":{"type":"number"},"height":{"type":"number"}}});
    let evidence = json!({"type":"object","required":["role","id","sha256_hex","page","rects"],"properties":{
        "role":{"enum":["signed_order","trigger","authority"]},"id":{"type":"string","format":"uuid"},
        "sha256_hex":{"type":"string","pattern":"^[0-9a-f]{64}$"},"page":{"type":"integer","minimum":1},
        "rects":{"type":"array","items":rect},"forgejo":forgejo_pin_schema()
    }});
    let draft = json!({"type":"object","required":["kind","title"],"properties":{
        "kind":{"enum":["signed_order","calculated"]},"title":{"type":"string","maxLength":200},
        "timezone":{"type":"string"},"due_on":{"type":"string","format":"date"},
        "due_at":{"type":"string","format":"date-time"},"cutoff":{"type":"string"},
        "hearing":{"type":"object"},"category":{"enum":store::project_deadlines::CATEGORIES},
        "calendar_role":{"enum":store::project_deadlines::CALENDAR_ROLES},
        "calculation":{"type":"object","description":"trigger, trigger_on, offset_days, counting_basis and authority_kind."},
        "evidence":{"type":"array","maxItems":32,"items":evidence}
    }});
    json!({
        "name":"queue_court_date",
        "description":"Stage a court date for a participating lawyer to review in Navigator. Source IDs, digests, one-based pages and normalized highlight rectangles refer to filed originals. This tool creates a proposal.",
        "inputSchema":{"type":"object","additionalProperties":false,"required":["project_id","request_id","draft"],"properties":{
            "project_id":{"type":"string","format":"uuid"},"request_id":{"type":"string","format":"uuid"},
            "target_deadline_id":{"type":"string","format":"uuid"},"draft":draft
        }}
    })
}

/// The same immutable repository coordinates are accepted by the HTTP and agent doors.
#[must_use]
pub fn forgejo_pin_schema() -> Value {
    json!({
        "type":"object","additionalProperties":false,"required":["repository","commit","path"],
        "properties":{
            "repository":{"enum":["project","firm_library"]},
            "commit":{"type":"string","pattern":"^[0-9a-f]{40}$"},
            "path":{"type":"string","minLength":1,"description":"Repository-relative path at the cited commit."}
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueArgs {
    project_id: Uuid,
    request_id: Uuid,
    target_deadline_id: Option<Uuid>,
    draft: model::deadline_proposals::DraftInput,
}

pub async fn queue_call(
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args: QueueArgs = super::decode_args(arguments)?;
    let project_id = args.project_id;
    if !role.is_lawyer_tier()
        || !store::projects::has_firm_participation(application_store, person_id, project_id)
            .await
            .map_err(|e| ToolError::Database(e.to_string()))?
    {
        return Err(ToolError::NotFound("project".into()));
    }
    let pg = store::postgres::PostgresDb::application()
        .await
        .map_err(|e| ToolError::Database(e.to_string()))?;
    let draft = store::deadline_proposals::resolve_draft(
        &pg,
        application_store,
        project_id,
        args.request_id,
        None,
        args.draft,
    )
    .await
    .map_err(map_proposal)?;
    let receipts = if draft.evidence.iter().any(|source| source.forgejo.is_some()) {
        let kms = cloud::runtime_kms_from_env().await;
        store::court_date_forgejo::verify(
            application_store,
            project_id,
            &draft,
            role.is_lawyer_tier(),
            kms.as_ref(),
        )
        .await
        .map_err(map_forgejo)?
    } else {
        Vec::new()
    };
    let proposal = store::deadline_proposals::stage_verified(
        &pg,
        project_id,
        args.request_id,
        args.target_deadline_id,
        &draft,
        &receipts,
    )
    .await
    .map_err(map_proposal)?;
    Ok(
        json!({"content":[{"type":"text","text":"Court date queued for lawyer review."}],"structuredContent":{"proposal":proposal}}),
    )
}

fn map_forgejo(error: store::court_date_forgejo::Error) -> ToolError {
    use store::court_date_forgejo::Error;
    match error {
        Error::Invalid => ToolError::InvalidArguments(
            "The pinned Forgejo source does not match the filed evidence.".into(),
        ),
        Error::NotFound => ToolError::NotFound("cited source".into()),
        Error::Forbidden => ToolError::Forbidden("The cited source is outside your access.".into()),
        Error::Unavailable => ToolError::Internal(
            "The Firm’s Forgejo source could not be verified. Retry when available.".into(),
        ),
    }
}

fn map_proposal(e: store::deadline_proposals::Error) -> ToolError {
    match e {
        store::deadline_proposals::Error::Invalid(reason) => {
            ToolError::InvalidArguments(reason.into())
        }
        store::deadline_proposals::Error::Conflict => {
            ToolError::Conflict("The court-date revision changed".into())
        }
        store::deadline_proposals::Error::NotFound => ToolError::NotFound("proposal".into()),
        other => ToolError::Database(other.to_string()),
    }
}

#[must_use]
pub fn create_descriptor() -> Value {
    json!({
        "name": "create_deadline",
        "description": "Store a deadline on a Project the caller participates in and return the written row. \
                        Supply an offset timestamp in due_at or a civil date in due_on. An omitted civil-date time zone inherits the Project zone. Use queue_court_date for hearings and court-bound dates. A participating lawyer confirms their evidence in Navigator. \
                        Dates are stored as given; court days are not computed.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": { "type": "string", "format": "uuid" },
                "description": { "type": "string" },
                "title": { "type": "string", "maxLength": 200, "description": "A short name for what is due, such as the pleading or response." },
                "due_at": { "type": "string", "format": "date-time" },
                "due_on": due_on_schema(),
                "hearing": {"type":"object","additionalProperties":false,"properties":{"duration_minutes":{"type":"integer","minimum":1,"maximum":u32::MAX},"location":{"type":"string"}}},
                "authority_kind": { "type": "string", "enum": store::project_deadlines::AUTHORITY_KINDS },
                "authority": { "type": "string" },
                "source": { "type": "string" },
                "source_document_id": { "type": "string", "format": "uuid" },
                "owner_person_id": { "type": "string", "format": "uuid" },
                "calendar_role": { "type": "string", "enum": store::project_deadlines::CALENDAR_ROLES },
                "category": { "type": "string", "enum": store::project_deadlines::CATEGORIES, "description": "court, hearing, discovery, internal, or tickler. Omitted lets the store settle it from the calendar role or an internal authority." },
                "internal_lead_days": { "type": "integer" },
                "client_lead_days": { "type": "integer" },
                "replay_key": { "type": "string" }
            },
            "required": ["project_id", "authority_kind", "calendar_role"],
            "oneOf": [{"required": ["due_at"]}, {"required": ["due_on"]}],
            "additionalProperties": false
        }
    })
}

#[must_use]
pub fn update_descriptor() -> Value {
    json!({
        "name": "update_deadline",
        "description": "Change a stored deadline and return the written row. Status changes only through complete_deadline, cancel_deadline, and reopen_deadline.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": { "type": "string", "format": "uuid" },
                "deadline_id": { "type": "string", "format": "uuid" },
                "description": { "type": "string" },
                "title": { "type": "string", "maxLength": 200, "description": "An empty string clears the title." },
                "due_at": { "type": "string", "format": "date-time" },
                "due_on": due_on_schema(),
                "hearing": {"type":"object","additionalProperties":false,"properties":{"duration_minutes":{"type":"integer","minimum":1,"maximum":u32::MAX},"location":{"type":"string"}}},
                "authority_kind": { "type": "string", "enum": store::project_deadlines::AUTHORITY_KINDS },
                "authority": { "type": "string" },
                "source": { "type": "string" },
                "source_document_id": { "type": ["string", "null"], "format": "uuid" },
                "owner_person_id": { "type": ["string", "null"], "format": "uuid" },
                "calendar_role": { "type": "string", "enum": store::project_deadlines::CALENDAR_ROLES },
                "category": { "type": ["string", "null"], "description": "court, hearing, discovery, internal, or tickler. An empty string clears it." },
                "internal_lead_days": { "type": ["integer", "null"] },
                "client_lead_days": { "type": ["integer", "null"] }
            },
            "required": ["project_id", "deadline_id"],
            "not": {"required": ["due_at", "due_on"]},
            "additionalProperties": false
        }
    })
}

#[must_use]
pub fn complete_descriptor() -> Value {
    transition_descriptor(
        "complete_deadline",
        "Set a deadline's status to complete and return the stored row. This is not a delete.",
    )
}

#[must_use]
pub fn cancel_descriptor() -> Value {
    transition_descriptor(
        "cancel_deadline",
        "Set a deadline's status to cancelled and return the stored row. This is not a delete. Use reopen_deadline to restore it.",
    )
}

#[must_use]
pub fn reopen_descriptor() -> Value {
    transition_descriptor(
        "reopen_deadline",
        "Restore a cancelled deadline to open and return the same stored row. An open row is unchanged; a complete row is refused.",
    )
}

fn transition_descriptor(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": { "type": "string", "format": "uuid" },
                "deadline_id": { "type": "string", "format": "uuid" }
            },
            "required": ["project_id", "deadline_id"],
            "additionalProperties": false
        }
    })
}

#[derive(Deserialize)]
struct ProjectArgs {
    project_id: Uuid,
}

#[derive(Deserialize)]
struct CreateArgs {
    project_id: Uuid,
    title: Option<String>,
    description: Option<String>,
    due_at: Option<String>,
    due_on: Option<DueOnInput>,
    hearing: Option<Hearing>,
    authority_kind: AuthorityKind,
    #[serde(default)]
    authority: String,
    #[serde(default)]
    source: String,
    source_document_id: Option<Uuid>,
    owner_person_id: Option<Uuid>,
    calendar_role: CalendarRole,
    #[serde(
        default,
        deserialize_with = "store::project_deadlines::optional_category"
    )]
    category: Option<Category>,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<String>,
}

#[derive(Deserialize)]
#[allow(clippy::option_option)]
struct UpdateArgs {
    project_id: Uuid,
    deadline_id: Uuid,
    title: Option<String>,
    description: Option<String>,
    due_at: Option<String>,
    due_on: Option<DueOnInput>,
    hearing: Option<Hearing>,
    authority_kind: Option<String>,
    authority: Option<String>,
    source: Option<String>,
    source_document_id: Option<Option<Uuid>>,
    owner_person_id: Option<Option<Uuid>>,
    calendar_role: Option<String>,
    /// An empty string clears the category.
    category: Option<String>,
    internal_lead_days: Option<Option<i64>>,
    client_lead_days: Option<Option<i64>>,
}

#[derive(Deserialize)]
struct IdArgs {
    project_id: Uuid,
    deadline_id: Uuid,
}

async fn require_participant(
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    role: Role,
    project_id: Uuid,
) -> Result<(), ToolError> {
    let viewer = super::portal_access()
        .matter_viewer(application_store, Some(person_id), role, project_id)
        .await
        .map_err(ToolError::Database)?;
    if viewer.is_some() {
        Ok(())
    } else {
        Err(ToolError::NotFound(format!("project_id={project_id}")))
    }
}

fn map_deadline(error: DeadlineError) -> ToolError {
    match error {
        DeadlineError::Invalid(message) => ToolError::InvalidArguments(message),
        DeadlineError::NotFound => ToolError::NotFound("deadline".into()),
        other => ToolError::Database(other.to_string()),
    }
}

fn row_result(deadline: &Deadline, summary: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": summary }],
        "structuredContent": { "deadline": deadline }
    })
}

pub async fn list_call(
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args: ProjectArgs = super::decode_args(arguments)?;
    require_participant(application_store, person_id, role, args.project_id).await?;
    let deadlines = store::project_deadlines::list(application_store, args.project_id)
        .await
        .map_err(map_deadline)?;
    Ok(json!({
        "content": [{
            "type": "text",
            "text": format!("{} deadlines on project {}.", deadlines.len(), args.project_id)
        }],
        "structuredContent": {
            "count": deadlines.len(),
            "deadlines": deadlines
        }
    }))
}

pub async fn create_call(
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args: CreateArgs = super::decode_args(arguments)?;
    require_participant(application_store, person_id, role, args.project_id).await?;
    if model::project_deadlines::requires_court_review(
        &args.calendar_role.to_string(),
        &args.authority_kind.to_string(),
        args.category
            .map(|category| category.to_string())
            .as_deref(),
    ) {
        return Err(ToolError::InvalidArguments(
            "Use queue_court_date to submit this date for lawyer review in Navigator.".into(),
        ));
    }
    let (due_at, due_on) = store::project_deadlines::resolve_due(
        application_store,
        args.project_id,
        DeadlineIdentity::Replay(args.replay_key.as_deref()),
        args.due_at.as_deref(),
        args.due_on,
    )
    .await
    .map_err(map_deadline)?;
    let deadline = store::project_deadlines::create(
        application_store,
        &store::project_deadlines::NewDeadline {
            project_id: args.project_id,
            title: args.title,
            description: args.description,
            due_on,
            hearing: args.hearing,
            effort_minutes: None,
            due_at,
            authority_kind: args.authority_kind.to_string(),
            authority: args.authority,
            source: args.source,
            source_document_id: args.source_document_id,
            owner_person_id: args.owner_person_id,
            calendar_role: args.calendar_role.to_string(),
            category: args.category.map(|category| category.to_string()),
            internal_lead_days: args.internal_lead_days,
            client_lead_days: args.client_lead_days,
            replay_key: args.replay_key,
        },
    )
    .await
    .map_err(map_deadline)?;
    Ok(row_result(
        &deadline,
        &format!(
            "Stored deadline {} due {}.",
            deadline.id,
            deadline.due_on.as_ref().map_or_else(
                || deadline.due_at.clone(),
                store::project_deadlines::DueOn::display_date
            )
        ),
    ))
}

pub async fn update_call(
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args: UpdateArgs = super::decode_args(arguments)?;
    require_participant(application_store, person_id, role, args.project_id).await?;
    let (due_at, due_on) = if args.due_at.is_some() || args.due_on.is_some() {
        let (at, on) = store::project_deadlines::resolve_due(
            application_store,
            args.project_id,
            DeadlineIdentity::Id(args.deadline_id),
            args.due_at.as_deref(),
            args.due_on,
        )
        .await
        .map_err(map_deadline)?;
        (Some(at), on)
    } else {
        (None, None)
    };
    let deadline = store::project_deadlines::update(
        application_store,
        args.project_id,
        args.deadline_id,
        &store::project_deadlines::DeadlinePatch {
            title: args.title.map(Some),
            description: args.description.map(Some),
            due_at,
            due_on,
            hearing: args.hearing,
            effort_minutes: None,
            authority_kind: args.authority_kind,
            authority: args.authority,
            source: args.source,
            source_document_id: args.source_document_id,
            owner_person_id: args.owner_person_id,
            calendar_role: args.calendar_role,
            category: args.category.map(Some),
            internal_lead_days: args.internal_lead_days,
            client_lead_days: args.client_lead_days,
        },
    )
    .await
    .map_err(map_deadline)?;
    Ok(row_result(
        &deadline,
        &format!("Updated deadline {}.", deadline.id),
    ))
}

pub async fn complete_call(
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    transition(
        application_store,
        person_id,
        role,
        arguments,
        Transition::Complete,
    )
    .await
}

pub async fn cancel_call(
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    transition(
        application_store,
        person_id,
        role,
        arguments,
        Transition::Cancel,
    )
    .await
}

pub async fn reopen_call(
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    transition(
        application_store,
        person_id,
        role,
        arguments,
        Transition::Reopen,
    )
    .await
}

#[derive(Clone, Copy)]
enum Transition {
    Complete,
    Cancel,
    Reopen,
}

async fn transition(
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
    which: Transition,
) -> Result<Value, ToolError> {
    let args: IdArgs = super::decode_args(arguments)?;
    require_participant(application_store, person_id, role, args.project_id).await?;
    let deadline = match which {
        Transition::Complete => {
            store::project_deadlines::complete(application_store, args.project_id, args.deadline_id)
                .await
        }
        Transition::Cancel => {
            store::project_deadlines::cancel(application_store, args.project_id, args.deadline_id)
                .await
        }
        Transition::Reopen => {
            store::project_deadlines::reopen(application_store, args.project_id, args.deadline_id)
                .await
        }
    }
    .map_err(map_deadline)?;
    Ok(row_result(
        &deadline,
        &format!("Deadline {} is {}.", deadline.id, deadline.status),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::persons::{NewPerson, Role};
    use uuid::Uuid;

    async fn project(application_store: &store::application_store::ApplicationStore) -> Uuid {
        crate::test_fixtures::seed_brands(application_store).await;
        store::projects::create(
            application_store,
            &store::projects::NewProject {
                code: format!("deadline-{}", Uuid::now_v7()),
                name: "Deadline matter".into(),
                status: "open".into(),
                entity_id: store::test_support::seed_entity(application_store).await,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn lawyer(
        application_store: &store::application_store::ApplicationStore,
        project_id: Option<Uuid>,
    ) -> Uuid {
        let person = store::persons::create(
            application_store,
            &NewPerson::with_role(
                "Synthetic Lawyer",
                format!("lawyer-{}@example.com", Uuid::now_v7()),
                Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        if let Some(project_id) = project_id {
            store::participation::add_participant(
                application_store,
                &store::participation::AddParticipantCommand {
                    project_id,
                    person_id: person.id,
                    dri: store::participation::DriRequest::Unchanged,
                    actor: store::participation::DriActor::System,
                },
            )
            .await
            .unwrap();
        }
        person.id
    }

    fn create_args(project_id: Uuid, authority: &str) -> Value {
        json!({
            "project_id": project_id,
            "due_at": "2026-11-02T17:00:00-08:00",
            "authority_kind": "contract",
            "authority": authority,
            "calendar_role": "firm",
            "category": "internal",
            "internal_lead_days": 14,
            "client_lead_days": 7
        })
    }

    #[tokio::test]
    async fn create_list_update_and_complete_round_trip() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let project_id = project(&application_store).await;
            let person_id = lawyer(&application_store, Some(project_id)).await;
            let created = create_call(
                &application_store,
                person_id,
                Role::Lawyer,
                &create_args(project_id, "Synthetic agreement § 1"),
            )
            .await
            .unwrap();
            let deadline_id = created["structuredContent"]["deadline"]["id"]
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(created["structuredContent"]["deadline"]["status"], "open");
            let listed = list_call(
                &application_store,
                person_id,
                Role::Lawyer,
                &json!({ "project_id": project_id }),
            )
            .await
            .unwrap();
            assert_eq!(listed["structuredContent"]["count"], 1);
            let updated = update_call(
                &application_store,
                person_id,
                Role::Lawyer,
                &json!({
                    "project_id": project_id,
                    "deadline_id": deadline_id,
                    "calendar_role": "opponent"
                }),
            )
            .await
            .unwrap();
            assert_eq!(
                updated["structuredContent"]["deadline"]["calendar_role"],
                "opponent"
            );
            let completed = complete_call(
                &application_store,
                person_id,
                Role::Lawyer,
                &json!({ "project_id": project_id, "deadline_id": deadline_id }),
            )
            .await
            .unwrap();
            assert_eq!(
                completed["structuredContent"]["deadline"]["status"],
                "complete"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_title_is_stored_and_changed() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let project_id = project(&application_store).await;
            let person_id = lawyer(&application_store, Some(project_id)).await;
            let mut args = create_args(project_id, "Synthetic agreement § 1");
            args["title"] = json!("Answer to the complaint");
            let created = create_call(&application_store, person_id, Role::Lawyer, &args)
                .await
                .unwrap();
            let deadline = &created["structuredContent"]["deadline"];
            assert_eq!(deadline["title"], "Answer to the complaint");
            let updated = update_call(
                &application_store,
                person_id,
                Role::Lawyer,
                &json!({
                    "project_id": project_id,
                    "deadline_id": deadline["id"],
                    "title": "Reply brief"
                }),
            )
            .await
            .unwrap();
            assert_eq!(
                updated["structuredContent"]["deadline"]["title"],
                "Reply brief"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_cancelled_deadline_can_be_reopened() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let project_id = project(&application_store).await;
            let person_id = lawyer(&application_store, Some(project_id)).await;
            let created = create_call(
                &application_store,
                person_id,
                Role::Lawyer,
                &create_args(project_id, "Synthetic agreement § 1"),
            )
            .await
            .unwrap();
            let deadline_id = created["structuredContent"]["deadline"]["id"].clone();
            let ids = json!({ "project_id": project_id, "deadline_id": deadline_id });
            let cancelled = cancel_call(&application_store, person_id, Role::Lawyer, &ids)
                .await
                .unwrap();
            assert_eq!(
                cancelled["structuredContent"]["deadline"]["status"],
                "cancelled"
            );
            let reopened = reopen_call(&application_store, person_id, Role::Lawyer, &ids)
                .await
                .unwrap();
            assert_eq!(reopened["structuredContent"]["deadline"]["status"], "open");
            assert_eq!(reopened["structuredContent"]["deadline"]["id"], deadline_id);
        })
        .await;
    }

    #[test]
    fn descriptors_point_cancelled_rows_at_reopen() {
        assert_eq!(reopen_descriptor()["name"], "reopen_deadline");
        for d in [update_descriptor(), cancel_descriptor()] {
            assert!(d["description"]
                .as_str()
                .unwrap()
                .contains("reopen_deadline"));
        }
    }

    #[tokio::test]
    async fn a_court_order_requires_review_even_with_a_citation() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let project_id = project(&application_store).await;
            let person_id = lawyer(&application_store, Some(project_id)).await;
            let mut args = create_args(project_id, "Synthetic order, page 1");
            args["authority_kind"] = json!("court_order");
            let error = create_call(&application_store, person_id, Role::Lawyer, &args)
                .await
                .unwrap_err();
            assert!(matches!(error, ToolError::InvalidArguments(reason) if reason.contains("queue_court_date")));
        })
        .await;
    }

    #[tokio::test]
    async fn a_non_participant_is_refused() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let project_id = project(&application_store).await;
            let person_id = lawyer(&application_store, None).await;
            let error = create_call(
                &application_store,
                person_id,
                Role::Lawyer,
                &create_args(project_id, "Synthetic agreement § 1"),
            )
            .await
            .unwrap_err();
            assert!(matches!(error, ToolError::NotFound(_)), "{error}");
        })
        .await;
    }
}
