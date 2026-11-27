//! Open the Project's own engagement questionnaire and prepare reviewed PDFs for signature.
use store::{notations::SigningDelivery, projects::Project};
use uuid::Uuid;
use workflows::dbos_engine::plan_retainers::{Outcome, Source};

pub(crate) struct PlanRetainers(pub crate::AppState);

pub(crate) fn eligible(project: &Project) -> bool {
    matches!(project.status.as_str(), "pitch" | "open")
        && project.plan_closed_on.is_none()
        && project.firm_id.is_some()
        && (project.legal_plan_id.is_some() || project.plan_case_number.is_some())
}

#[async_trait::async_trait]
impl Source for PlanRetainers {
    async fn projects(&self) -> anyhow::Result<Vec<Uuid>> {
        let db = &self.0.application_store;
        let firm = store::firms::anchor_firm(db).await?.ok_or_else(|| {
            anyhow::anyhow!("retainer reconciliation requires the installation's Firm")
        })?;
        let projects = store::projects::all(db).await?;
        Ok(projects
            .iter()
            .filter(|p| p.firm_id == Some(firm.id) && eligible(p))
            .map(|p| p.id)
            .collect())
    }

    async fn reconcile(&self, id: Uuid) -> anyhow::Result<Outcome> {
        let state = &self.0;
        let db = &state.application_store;
        let pg = store::postgres::PostgresDb::application().await?;
        let lane = store::notation_machine_state::acquire_lane(&pg, id).await?;
        let Some(project) = store::projects::find_by_id(db, id).await?.filter(eligible) else {
            return Ok(Outcome::ClosedOrUnlinked);
        };
        if store::firms::anchor_firm(db)
            .await?
            .is_none_or(|firm| project.firm_id != Some(firm.id))
        {
            return Ok(Outcome::ClosedOrUnlinked);
        }
        if store::onboarding::official_projects(&pg, &[id])
            .await?
            .contains(&id)
        {
            return Ok(Outcome::Executed);
        }
        if let Some(request) = store::onboarding::pending_esign(&pg, &[id]).await?.get(&id) {
            return Ok(if request.state == "requested" {
                Outcome::ExistingEnvelope
            } else {
                Outcome::NeedsReview
            });
        }
        let participants = store::projects::participations_for_project(db, id).await?;
        let Some(client) =
            crate::api::project_client_dri(project.client_dri_person_id, &participants)
        else {
            return Ok(Outcome::NeedsClient);
        };
        let existing = engagement_notations(db, id).await?;
        let outcome = match existing.as_slice() {
            [] => {
                let drafts = store::templates::list_for_project(db, id).await?;
                let drafts: Vec<_> = drafts
                    .iter()
                    .filter(|t| {
                        store::projects::template_opens_a_matter(t.kind.as_deref())
                            && t.source_commit_sha.is_some()
                    })
                    .collect();
                let [template] = drafts.as_slice() else {
                    return Ok(Outcome::NeedsDraft);
                };
                let body = store::templates::body(db, &state.storage, template).await?;
                if project_scaffold::is_unedited_stub(&body) {
                    return Ok(Outcome::NeedsDraft);
                }
                workflows::start_notation_pinned_with_delivery(
                    db,
                    state.workflow_runtime.as_ref(),
                    Some(&state.storage),
                    workflows::StartPinnedNotation {
                        template,
                        person_id: client,
                        project_id: id,
                        entity_id: Some(project.entity_id),
                    },
                    SigningDelivery::Emailed,
                )
                .await?;
                Outcome::QuestionnaireOpened
            }
            [row]
                if row.person_id == client
                    && (row.state.starts_with("generate_pdf") || row.state == "lawyer_review") =>
            {
                let Some(provider) = state.signature_provider.for_project(db, id).await? else {
                    return Ok(Outcome::NeedsDocuSign);
                };
                let ctx = crate::signature_flow::SendFlowContext {
                    application_store: db,
                    runtime: state.workflow_runtime.as_ref(),
                    storage: &state.storage,
                    acting: None,
                };
                match crate::signature_flow::send_notation_for_signature(
                    &ctx,
                    row.id,
                    &provider,
                    crate::signature_flow::SendMode::Prepare,
                )
                .await
                {
                    Ok(_) => Outcome::EnvelopePrepared,
                    Err(crate::retainer_walk::WorkflowDriveError::DocumentNotReady(_)) => {
                        Outcome::NeedsReview
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            _ => Outcome::NeedsReview,
        };
        lane.commit().await?;
        Ok(outcome)
    }
}

async fn engagement_notations(
    db: &store::application_store::ApplicationStore,
    id: Uuid,
) -> anyhow::Result<Vec<store::notations::Notation>> {
    let mut existing = Vec::new();
    for row in store::notations::list_by_project(db, id).await? {
        let template = store::templates::find_by_id(db, row.template_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("notation template unavailable"))?;
        if store::projects::template_opens_a_matter(template.kind.as_deref()) {
            existing.push(row);
        }
    }
    Ok(existing)
}
