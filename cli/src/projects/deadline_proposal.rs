//! Calendar-day proposals stay separate from the stored Project Deadline.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, FixedOffset, NaiveDate};
use clap::Args;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Args)]
pub struct ProposalArgs {
    /// Private JSON rule: `trigger`, `offset_days`, `basis`, `authority_kind`, `authority_id`, `category`.
    #[arg(long)]
    pub rule: PathBuf,
    /// Date of the event named by the rule (YYYY-MM-DD).
    #[arg(long)]
    pub trigger_on: NaiveDate,
    /// What is due if the lawyer confirms the proposal.
    #[arg(long)]
    pub title: String,
    /// Stable obligation key, retained when the trigger changes.
    #[arg(long)]
    pub replay_key: String,
    /// Lawyer confirmation: store the reviewed date through the Deadline API.
    #[arg(long)]
    pub confirm: bool,
    /// Lawyer-supplied due instant with an explicit offset; requires confirmation.
    #[arg(long, requires = "confirm")]
    pub due_at: Option<String>,
    /// Lawyer-counted date for court-day or holiday rules; requires confirmation.
    #[arg(long, requires = "confirm")]
    pub due_on: Option<NaiveDate>,
}

#[derive(Clone, Copy, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Basis {
    CalendarDays,
    CourtDays,
    Holidays,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    trigger: String,
    offset_days: i64,
    basis: Basis,
    authority_kind: String,
    authority_id: Uuid,
    /// What kind of date this proposal produces. A rule names it because
    /// `court` and `discovery` deadlines share authority kinds and are never
    /// guessed; creation is refused without one unless the calendar role or
    /// an internal authority settles it.
    category: Option<String>,
}

#[derive(Deserialize)]
struct FiledAuthority {
    id: Uuid,
    citation: String,
}

pub async fn propose(
    host: Option<&str>,
    project: Option<&str>,
    args: ProposalArgs,
) -> std::process::ExitCode {
    super::deadline::finish(
        async {
            let bytes = std::fs::read(&args.rule).context("read private proposal rule")?;
            let rule = serde_json::from_slice(&bytes).context("parse proposal rule")?;
            let (base, token, project_id) = if args.confirm {
                let (base, token, id) = super::deadline::coordinates(host, project).await?;
                (base, token, Some(id))
            } else {
                let manifest = super::manifest::read(Path::new("."));
                let host =
                    host.or_else(|| manifest.as_ref().and_then(|value| value.host.as_deref()));
                let (base, token) = crate::remote::resolve(host)?;
                (base, token, None)
            };
            execute(&base, &token, project_id, &args, &rule).await
        }
        .await,
    )
}

