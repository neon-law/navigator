//! `navigator legal-plans` — a Firm's Legal Plans (where each is logged into,
//! who to call, its service levels) — and `navigator project legal-plan`,
//! which links one matter to its plan by Plan Case Number. The plans are Firm
//! data held by the deployment, never by this repository
//! (`docs/legal-plans.md`).
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    projects::{manifest, surfaces},
    remote,
};

#[derive(Subcommand)]
pub(crate) enum LegalPlansCmd {
    /// The Firm's plans: portal, contact, and service levels.
    List,
    /// Add a plan to the Firm's vocabulary.
    Create {
        /// The plan's name, unique in the Firm, for example `ARAG`.
        #[arg(long)]
        name: String,
        /// Where the Firm logs in to accept cases and file claims.
        #[arg(long)]
        portal: Option<String>,
        /// The plan's provider-support contact: email, phone, hours.
        #[arg(long)]
        contact: Option<String>,
        /// Hours the Firm has to answer a case request.
        #[arg(long)]
        acceptance_sla_hours: i32,
        /// Days the Firm has to finish an accepted matter.
        #[arg(long)]
        turnaround_sla_days: i32,
        /// What the plan asks the Firm to report, and how often.
        #[arg(long)]
        reporting_requirements: Option<String>,
    },
}

/// The plan fields `navigator project legal-plan set` may change. An absent
/// flag keeps the value; an empty one (`--case-number ""`) clears it.
#[derive(Args, Debug, Default)]
pub(crate) struct PlanFields {
    /// One of the Firm's plan names, for example `ARAG`.
    #[arg(long)]
    pub plan: Option<String>,
    /// The plan's own number for this referral (the ARAG `CaseAssist`
    /// number, the `LegalEASE` member case), as the plan prints it.
    #[arg(long)]
    pub case_number: Option<String>,
    /// The plan's member identifier.
    #[arg(long)]
    pub member_id: Option<String>,
    /// The benefit code the plan assigned.
    #[arg(long)]
    pub benefit_code: Option<String>,
    /// `document_review`, `letter`, `uncontested_divorce`, or `court_appearance`.
    #[arg(long)]
    pub matter_type: Option<String>,
    /// Lowercase jurisdiction code, for example `nv`.
    #[arg(long)]
    pub state: Option<String>,
    /// When the plan's case request arrived (YYYY-MM-DD).
    #[arg(long)]
    pub assigned_on: Option<String>,
    /// When the Firm accepted the case (YYYY-MM-DD).
    #[arg(long)]
    pub accepted_on: Option<String>,
    /// When the matter closed for plan reporting (YYYY-MM-DD).
    #[arg(long)]
    pub closed_on: Option<String>,
}

impl PlanFields {
    /// The `PATCH` body: only the flags given, under the API's names.
    fn patch(&self) -> Map<String, Value> {
        [
            ("legal_plan", &self.plan),
            ("plan_case_number", &self.case_number),
            ("plan_member_id", &self.member_id),
            ("benefit_code", &self.benefit_code),
            ("plan_matter_type", &self.matter_type),
            ("plan_state", &self.state),
            ("plan_assigned_on", &self.assigned_on),
            ("plan_accepted_on", &self.accepted_on),
            ("plan_closed_on", &self.closed_on),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.as_ref().map(|v| (key.to_string(), v.clone().into())))
        .collect()
    }
}

async fn call(request: reqwest::RequestBuilder, action: &str) -> Result<Value> {
    let response = request
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("{action} legal plan"))?;
    let status = response.status();
    let body = response.text().await.context("read legal plan response")?;
    if !status.is_success() {
        return Err(anyhow!("legal plan {action} refused: {status} {body}"));
    }
    serde_json::from_str(&body).context("parse legal plan response")
}

fn finish(command: &str, result: Result<Value>) -> ExitCode {
    match result {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator {command}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) async fn run(firm: String, host: Option<String>, action: LegalPlansCmd) -> ExitCode {
    let result = async {
        let (base, token) = remote::resolve(host.as_deref())?;
        firm_command(&base, &token, &firm, action).await
    }
    .await;
    finish("legal-plans", result)
}

async fn firm_command(base: &str, token: &str, firm: &str, action: LegalPlansCmd) -> Result<Value> {
    let firm = match Uuid::parse_str(firm) {
        Ok(id) => id,
        Err(_) => remote::resolve_firm(base, token, firm, None)
            .await
            .context("resolve Firm by name (pass a Firm UUID if you lack directory access)")?,
    };
    let route = format!("{base}/app/api/firms/{firm}/legal-plans");
    let client = reqwest::Client::new();
    match action {
        LegalPlansCmd::List => call(client.get(route).bearer_auth(token), "list").await,
        LegalPlansCmd::Create {
            name,
            portal,
            contact,
            acceptance_sla_hours,
            turnaround_sla_days,
            reporting_requirements,
        } => {
            let body = serde_json::json!({
                "name": name,
                "portal": portal,
                "contact": contact,
                "acceptance_sla_hours": acceptance_sla_hours,
                "turnaround_sla_days": turnaround_sla_days,
                "reporting_requirements": reporting_requirements,
            });
            call(client.post(route).bearer_auth(token).json(&body), "create").await
        }
    }
}

/// The Project's API route, from `--project` or this checkout's
/// `navigator.yaml`.
async fn project_route(host: Option<&str>, project: Option<&str>) -> Result<(String, String)> {
    let local = manifest::read(Path::new("."));
    let project = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, token) = remote::resolve(host)?;
    let id = surfaces::resolve_project_id(&base, &token, project).await?;
    Ok((format!("{base}/app/api/projects/{id}/legal-plan"), token))
}

