//! Firm-authenticated completion intake and reconciliation.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{Duration, Utc};
use store::{
    docusign_completion::{self as journal, Outcome, Receipt},
    postgres::PostgresDb,
    projects::Project,
    signatures::{self, SignatureProvider::DocuSign, SignatureState},
};
use uuid::Uuid;

use crate::{
    esignature_webhook::ConnectPayload,
    signature::{DocuSignSignatureProvider, SignatureProvider},
    AppState,
};

pub(crate) async fn webhook(
    State(state): State<AppState>,
    Path(firm): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    let signature = headers
        .get("x-docusign-signature-1")
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let account = state
        .signature_provider
        .verify_firm_connect(&state.application_store, firm, signature, &body)
        .await
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let payload: ConnectPayload =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if payload.data.account_id != Some(account) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if !payload.is_completed() {
        if payload.reports_progress() {
            let envelope = payload
                .data
                .envelope_id
                .parse()
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            observe_progress(&state, firm, account, envelope)
                .await
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        }
        return progress(&state, firm, payload, &body).await;
    }
    let envelope = payload
        .data
        .envelope_id
        .parse()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let db = PostgresDb::from_env()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    journal::enqueue(&db, firm, account, &[envelope])
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::ACCEPTED)
}

async fn observe_progress(
    state: &AppState,
    firm: Uuid,
    account: Uuid,
    id: Uuid,
) -> anyhow::Result<()> {
    let db = &state.application_store;
    let provider = state
        .signature_provider
        .for_firm(db, firm)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Firm connection unavailable"))?;
    anyhow::ensure!(
        provider.account_id() == account.to_string(),
        "Firm connection changed"
    );
    if !unique_account(
        db,
        state.signature_provider.as_ref(),
        firm,
        provider.account_id(),
    )
    .await?
    {
        return Ok(());
    }
    let started = Utc::now();
    let envelope = provider.read_external_envelope(id).await?;
    let project_id =
        if let Some(signature) = signatures::by_provider(db, DocuSign, &id.to_string()).await? {
            let notation = store::notations::find_by_id(db, signature.notation_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Missing Notation"))?;
            notation.project_id
        } else {
            let (fields, names) = provider.envelope_project_hints(id).await?;
            let Ok(project) = identify_project(db, firm, &envelope, &fields, &names).await? else {
                return Ok(());
            };
            project.id
        };
    let pg = PostgresDb::from_env().await?;
    store::project_envelopes::observe(&pg, firm, account, project_id, started, &envelope).await?;
    Ok(())
}

async fn progress(
    state: &AppState,
    firm: Uuid,
    payload: ConnectPayload,
    body: &[u8],
) -> Result<StatusCode, StatusCode> {
    let db = &state.application_store;
    let unavailable = |_| StatusCode::SERVICE_UNAVAILABLE;
    let Some(signature) = signatures::by_provider(db, DocuSign, &payload.data.envelope_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    else {
        return Ok(StatusCode::OK);
    };
    let notation = store::notations::find_by_id(db, signature.notation_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let belongs_to_firm = store::projects::find_by_id(db, notation.project_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .is_some_and(|project| project.firm_id == Some(firm));
    if !belongs_to_firm {
        return Err(StatusCode::UNAUTHORIZED);
    }
    crate::esignature_webhook::process_known(state, payload, body)
        .await
        .map_err(unavailable)
}

pub(crate) fn spawn(state: &AppState) {
    if !state.signature_provider.supports_firm_connect() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut poll_at = tokio::time::Instant::now();
        loop {
            interval.tick().await;
            let Ok(db) = PostgresDb::from_env().await else {
                tracing::error!("DocuSign completion worker cannot open PostgreSQL");
                continue;
            };
            if tokio::time::Instant::now() >= poll_at {
                let deadline = std::time::Duration::from_secs(60);
                if tokio::time::timeout(deadline, poll_accounts(&state, &db))
                    .await
                    .is_err()
                {
                    tracing::warn!("DocuSign completion polling exceeded its time limit");
                }
                poll_at = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
            }
            if drain(&state, &db).await.is_err() {
                tracing::error!("DocuSign completion worker could not drain its journal");
            }
            if notify(&state, &db).await.is_err() {
                tracing::error!("DocuSign completion worker could not drain its notices");
            }
        }
    });
}

async fn poll_accounts(state: &AppState, db: &PostgresDb) {
    let Ok(firms) = store::firms::all(&state.application_store).await else {
        tracing::error!("DocuSign completion worker could not list Firms");
        return;
    };
    for firm in firms {
        let Ok(Some(provider)) = state
            .signature_provider
            .for_firm(&state.application_store, firm.id)
            .await
        else {
            continue;
        };
        if poll_account(db, firm.id, &provider).await.is_err() {
            tracing::warn!(firm_id = %firm.id, "DocuSign completion poll failed");
        }
    }
}

async fn poll_account(
    db: &PostgresDb,
    firm: Uuid,
    provider: &DocuSignSignatureProvider,
) -> anyhow::Result<()> {
    let account = provider.account_id().parse()?;
    let now = Utc::now();
    let cursor = journal::cursor(db, firm, account)
        .await?
        .unwrap_or(now - Duration::days(1));
    // Overlap the cutoff for completions the provider indexes late. A
    // missed poll resumes from the last slice whose IDs were journaled.
    let from = cursor - Duration::minutes(10);
    let through = (cursor + Duration::days(1)).min(now);
    let ids = provider.completed_envelopes(from, through).await?;
    journal::enqueue(db, firm, account, &ids).await?;
    journal::checkpoint(db, firm, account, through).await?;
    Ok(())
}

async fn drain(state: &AppState, db: &PostgresDb) -> anyhow::Result<()> {
    for _ in 0..30 {
        let mut tx = db.pool().begin().await?;
        let Some(receipt) = journal::claim(&mut tx).await? else {
            return Ok(());
        };
        let deadline = std::time::Duration::from_secs(45);
        let outcome = if let Ok(Ok(outcome)) =
            tokio::time::timeout(deadline, reconcile(state, &receipt)).await
        {
            outcome
        } else {
            tracing::warn!(firm_id = %receipt.firm_id, envelope_id = %receipt.envelope_id,
                "DocuSign filing will retry");
            Outcome::Retry
        };
        journal::finish(&mut tx, &receipt, outcome).await?;
        tx.commit().await?;
    }
    Ok(())
}

async fn notify(state: &AppState, db: &PostgresDb) -> anyhow::Result<()> {
    for _ in 0..20 {
        let mut tx = db.pool().begin().await?;
        let Some((firm, account, envelope, project)) = journal::claim_notice(&mut tx).await? else {
            return Ok(());
        };
        let sent = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            notify_project(state, firm, project),
        )
        .await
        .is_ok_and(|result| result.is_ok());
        journal::finish_notice(&mut tx, firm, account, envelope, sent).await?;
        tx.commit().await?;
    }
    Ok(())
}

async fn notify_project(state: &AppState, firm: Uuid, project: Uuid) -> anyhow::Result<()> {
    let row = store::projects::find_by_id(&state.application_store, project)
        .await?
        .filter(|row| row.firm_id == Some(firm))
        .ok_or_else(|| anyhow::anyhow!("Project is outside the Firm"))?;
    let slack = state
        .integration_providers
        .slack(&state.application_store, project)
        .await?;
    let name = workflows::project_channel_name(cloud::labels_slack_from_staging_env(), &row.code);
    let channel = slack
        .find_private_channel(&name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Private Project channel not found"))?;
    slack
        .post_message(&channel.id, "DocuSign completion filed.")
        .await?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn reconcile(state: &AppState, receipt: &Receipt) -> anyhow::Result<Outcome> {
    let db = &state.application_store;
    let provider = state
        .signature_provider
        .for_firm(db, receipt.firm_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Firm connection unavailable"))?;
    if provider.account_id() != receipt.account_id.to_string() {
        return Ok(Outcome::Review("account_changed"));
    }
    if !unique_account(
        db,
        state.signature_provider.as_ref(),
        receipt.firm_id,
        provider.account_id(),
    )
    .await?
    {
        return Ok(Outcome::Review("account_used_by_multiple_firms"));
    }
    let started = Utc::now();
    let envelope = provider.read_external_envelope(receipt.envelope_id).await?;
    if envelope.status != "completed" || envelope.completed_date_time.is_none() {
        return Ok(Outcome::Retry);
    }

    let envelope_key = receipt.envelope_id.to_string();
    if let Some(signature) = signatures::by_provider(db, DocuSign, &envelope_key).await? {
        let notation = store::notations::find_by_id(db, signature.notation_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Missing Notation"))?;
        let project = store::projects::find_by_id(db, notation.project_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Missing Project"))?;
        if project.firm_id != Some(receipt.firm_id)
            || receipt.project_id.is_some_and(|id| id != project.id)
        {
            return Ok(Outcome::Review("envelope_linked_elsewhere"));
        }
        store::project_envelopes::observe(
            &PostgresDb::from_env().await?,
            receipt.firm_id,
            receipt.account_id,
            project.id,
            started,
            &envelope,
        )
        .await?;
        crate::esignature_webhook::consume_receipt(
            state,
            &signature,
            SignatureState::Completed,
            envelope.completed_date_time,
        )
        .await?;
        return Ok(Outcome::Filed(project.id));
    }

    let (fields, names) = provider.envelope_project_hints(receipt.envelope_id).await?;
    let project = match identify_project(db, receipt.firm_id, &envelope, &fields, &names).await? {
        Ok(project) => project,
        Err(reason) => return Ok(Outcome::Review(reason)),
    };
    store::project_envelopes::observe(
        &PostgresDb::from_env().await?,
        receipt.firm_id,
        receipt.account_id,
        project.id,
        started,
        &envelope,
    )
    .await?;
    match receipt.project_id {
        None => return Ok(Outcome::Assigned(project.id)),
        Some(id) if id != project.id => return Ok(Outcome::Review("assigned_project_unavailable")),
        Some(_) => {}
    }
    let client = project
        .client_dri_person_id
        .ok_or_else(|| anyhow::anyhow!("Missing client DRI"))?;

    let lane = crate::signature_adoption::lock()
        .await
        .map_err(|_| anyhow::anyhow!("DocuSign adoption lock unavailable"))?;
    // Manual adoption may have won between discovery and taking the lane.
    if signatures::by_provider(db, DocuSign, &envelope_key)
        .await?
        .is_some()
    {
        return Ok(Outcome::Retry);
    }
    match onboarding_notation(db, &project, client, &names).await? {
        Onboarding::Notation(notation) => {
            crate::signature_adoption::reconcile(
                db,
                &state.storage,
                &provider,
                notation,
                &envelope,
            )
            .await
            .map_err(|_| anyhow::anyhow!("DocuSign adoption failed"))?;
        }
        Onboarding::OtherDocument => {
            let docs = provider
                .fetch_completed_documents(&crate::signature::SignatureRequestId(
                    envelope_key.clone(),
                ))
                .await?;
            crate::esignature_webhook::file_external_completion(
                db,
                &state.storage,
                project.id,
                &envelope,
                docs,
            )
            .await?;
        }
        Onboarding::Review => return Ok(Outcome::Review("onboarding_notation_unresolved")),
    }
    lane.commit().await?;
    Ok(Outcome::Filed(project.id))
}

pub(crate) async fn unique_account(
    db: &store::application_store::ApplicationStore,
    connections: &dyn SignatureProvider,
    firm: Uuid,
    account: &str,
) -> anyhow::Result<bool> {
    for other in store::firms::all(db).await? {
        if other.id == firm {
            continue;
        }
        if let Ok(Some(other)) = connections.for_firm(db, other.id).await {
            if other.account_id() == account {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub(crate) async fn identify_project(
    db: &store::application_store::ApplicationStore,
    firm: Uuid,
    envelope: &crate::signature::ExternalEnvelope,
    fields: &[String],
    names: &[String],
) -> anyhow::Result<Result<Project, &'static str>> {
    let mut projects = store::projects::all(db).await?;
    let roles = store::projects::all_participations(db).await?;
    resolve_designated_clients(&mut projects, roles);
    let mut signer_ids = Vec::new();
    for signer in &envelope.signers {
        if let Some(person) = store::persons::find_by_any_email(db, &signer.email).await? {
            signer_ids.push(person.id);
        }
    }
    Ok(match_project(firm, &projects, &signer_ids, fields, names).cloned())
}

fn resolve_designated_clients(
    projects: &mut [Project],
    roles: Vec<store::projects::PersonProjectRole>,
) {
    let mut designated = std::collections::HashMap::<_, Vec<_>>::new();
    for role in roles.into_iter().filter(|role| role.is_client_dri) {
        designated.entry(role.project_id).or_default().push(role);
    }
    for project in projects {
        project.client_dri_person_id = crate::api::project_client_dri(
            project.client_dri_person_id,
            designated.get(&project.id).map_or(&[], Vec::as_slice),
        );
    }
}

fn match_project<'a>(
    firm: Uuid,
    projects: &'a [Project],
    signers: &[Uuid],
    fields: &[String],
    names: &[String],
) -> Result<&'a Project, &'static str> {
    let filename_codes: Vec<_> = names
        .iter()
        .filter_map(|name| name.split_once("__"))
        .map(|(code, _)| code)
        .filter(|code| projects.iter().any(|p| p.code.eq_ignore_ascii_case(code)))
        .collect();
    let hints: Vec<_> = if fields.is_empty() {
        filename_codes
    } else {
        fields.iter().map(String::as_str).collect()
    };
    if let Some(first) = hints.first() {
        if hints.iter().any(|hint| !hint.eq_ignore_ascii_case(first)) {
            return Err("ambiguous_project_hint");
        }
    }
    let mut matches = projects.iter().filter(|project| {
        project.firm_id == Some(firm)
            && matches!(project.status.as_str(), "pitch" | "open")
            && match hints.first() {
                Some(code) => project.code.eq_ignore_ascii_case(code),
                None => project
                    .client_dri_person_id
                    .is_some_and(|id| signers.contains(&id)),
            }
    });
    let project = matches.next().ok_or("no_project_match")?;
    if matches.next().is_some() {
        return Err("ambiguous_project_match");
    }
    // A code disambiguates the Project. A signer still has to be its
    // client before the signed document can be shared with the Project.
    if !project
        .client_dri_person_id
        .is_some_and(|id| signers.contains(&id))
    {
        return Err("client_not_signer");
    }
    Ok(project)
}

enum Onboarding {
    Notation(Uuid),
    OtherDocument,
    Review,
}

async fn onboarding_notation(
    db: &store::application_store::ApplicationStore,
    project: &Project,
    client: Uuid,
    names: &[String],
) -> anyhow::Result<Onboarding> {
    // An unrelated agreement must not promote a pitch just because its
    // signer appears on the same Project.
    if !names.iter().any(|name| {
        let name = name.to_ascii_lowercase();
        name.starts_with("onboarding")
            || name.contains("__onboarding")
            || name.starts_with("engagement")
            || name.contains("__engagement")
    }) {
        return Ok(Onboarding::OtherDocument);
    }
    let mut selected = None;
    for notation in store::notations::list_by_project(db, project.id).await? {
        if notation.person_id != client {
            continue;
        }
        let template = store::templates::find_by_id(db, notation.template_id).await?;
        if !template.is_some_and(|template| {
            store::projects::template_opens_a_matter(template.kind.as_deref())
        }) {
            continue;
        }
        if signatures::latest_for_notation(db, notation.id)
            .await?
            .is_some()
        {
            continue;
        }
        if selected.replace(notation.id).is_some() {
            return Ok(Onboarding::Review);
        }
    }
    if let Some(notation) = selected {
        return Ok(Onboarding::Notation(notation));
    }
    let template = store::templates::resolve(db, Some(project.id), "onboarding__letter").await?;
    let Some(template) =
        template.filter(|t| store::projects::template_opens_a_matter(t.kind.as_deref()))
    else {
        return Ok(Onboarding::Review);
    };
    let new = store::notations::NewNotation::new(template.id, client, project.id, "END")
        .with_entity(project.entity_id);
    let notation = store::notations::create(db, &new).await?;
    Ok(Onboarding::Notation(notation.id))
}

#[cfg(test)]
mod tests {
    use super::match_project;
    use uuid::Uuid;

    #[test]
    fn signer_is_scoped_to_the_firm_and_a_project_code_disambiguates() {
        let firm = Uuid::new_v4();
        let client = Uuid::new_v4();
        let first: store::projects::Project = serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(),
            "code": "completion-example",
            "name": "Completion Example",
            "status": "open",
            "brand": "neon",
            "entity_id": Uuid::new_v4(),
            "firm_id": firm,
            "client_dri_person_id": client,
            "inserted_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();

        let mut second = first.clone();
        second.id = Uuid::new_v4();
        second.code = "second-example".into();
        second.firm_id = Some(Uuid::new_v4());
        let mut candidates = [first.clone(), second];
        assert_eq!(
            match_project(firm, &candidates, &[client], &[], &[])
                .unwrap()
                .id,
            first.id
        );

        candidates[1].firm_id = Some(firm);
        assert_eq!(
            match_project(firm, &candidates, &[client], &[], &[]).unwrap_err(),
            "ambiguous_project_match"
        );
        assert_eq!(
            match_project(
                firm,
                &candidates,
                &[client],
                std::slice::from_ref(&first.code),
                &[]
            )
            .unwrap()
            .id,
            first.id
        );
        assert_eq!(
            match_project(firm, &candidates, &[], &[first.code], &[]).unwrap_err(),
            "client_not_signer"
        );
    }

    /// A fresh open matter the webhook can attach a completed envelope to.
    fn open_project(firm: Uuid, client: Uuid, code: &str) -> store::projects::Project {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(),
            "code": code,
            "name": "Completion Example",
            "status": "open",
            "brand": "neon",
            "entity_id": Uuid::new_v4(),
            "firm_id": firm,
            "client_dri_person_id": client,
            "inserted_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn a_pitch_can_receive_its_first_completed_envelope() {
        let firm = Uuid::new_v4();
        let client = Uuid::new_v4();
        let mut project = open_project(firm, client, "new-pitch");
        project.status = "pitch".into();
        let matched = match_project(firm, std::slice::from_ref(&project), &[client], &[], &[]);
        assert_eq!(matched.unwrap().id, project.id);
    }

    #[test]
    fn differing_hints_refuse_rather_than_guessing() {
        let firm = Uuid::new_v4();
        let client = Uuid::new_v4();
        let candidates = [
            open_project(firm, client, "completion-example"),
            open_project(firm, client, "second-example"),
        ];
        assert_eq!(
            match_project(
                firm,
                &candidates,
                &[client],
                &["completion-example".into(), "second-example".into()],
                &[]
            )
            .unwrap_err(),
            "ambiguous_project_hint"
        );
    }

    #[test]
    fn an_unpinned_client_designation_matches_the_signer_only_on_its_own_project() {
        let firm = Uuid::new_v4();
        let client = Uuid::new_v4();
        let mut projects = [
            open_project(firm, client, "first"),
            open_project(firm, client, "second"),
        ];
        projects[0].client_dri_person_id = None;
        let role = |project_id, person_id| store::projects::PersonProjectRole {
            id: Uuid::new_v4(),
            project_id,
            person_id,
            participation: "client".into(),
            is_client_dri: true,
            is_lawyer_dri: false,
            inserted_at: String::new(),
            updated_at: String::new(),
        };
        let first = projects[0].id;
        let second = projects[1].id;
        super::resolve_designated_clients(
            &mut projects,
            vec![role(first, client), role(second, Uuid::new_v4())],
        );
        assert_eq!(
            match_project(firm, &projects, &[client], &[], &[])
                .unwrap()
                .id,
            first
        );
        projects[0].client_dri_person_id = None;
        super::resolve_designated_clients(
            &mut projects,
            vec![role(first, client), role(first, Uuid::new_v4())],
        );
        assert!(match_project(firm, &projects, &[client], &[], &[]).is_err());
    }

    #[test]
    fn a_filename_code_drives_the_match_when_no_field_hint_exists() {
        let firm = Uuid::new_v4();
        let client = Uuid::new_v4();
        let first = open_project(firm, client, "completion-example");
        let second = open_project(firm, client, "second-example");
        let first_id = first.id;
        // Both projects share the signer as their client, so only the
        // filename's code can pick one.
        assert_eq!(
            match_project(
                firm,
                &[first, second],
                &[client],
                &[],
                &["completion-example__nda.pdf".into()]
            )
            .unwrap()
            .id,
            first_id
        );
    }

    #[test]
    fn an_unknown_code_or_a_closed_matter_matches_nothing() {
        let firm = Uuid::new_v4();
        let client = Uuid::new_v4();
        let mut project = open_project(firm, client, "completion-example");
        assert_eq!(
            match_project(
                firm,
                &[project.clone()],
                &[client],
                &["ghost-project".into()],
                &[]
            )
            .unwrap_err(),
            "no_project_match"
        );
        project.status = "closed".into();
        // Even the signer match is scoped to open matters.
        assert_eq!(
            match_project(firm, &[project], &[client], &[], &[]).unwrap_err(),
            "no_project_match"
        );
    }

    #[test]
    fn a_hint_matches_case_insensitively_and_beats_the_signer_ambiguity() {
        let firm = Uuid::new_v4();
        let client = Uuid::new_v4();
        let first = open_project(firm, client, "completion-example");
        let second = open_project(firm, client, "second-example");
        assert_eq!(
            match_project(
                firm,
                &[first, second],
                &[client],
                &["COMPLETION-EXAMPLE".into()],
                &[]
            )
            .unwrap()
            .code,
            "completion-example"
        );
    }
}