async fn execute(
    base: &str,
    token: &str,
    project: Option<Uuid>,
    args: &ProposalArgs,
    rule: &Rule,
) -> Result<Value> {
    if rule.trigger.trim().is_empty()
        || args.title.trim().is_empty()
        || args.replay_key.trim().is_empty()
    {
        return Err(anyhow!("trigger, title, and replay_key must be nonempty"));
    }
    if ![
        "statute",
        "court_rule",
        "court_order",
        "contract",
        "internal",
    ]
    .contains(&rule.authority_kind.as_str())
    {
        return Err(anyhow!("unrecognized authority_kind"));
    }
    let due_on = match rule.basis {
        Basis::CalendarDays => Some(
            args.trigger_on
                .checked_add_signed(
                    chrono::Duration::try_days(rule.offset_days)
                        .ok_or_else(|| anyhow!("calendar-day offset is out of range"))?,
                )
                .ok_or_else(|| anyhow!("proposed date is out of range"))?,
        ),
        Basis::CourtDays | Basis::Holidays => None,
    };
    let client = reqwest::Client::new();
    let authority: FiledAuthority = serde_json::from_value(
        super::deadline::call(
            client
                .get(format!("{base}/app/api/authorities/{}", rule.authority_id))
                .bearer_auth(token),
            "read authority",
        )
        .await?,
    )?;
    if authority.id != rule.authority_id || authority.citation.trim().is_empty() {
        return Err(anyhow!("filed Authority metadata does not match the rule"));
    }
    let mut proposal = json!({"status":"proposal", "trigger":rule.trigger,
        "trigger_on":args.trigger_on, "offset_days":rule.offset_days, "basis":rule.basis,
        "due_on":due_on, "manual_due_on_required":due_on.is_none(),
        "authority_kind":rule.authority_kind, "authority_id":authority.id,
        "citation":authority.citation, "replay_key":args.replay_key});
    if let Some(category) = &rule.category {
        proposal["category"] = json!(category);
    }
    if !args.confirm {
        return Ok(proposal);
    }
    let chosen = due_on
        .or(args.due_on)
        .ok_or_else(|| anyhow!("this counting basis requires lawyer-entered --due-on"))?;
    if args.due_on.is_some_and(|manual| manual != chosen) {
        return Err(anyhow!("--due-on cannot override a calendar-day proposal"));
    }
    let due_at = args
        .due_at
        .as_deref()
        .ok_or_else(|| anyhow!("confirmation requires --due-at with an explicit offset"))?;
    let instant = DateTime::<FixedOffset>::parse_from_rfc3339(due_at)
        .context("--due-at must include an explicit offset")?;
    if instant.date_naive() != chosen {
        return Err(anyhow!("--due-at must match the reviewed due_on"));
    }
    let project = project.ok_or_else(|| anyhow!("confirmation requires a Project"))?;
    proposal["reviewed_due_on"] = json!(chosen);
    save(base, token, project, args, due_at, &authority, proposal).await
}

#[derive(Deserialize)]
struct ExistingDeadline {
    id: Uuid,
    replay_key: Option<String>,
    status: String,
    description: Option<String>,
}

fn audit_description(existing: Option<&str>, proposal: Value) -> String {
    let mut audit = existing
        .and_then(|description| serde_json::from_str::<Value>(description).ok())
        .filter(|value| value.is_object() && value.get("deadline_proposal").is_some())
        .unwrap_or_else(|| match existing {
            Some(notes) => json!({"notes": notes}),
            None => json!({}),
        });
    audit["deadline_proposal"] = proposal;
    audit.to_string()
}

