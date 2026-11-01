//! DBOS entry point for the nightly Project sweep.
//!
//! The existing tag operations remain the business logic. DBOS checkpoints the
//! roster, each (date, Project, tag) operation, and both Slack notices so a
//! worker restart resumes the same daily run.

use std::sync::Arc;

use chrono::Utc;
use dbos::{StepOptions, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::surreal::SurrealDb;
use uuid::Uuid;
use workflows::SlackBot;

use crate::nightly_project_update::{self as nightly, NightlyProjectUpdateReport, Tag, TagOutcome};

pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED";
const WORKFLOW: &str = "NightlyProjectUpdate";
const MESSAGE_ATTEMPTS: u32 = 3;

pub type NightlyWorkflow = WorkflowRef<(), NightlyProjectUpdateReport, NightlyError>;
pub type TagWorkflow =
    WorkflowRef<crate::nightly_project_update::ProjectTagRequest, TagOutcome, NightlyError>;

#[derive(Clone)]
pub struct Providers {
    pub github_forge: Option<Arc<dyn cloud::forge::ForgeService>>,
    pub repository_forge: crate::nightly_project_update::repository::RepositoryForge,
    pub runtime_version: String,
    pub portal_origin: Option<String>,
}

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
pub enum NightlyError {
    #[error("nightly Project roster could not be read")]
    Roster,
    #[error("nightly Project update notice could not be delivered")]
    Notice,
    #[error("nightly Project workflow step failed")]
    Step,
}

#[derive(Clone)]
pub struct Context {
    surreal: SurrealDb,
    slack: Arc<dyn SlackBot>,
    outcome_channel: String,
    simulated: bool,
    github_forge: Option<Arc<dyn cloud::forge::ForgeService>>,
    repository_forge: crate::nightly_project_update::repository::RepositoryForge,
    runtime_version: String,
    portal_origin: Option<String>,
}

impl Context {
    #[must_use]
    pub fn new(
        surreal: SurrealDb,
        slack: Arc<dyn SlackBot>,
        outcome_channel: String,
        simulated: bool,
        providers: Providers,
    ) -> Self {
        Self {
            surreal,
            slack,
            outcome_channel,
            simulated,
            github_forge: providers.github_forge,
            repository_forge: providers.repository_forge,
            runtime_version: providers.runtime_version,
            portal_origin: providers.portal_origin,
        }
    }
}

pub fn enabled(get: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match get(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED must be true or false"),
    }
}

/// The UTC date suffix is retained from the Restate invocation key while the
/// workflow name is added because DBOS ids share a global namespace.
#[must_use]
pub fn workflow_id(date: &str) -> String {
    format!("{WORKFLOW}:{date}")
}

#[must_use]
pub fn child_workflow_id(tag: Tag, ran_on: &str, project_id: Uuid) -> String {
    format!("{}:{}:{project_id}", tag.service(), ran_on)
}

#[derive(Clone)]
struct ParentContext {
    context: Context,
    membership: TagWorkflow,
    slack: TagWorkflow,
    repository: TagWorkflow,
}

#[allow(clippy::too_many_lines)] // One factory keeps all four workflow registrations coherent.
pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<NightlyWorkflow> {
    let membership = register_tag(dbos, context.clone(), Tag::Membership)?;
    let slack = register_tag(dbos, context.clone(), Tag::Slack)?;
    let repository = register_tag(dbos, context.clone(), Tag::Repository)?;
    let parent = ParentContext {
        context,
        membership,
        slack,
        repository,
    };
    dbos.register_workflow(WORKFLOW, move |()| {
        let parent = parent.clone();
        async move {
            let workflow_id = dbos::workflow_id().unwrap_or_default();
            let Some(ran_on) = workflow_id.strip_prefix("NightlyProjectUpdate:") else {
                return Err(dbos::Error::Application(NightlyError::Step));
            };
            chrono::NaiveDate::parse_from_str(ran_on, "%Y-%m-%d")
                .map_err(|_| dbos::Error::Application(NightlyError::Step))?;

            let started_at = dbos::step("read-clock", || async {
                Ok::<_, dbos::Error<NightlyError>>(Utc::now().to_rfc3339())
            })
            .await?;
            tracing::info!(%ran_on, %started_at, "DBOS Nightly Project Update started");

            let surreal = parent.context.surreal.clone();
            let roster = dbos::step("discover-projects", move || {
                let surreal = surreal.clone();
                async move {
                    nightly::discover_roster(&surreal)
                        .await
                        .map_err(|_| dbos::Error::Application(NightlyError::Roster))
                }
            })
            .await?;

            // Folder convergence precedes Slack posting and cannot be starved by it.
            store::project_folder_reconcile::visit_nightly_roster(
                roster.iter().filter(|project| project.open).map(|project| project.project_id).collect::<Vec<_>>(),
                |project_id| {
                    let surreal = parent.context.surreal.clone();
                    async move {
                        dbos::step(&format!("converge-project-folders-{project_id}"), move || {
                            let surreal = surreal.clone();
                            async move { Ok::<_, dbos::Error<NightlyError>>(
                                store::project_folder_reconcile::nightly_from_env(&surreal, project_id).await,
                            ) }
                        }).await
                    }
                },
            ).await?;

            let slack = Arc::clone(&parent.context.slack);
            let channel = parent.context.outcome_channel.clone();
            let correlation = format!("nightly-project-update-{ran_on}-start");
            dbos::step_with(
                "post-start",
                StepOptions {
                    max_attempts: MESSAGE_ATTEMPTS,
                    ..StepOptions::default()
                },
                move || {
                    let slack = Arc::clone(&slack);
                    let channel = channel.clone();
                    let correlation = correlation.clone();
                    async move {
                        post_once(
                            slack.as_ref(),
                            &channel,
                            &correlation,
                            "*Nightly Project Update beginning*\nRepairing Slack channels, reconciling internal membership, and checking repositories.",
                        )
                        .await
                        .map_err(dbos::Error::Application)
                    }
                },
            )
            .await?;

            let report = nightly::sweep(&roster, |tag, project_id| {
                let parent = parent.clone();
                let ran_on = ran_on.to_owned();
                async move {
                    let child = match tag {
                        Tag::Membership => &parent.membership,
                        Tag::Slack => &parent.slack,
                        Tag::Repository => &parent.repository,
                    };
                    let child_id = child_workflow_id(tag, &ran_on, project_id);
                    let request = crate::nightly_project_update::ProjectTagRequest {
                        ran_on,
                        project_id,
                    };
                    child
                        .run_with(
                            request,
                            dbos::RunOptions {
                                workflow_id: Some(&child_id),
                                ..Default::default()
                            },
                        )
                        .await
                        .map_err(|_| {
                            restate_sdk::prelude::TerminalError::new("DBOS tag child failed")
                        })
                }
            })
            .await;

            let message = nightly::render_report(&report);
            let slack = Arc::clone(&parent.context.slack);
            let channel = parent.context.outcome_channel.clone();
            let correlation = format!("nightly-project-update-{ran_on}-complete");
            dbos::step_with(
                "post-outcome",
                StepOptions {
                    max_attempts: MESSAGE_ATTEMPTS,
                    ..StepOptions::default()
                },
                move || {
                    let slack = Arc::clone(&slack);
                    let channel = channel.clone();
                    let correlation = correlation.clone();
                    let message = message.clone();
                    async move {
                        post_once(slack.as_ref(), &channel, &correlation, &message)
                            .await
                            .map_err(dbos::Error::Application)
                    }
                },
            )
            .await?;
            dbos::Result::<_, NightlyError>::Ok(report)
        }
    })
}

#[allow(clippy::too_many_lines)] // The three fixed tag definitions share one typed registration path.
fn register_tag(dbos: &DBOS, context: Context, tag: Tag) -> dbos::Result<TagWorkflow> {
    dbos.register_workflow(
        tag.service(),
        move |request: crate::nightly_project_update::ProjectTagRequest| {
            let context = context.clone();
            async move {
                chrono::NaiveDate::parse_from_str(&request.ran_on, "%Y-%m-%d")
                    .map_err(|_| dbos::Error::Application(NightlyError::Step))?;
                let project_id = request.project_id;
                let ran_on = request.ran_on;
                match tag {
                    Tag::Membership => {
                        let resolution_context = context.clone();
                        let resolution = dbos::step("resolve-membership-channel", move || {
                            let context = resolution_context.clone();
                            async move {
                                crate::nightly_project_update::membership::resolve_channel(
                                    &context.surreal,
                                    &context.slack,
                                    context.simulated,
                                    project_id,
                                )
                                .await
                                .map_err(|_| dbos::Error::Application(NightlyError::Step))
                            }
                        })
                        .await?;
                        let mut report = resolution.outcome;
                        if resolution.open {
                            if let Some(channel_id) = resolution.channel_id {
                                let invite_context = context.clone();
                                let invited = dbos::step("invite-missing-members", move || {
                                    let context = invite_context.clone();
                                    let channel_id = channel_id.clone();
                                    async move {
                                        crate::nightly_project_update::membership::invite_members(
                                            &context.surreal,
                                            &context.slack,
                                            project_id,
                                            &channel_id,
                                        )
                                        .await
                                        .map_err(|_| dbos::Error::Application(NightlyError::Step))
                                    }
                                })
                                .await?;
                                report.channels += invited.channels;
                                report.invited += invited.invited;
                                report.already_members += invited.already_members;
                                report.unmatched_assignments += invited.unmatched_assignments;
                            } else {
                                report.projects_without_channel += 1;
                            }
                        }
                        Ok(TagOutcome::Membership(report))
                    }
                    Tag::Slack => {
                        let canvas_context = context.clone();
                        let canvas_ran_on = ran_on.clone();
                        let canvas = dbos::step("upsert-canvas", move || {
                            let context = canvas_context.clone();
                            let ran_on = canvas_ran_on.clone();
                            async move {
                                crate::nightly_project_update::slack::upsert_canvas(
                                    &context.surreal,
                                    context.slack.as_ref(),
                                    context.portal_origin.as_deref(),
                                    &ran_on,
                                    project_id,
                                )
                                .await
                                .map_err(|_| dbos::Error::Application(NightlyError::Step))
                            }
                        })
                        .await;
                        let notice_context = context.clone();
                        let notice_ran_on = ran_on.clone();
                        let posted = dbos::step("post-notice", move || {
                            let context = notice_context.clone();
                            let ran_on = notice_ran_on.clone();
                            async move {
                                crate::nightly_project_update::slack::post_notice(
                                    &context.surreal,
                                    context.slack.as_ref(),
                                    context.github_forge.as_deref(),
                                    &ran_on,
                                    project_id,
                                )
                                .await
                                .map_err(|_| dbos::Error::Application(NightlyError::Step))
                            }
                        })
                        .await?;
                        let (canvases_created, canvases_edited, canvas_failures) = match canvas {
                            Ok(crate::nightly_project_update::slack::CanvasStep::Created) => {
                                (1, 0, 0)
                            }
                            Ok(crate::nightly_project_update::slack::CanvasStep::Edited) => {
                                (0, 1, 0)
                            }
                            Ok(crate::nightly_project_update::slack::CanvasStep::Skipped) => {
                                (0, 0, 0)
                            }
                            Err(_) => (0, 0, 1),
                        };
                        Ok(TagOutcome::Slack(
                            crate::nightly_project_update::slack::SlackOutcome {
                                canvases_created,
                                canvases_edited,
                                notices_posted: usize::from(posted),
                                canvas_failures,
                            },
                        ))
                    }
                    Tag::Repository => {
                        let context = context.clone();
                        dbos::step("reconcile-repository", move || {
                            let context = context.clone();
                            async move {
                                crate::nightly_project_update::repository::reconcile_repository(
                                    &context.surreal,
                                    &context.repository_forge,
                                    project_id,
                                    &context.runtime_version,
                                )
                                .await
                                .map(TagOutcome::Repository)
                                .map_err(|_| dbos::Error::Application(NightlyError::Step))
                            }
                        })
                        .await
                    }
                }
            }
        },
    )
}

async fn post_once(
    slack: &dyn SlackBot,
    channel: &str,
    correlation: &str,
    message: &str,
) -> Result<(), NightlyError> {
    if slack
        .message_receipt_by_correlation(channel, correlation)
        .await
        .map_err(|_| NightlyError::Notice)?
        .is_none()
    {
        slack
            .post_message_with_receipt(channel, message, Some(correlation))
            .await
            .map_err(|_| NightlyError::Notice)?;
    }
    Ok(())
}

/// A registered parent workflow on a DBOS instance that never launches, so a
/// start fails before reaching PostgreSQL. For route tests only.
#[cfg(test)]
pub(crate) async fn unlaunched_workflow() -> NightlyWorkflow {
    let dbos = DBOS::new(workflows::dbos_engine::config(
        "postgres://localhost/navigator?sslmode=disable",
        "test",
    ));
    register(
        &dbos,
        Context::new(
            store::test_support::mem_surreal().await,
            Arc::new(workflows::CapturingSlackBot::new()),
            "C_STAGING_MESSAGES".to_owned(),
            true,
            Providers {
                github_forge: None,
                repository_forge: crate::nightly_project_update::repository::RepositoryForge::None,
                runtime_version: "test".to_owned(),
                portal_origin: None,
            },
        ),
    )
    .expect("register the parent and its three tag workflows")
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;
    use workflows::{
        CapturingSlackBot, SlackBot, SlackBotError, SlackChannel, SlackMessageReceipt,
    };

    use super::{
        child_workflow_id, enabled, post_once, unlaunched_workflow, workflow_id, NightlyError,
    };
    use crate::nightly_project_update::Tag;

    /// Slack that refuses either the receipt lookup or the post.
    struct Refusing {
        lookup_fails: bool,
    }

    #[async_trait::async_trait]
    impl SlackBot for Refusing {
        async fn create_private_channel(&self, _: &str) -> Result<SlackChannel, SlackBotError> {
            Err(SlackBotError::Api("not_used".into()))
        }
        async fn post_message(&self, _: &str, _: &str) -> Result<(), SlackBotError> {
            Err(SlackBotError::Api("channel_not_found".into()))
        }
        async fn user_id_by_email(&self, _: &str) -> Result<Option<String>, SlackBotError> {
            Ok(None)
        }
        async fn channel_member_ids(&self, _: &str) -> Result<Vec<String>, SlackBotError> {
            Ok(Vec::new())
        }
        async fn invite_to_channel(&self, _: &str, _: &[String]) -> Result<(), SlackBotError> {
            Ok(())
        }
        async fn message_receipt_by_correlation(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
            if self.lookup_fails {
                Err(SlackBotError::Transport("synthetic".into()))
            } else {
                Ok(None)
            }
        }
    }

    #[test]
    fn parent_and_child_ids_are_namespaced_by_utc_date_and_project() {
        assert_eq!(workflow_id("2026-10-22"), "NightlyProjectUpdate:2026-10-22");
        let project = Uuid::nil();
        let ids: Vec<String> = Tag::ALL
            .iter()
            .map(|tag| child_workflow_id(*tag, "2026-10-22", project))
            .collect();
        for (tag, id) in Tag::ALL.iter().zip(&ids) {
            assert_eq!(
                id,
                &format!("{}:2026-10-22:{project}", tag.service()),
                "each tag keeps its own DBOS id"
            );
        }
        assert_eq!(
            ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            Tag::ALL.len()
        );
    }

    #[test]
    fn the_switch_accepts_only_true_or_false() {
        assert!(!enabled(|_| None).unwrap());
        assert!(!enabled(|_| Some(" false ".into())).unwrap());
        assert!(enabled(|_| Some("true".into())).unwrap());
        assert_eq!(
            enabled(|_| Some("on".into())),
            Err("NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED must be true or false")
        );
    }

    #[tokio::test]
    async fn a_correlated_notice_is_posted_once_across_retries() {
        let slack = CapturingSlackBot::new();
        for _ in 0..2 {
            post_once(
                &slack,
                "C_MESSAGES",
                "nightly-project-update-2026-10-22-start",
                "begun",
            )
            .await
            .expect("post or find the receipt");
        }
        assert_eq!(
            slack.posted_messages(),
            vec![("C_MESSAGES".to_owned(), "begun".to_owned())]
        );
    }

    #[tokio::test]
    async fn an_unreadable_receipt_or_refused_post_is_a_notice_failure() {
        for lookup_fails in [true, false] {
            let refused = post_once(&Refusing { lookup_fails }, "C", "corr", "text").await;
            assert!(matches!(refused, Err(NightlyError::Notice)));
        }
        assert_eq!(
            NightlyError::Notice.to_string(),
            "nightly Project update notice could not be delivered"
        );
    }

    #[tokio::test]
    async fn an_unlaunched_executor_refuses_to_start_the_parent() {
        let workflow = unlaunched_workflow().await;
        let started = workflow
            .start_with(
                (),
                dbos::StartOptions {
                    workflow_id: Some("NightlyProjectUpdate:2026-10-22"),
                    ..Default::default()
                },
            )
            .await;
        assert!(started.is_err());
    }
}