/// `navigator project legal-plan show`: the matter's plan, Plan Case
/// Number, and plan dates.
pub(crate) async fn show(host: Option<&str>, project: Option<&str>) -> ExitCode {
    let result = async {
        let (route, token) = project_route(host, project).await?;
        call(reqwest::Client::new().get(route).bearer_auth(token), "show").await
    }
    .await;
    finish("project legal-plan", result)
}

/// `navigator project legal-plan set`: change only the fields given.
pub(crate) async fn set(
    host: Option<&str>,
    project: Option<&str>,
    fields: &PlanFields,
) -> ExitCode {
    let result = async {
        let patch = fields.patch();
        if patch.is_empty() {
            return Err(anyhow!(
                "name at least one field to set, for example --case-number"
            ));
        }
        let (route, token) = project_route(host, project).await?;
        call(
            reqwest::Client::new()
                .patch(route)
                .bearer_auth(token)
                .json(&patch),
            "set",
        )
        .await
    }
    .await;
    finish("project legal-plan", result)
}

#[cfg(test)]
mod tests {
    use super::{finish, firm_command, LegalPlansCmd, PlanFields};
    use crate::{Cli, Command};
    use clap::Parser;
    use std::process::ExitCode;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_resolves_a_display_name_through_the_authorized_firm_directory() {
        let server = MockServer::start().await;
        let firm_id = uuid::Uuid::nil();
        Mock::given(method("GET"))
            .and(path("/app/api/firms"))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": firm_id, "name": "Example Practice PC"}
            ])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/firms/{firm_id}/legal-plans")))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let cli = Cli::try_parse_from([
            "navigator",
            "legal-plans",
            "--firm",
            "example practice pc",
            "list",
        ])
        .unwrap();
        let Some(Command::LegalPlans { firm, action, .. }) = cli.command else {
            panic!("expected legal-plans command");
        };
        let plans = firm_command(&server.uri(), "token", &firm, action)
            .await
            .unwrap();
        assert_eq!(plans, serde_json::json!([]));
    }

    #[test]
    fn the_patch_carries_only_the_flags_given_and_keeps_an_empty_one_to_clear() {
        let fields = PlanFields {
            plan: Some("ARAG".into()),
            case_number: Some("4900000-001".into()),
            closed_on: Some(String::new()),
            ..PlanFields::default()
        };
        assert_eq!(
            serde_json::Value::Object(fields.patch()),
            serde_json::json!({
                "legal_plan": "ARAG",
                "plan_case_number": "4900000-001",
                "plan_closed_on": "",
            })
        );
        assert!(PlanFields::default().patch().is_empty());
    }

    #[tokio::test]
    async fn create_posts_the_plan_to_the_firm_and_a_refusal_names_its_action() {
        let server = MockServer::start().await;
        let firm = uuid::Uuid::nil();
        Mock::given(method("GET"))
            .and(path("/app/api/firms"))
            .respond_with(ResponseTemplate::new(403))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/firms/{firm}/legal-plans")))
            .and(header("authorization", "Bearer token"))
            .and(body_json(serde_json::json!({
                "name": "Sample Panel Plan",
                "portal": "https://plan.example.com/account",
                "contact": null,
                "acceptance_sla_hours": 24,
                "turnaround_sla_days": 30,
                "reporting_requirements": null,
            })))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"name": "Sample Panel Plan"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/firms/{firm}/legal-plans")))
            .respond_with(ResponseTemplate::new(404).set_body_string("{}"))
            .mount(&server)
            .await;
        let created = firm_command(
            &server.uri(),
            "token",
            &firm.to_string(),
            LegalPlansCmd::Create {
                name: "Sample Panel Plan".into(),
                portal: Some("https://plan.example.com/account".into()),
                contact: None,
                acceptance_sla_hours: 24,
                turnaround_sla_days: 30,
                reporting_requirements: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(created["name"], "Sample Panel Plan");
        let refused = firm_command(
            &server.uri(),
            "token",
            &firm.to_string(),
            LegalPlansCmd::List,
        )
        .await
        .unwrap_err();
        assert!(refused.to_string().contains("legal plan list refused: 404"));
        assert_eq!(finish("legal-plans", Err(refused)), ExitCode::FAILURE);
    }
}