async fn save(
    base: &str,
    token: &str,
    project: Uuid,
    args: &ProposalArgs,
    due_at: &str,
    authority: &FiledAuthority,
    proposal: Value,
) -> Result<Value> {
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/app/api/projects/{project}/deadlines");
    let rows: Vec<ExistingDeadline> = serde_json::from_value(
        super::deadline::call(client.get(&endpoint).bearer_auth(token), "find replay").await?,
    )?;
    let existing = rows
        .into_iter()
        .find(|row| row.replay_key.as_deref() == Some(args.replay_key.trim()));
    if let Some(row) = &existing {
        if row.status == "cancelled" {
            return Err(anyhow!(
                "deadline {} is cancelled; reopen it before confirming this proposal",
                row.id
            ));
        }
    }
    let authority_kind = proposal["authority_kind"].clone();
    let category = proposal.get("category").cloned();
    let description = audit_description(
        existing.as_ref().and_then(|row| row.description.as_deref()),
        proposal,
    );
    let mut body = json!({"title":args.title, "description":description, "due_at":due_at,
        "authority_kind":authority_kind, "authority":authority.citation});
    if let Some(category) = category {
        body["category"] = category;
    }
    let request = if let Some(row) = existing {
        client.patch(format!("{endpoint}/{}", row.id))
    } else {
        body["source"] = json!("lawyer");
        body["calendar_role"] = json!("firm");
        body["replay_key"] = json!(args.replay_key);
        client.post(&endpoint)
    };
    super::deadline::call(request.bearer_auth(token).json(&body), "confirm proposal").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn command_parser_requires_explicit_confirmation_for_a_due_instant() {
        use clap::Parser;
        let base = [
            "navigator",
            "project",
            "deadline",
            "propose",
            "--rule",
            "synthetic.json",
            "--trigger-on",
            "2028-02-28",
            "--title",
            "Synthetic response",
            "--replay-key",
            "synthetic-rule-1",
        ];
        assert!(crate::Cli::try_parse_from(base).is_ok());
        assert!(crate::Cli::try_parse_from(
            base.into_iter()
                .chain(["--due-at", "2028-03-01T17:00:00-08:00"])
        )
        .is_err());
        assert!(crate::Cli::try_parse_from(base.into_iter().chain([
            "--confirm",
            "--due-at",
            "2028-03-01T17:00:00-08:00"
        ]))
        .is_ok());
    }

    fn args() -> ProposalArgs {
        ProposalArgs {
            rule: "unused.json".into(),
            trigger_on: "2028-02-28".parse().unwrap(),
            title: "Synthetic response".into(),
            replay_key: "synthetic-rule-1".into(),
            confirm: false,
            due_at: None,
            due_on: None,
        }
    }
    fn rule() -> Rule {
        serde_json::from_value(json!({"trigger":"synthetic event", "offset_days":2,
            "basis":"calendar_days", "authority_kind":"court_rule",
            "authority_id":"00000000-0000-4000-8000-000000000001"}))
        .unwrap()
    }

    async fn server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/app/api/authorities/00000000-0000-4000-8000-000000000001",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id":"00000000-0000-4000-8000-000000000001", "citation":"Synthetic Rule 1"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/app/api/projects/[^/]+/deadlines$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .with_priority(10)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn proposing_and_changing_a_trigger_never_write_a_deadline() {
        let server = server().await;
        let mut args = args();
        let first = execute(&server.uri(), "synthetic-token", None, &args, &rule())
            .await
            .unwrap();
        assert_eq!(first["status"], "proposal");
        assert_eq!(first["due_on"], "2028-03-01");
        assert_eq!(first["citation"], "Synthetic Rule 1");
        args.trigger_on = "2028-03-01".parse().unwrap();
        let changed = execute(&server.uri(), "synthetic-token", None, &args, &rule())
            .await
            .unwrap();
        assert_eq!(changed["due_on"], "2028-03-03");
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET"));
    }

    #[tokio::test]
    async fn confirmed_proposal_uses_the_existing_deadline_write_with_a_replay_key() {
        let server = server().await;
        let project = uuid::Uuid::new_v4();
        Mock::given(method("POST")).and(path(format!("/app/api/projects/{project}/deadlines")))
            .and(body_partial_json(json!({"due_at":"2028-03-01T17:00:00-08:00",
                "authority":"Synthetic Rule 1", "replay_key":"synthetic-rule-1", "source":"lawyer"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"saved"})))
            .expect(1).mount(&server).await;
        let mut args = args();
        args.confirm = true;
        args.due_at = Some("2028-03-01T17:00:00-08:00".into());
        let result = execute(
            &server.uri(),
            "synthetic-token",
            Some(project),
            &args,
            &rule(),
        )
        .await
        .unwrap();
        assert_eq!(result["id"], "saved");
    }

    #[tokio::test]
    async fn a_lawyer_can_confirm_a_manual_date_without_computing_court_days() {
        let server = server().await;
        let project = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project}/deadlines")))
            .and(body_partial_json(json!({"due_at":"2028-03-07T09:00:00Z"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"manual"})))
            .expect(1)
            .mount(&server)
            .await;
        let mut args = args();
        args.confirm = true;
        args.due_on = Some("2028-03-07".parse().unwrap());
        args.due_at = Some("2028-03-07T09:00:00Z".into());
        let mut rule = rule();
        rule.basis = Basis::CourtDays;
        assert_eq!(
            execute(
                &server.uri(),
                "synthetic-token",
                Some(project),
                &args,
                &rule
            )
            .await
            .unwrap()["id"],
            "manual"
        );
        let requests = server.received_requests().await.unwrap();
        let body: Value = requests.last().unwrap().body_json().unwrap();
        let audit: Value = serde_json::from_str(body["description"].as_str().unwrap()).unwrap();
        assert!(audit["deadline_proposal"]["due_on"].is_null());
        assert_eq!(audit["deadline_proposal"]["reviewed_due_on"], "2028-03-07");
    }

    #[tokio::test]
    async fn replay_patches_only_reviewed_fields_and_preserves_existing_notes() {
        let server = server().await;
        let project = Uuid::new_v4();
        let id = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project}/deadlines")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id":id, "replay_key":"synthetic-rule-1", "status":"open",
                "description":"Synthetic preparation instructions"
            }])))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("/app/api/projects/{project}/deadlines/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":id})))
            .expect(1)
            .mount(&server)
            .await;
        let mut args = args();
        args.confirm = true;
        args.due_at = Some("2028-03-01T17:00:00-08:00".into());
        assert_eq!(
            execute(
                &server.uri(),
                "synthetic-token",
                Some(project),
                &args,
                &rule()
            )
            .await
            .unwrap()["id"],
            id.to_string()
        );
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|request| request.method != "POST"));
        let patch: Value = requests.last().unwrap().body_json().unwrap();
        assert_eq!(patch.as_object().unwrap().len(), 5);
        assert_eq!(patch["due_at"], "2028-03-01T17:00:00-08:00");
        let audit: Value = serde_json::from_str(patch["description"].as_str().unwrap()).unwrap();
        assert_eq!(audit["notes"], "Synthetic preparation instructions");
        assert_eq!(
            audit["deadline_proposal"]["authority_id"],
            "00000000-0000-4000-8000-000000000001"
        );
    }

    #[tokio::test]
    async fn cancelled_replay_requires_reopening_and_never_writes() {
        let server = server().await;
        let project = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project}/deadlines")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id":Uuid::new_v4(), "replay_key":"synthetic-rule-1", "status":"cancelled"
            }])))
            .with_priority(1)
            .mount(&server)
            .await;
        let mut args = args();
        args.confirm = true;
        args.due_at = Some("2028-03-01T17:00:00-08:00".into());
        let error = execute(
            &server.uri(),
            "synthetic-token",
            Some(project),
            &args,
            &rule(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("reopen it before confirming"));
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == "GET"));
    }

    #[test]
    fn repeat_audit_replaces_only_its_proposal_and_keeps_notes() {
        let existing = audit_description(Some("Synthetic notes"), json!({"due_on":"2028-03-01"}));
        let updated: Value = serde_json::from_str(&audit_description(
            Some(&existing),
            json!({"due_on":"2028-03-03"}),
        ))
        .unwrap();
        assert_eq!(updated["notes"], "Synthetic notes");
        assert_eq!(updated["deadline_proposal"]["due_on"], "2028-03-03");
    }

    #[tokio::test]
    async fn overflow_is_rejected_before_any_remote_call() {
        let mut rule = rule();
        rule.offset_days = i64::MAX;
        let server = server().await;
        let error = execute(&server.uri(), "synthetic-token", None, &args(), &rule)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("calendar-day offset is out of range"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unsupported_counting_requires_a_manual_date_and_an_explicit_matching_instant() {
        let server = server().await;
        for basis in [Basis::CourtDays, Basis::Holidays] {
            let mut rule = rule();
            rule.basis = basis;
            let mut args = args();
            let proposal = execute(&server.uri(), "synthetic-token", None, &args, &rule)
                .await
                .unwrap();
            assert!(proposal["due_on"].is_null());
            assert_eq!(proposal["manual_due_on_required"], true);
            args.confirm = true;
            args.due_at = Some("2028-03-01T17:00:00-08:00".into());
            let error = execute(
                &server.uri(),
                "synthetic-token",
                Some(uuid::Uuid::new_v4()),
                &args,
                &rule,
            )
            .await
            .unwrap_err();
            assert!(error
                .to_string()
                .contains("requires lawyer-entered --due-on"));
        }
        for (instant, expected_error) in [
            ("2028-03-01", "--due-at must include an explicit offset"),
            (
                "2028-03-02T17:00:00-08:00",
                "--due-at must match the reviewed due_on",
            ),
        ] {
            let mut args = args();
            args.confirm = true;
            args.due_at = Some(instant.into());
            let error = execute(
                &server.uri(),
                "synthetic-token",
                Some(uuid::Uuid::new_v4()),
                &args,
                &rule(),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains(expected_error));
        }
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == "GET"));
    }
}
