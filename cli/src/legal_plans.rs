//! `navigator legal-plans` — a Firm's Legal Plans (where each is logged into,
//! who to call, its service levels) — and `navigator project legal-plan`,
//! which links one matter to its plan by Plan Case Number. The plans are Firm
//! data held by the deployment, never by this repository
//! (`docs/legal-plans.md`).
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use clap::{Args, Subcommand, ValueEnum};
use model::plan_benefit::{PlanBenefitTerms, PlanCapShape};
use model::plan_filing::PlanBillingConfiguration;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    projects::{project_target, surfaces},
    remote,
};

#[derive(Subcommand)]
pub enum LegalPlansCmd {
    /// The Firm's plans: portal, contact, and service levels.
    List,
    /// Review completed work and unpaid claims, grouped by Legal Plan.
    Collections {
        /// Read Firm-wide totals without Project details (requires Firm plan-metrics access).
        #[arg(long)]
        totals_only: bool,
        /// Show each carrier’s count and verified dollars awaiting filing review.
        #[arg(long)]
        filing_review: bool,
        /// Omit claim amounts from the filing-review table.
        #[arg(long, requires = "filing_review")]
        counts_only: bool,
    },
    /// Register a plan and effective-dated rules from a private YAML file.
    Configure {
        /// Read the policy at this explicit path.
        #[arg(long)]
        file: PathBuf,
        /// Verify the policy against the Firm without saving it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Record where and how a plan takes its claims, for the claim packet.
    ClaimFiling {
        /// The plan's name, as `list` shows it.
        #[arg(long)]
        plan: String,
        /// The claims email address or portal URL.
        #[arg(long)]
        file_with: String,
        /// The claim email's subject; `{case_number}` is replaced.
        #[arg(long)]
        subject: Option<String>,
        /// A separate address the itemized invoice goes to.
        #[arg(long)]
        invoice_to: Option<String>,
        /// The plan's name for its claim form.
        #[arg(long)]
        form_title: Option<String>,
        /// Proof to include, in order. Repeat for each.
        #[arg(long, value_enum)]
        proof: Vec<ClaimProof>,
    },
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
        /// Optional carrier response window in elapsed hours. Defaults to one business day.
        #[arg(long)]
        acceptance_sla_hours: Option<i32>,
        /// Optional carrier completion term in days. Omit for an open-ended matter.
        #[arg(long)]
        turnaround_sla_days: Option<i32>,
        /// What the plan asks the Firm to report, and how often.
        #[arg(long)]
        reporting_requirements: Option<String>,
    },
}

#[derive(Clone, Copy, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ClaimProof {
    EngagementLetter,
    FiledDocument,
}

/// The plan fields `navigator project legal-plan set` may change. An absent
/// flag keeps the value; an empty one (`--case-number ""`) clears it.
#[derive(Args, Debug, Default)]
pub struct PlanFields {
    /// One of the Firm's plan names, for example `ARAG`. `navigator
    /// legal-plans --firm <firm> list` shows them; `create` adds one.
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
    /// Verified benefit shape: hourly-cap, dollar-cap, contested-uncontested, consult-only, or flat-fee.
    #[arg(long)]
    pub cap_shape: Option<PlanCapShape>,
    /// Dollar cap, flat fee, or uncontested cap, in cents.
    #[arg(long, requires = "cap_shape")]
    pub cap_cents: Option<i32>,
    /// Hourly cap, in minutes.
    #[arg(long, requires = "cap_shape")]
    pub cap_minutes: Option<i32>,
    /// Contested cap, in cents, when the plan gives both caps.
    #[arg(long, requires = "cap_shape")]
    pub contested_cap_cents: Option<i32>,
    /// Verified plan rate, in cents per hour.
    #[arg(long, requires = "cap_shape")]
    pub plan_rate_cents_per_hour: Option<i32>,
    /// Verified paralegal rate, in cents per hour.
    #[arg(long, requires = "cap_shape")]
    pub paralegal_rate_cents_per_hour: Option<i32>,
    /// Verified member-paid rate for separately agreed uncovered work, in cents per hour.
    #[arg(long, requires = "cap_shape")]
    pub member_rate_cents_per_hour: Option<i32>,
    /// `document_review`, `letter`, `uncontested_divorce`, `court_appearance`,
    /// `contested_divorce`, `custody`, or `probate`.
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
    fn patch(&self) -> Result<Map<String, Value>> {
        let mut body: Map<String, Value> = [
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
        .collect();
        if let Some(cap_shape) = self.cap_shape {
            let terms = PlanBenefitTerms {
                cap_shape,
                cap_cents: self.cap_cents,
                cap_minutes: self.cap_minutes,
                contested_cap_cents: self.contested_cap_cents,
                plan_rate_cents_per_hour: self.plan_rate_cents_per_hour,
                paralegal_rate_cents_per_hour: self.paralegal_rate_cents_per_hour,
                member_rate_cents_per_hour: self.member_rate_cents_per_hour,
            };
            terms.validate().map_err(|diagnostic| anyhow!(diagnostic))?;
            body.insert("benefit_terms".into(), serde_json::to_value(terms)?);
        }
        Ok(body)
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
    crate::billing::finish(
        command,
        result.map(|value| serde_json::to_string_pretty(&value).unwrap_or_default()),
    )
}

pub async fn run(firm: String, host: Option<String>, action: LegalPlansCmd) -> ExitCode {
    let filing_review = matches!(
        action,
        LegalPlansCmd::Collections {
            filing_review: true,
            ..
        }
    );
    let counts_only = matches!(
        action,
        LegalPlansCmd::Collections {
            counts_only: true,
            ..
        }
    );
    let result = async {
        let (base, token) = remote::resolve(host.as_deref())?;
        firm_command(&base, &token, &firm, action).await
    }
    .await;
    if filing_review {
        return crate::billing::finish(
            "legal-plans",
            result.and_then(|value| render_filing_review(&value, counts_only)),
        );
    }
    finish("legal-plans", result)
}

fn render_filing_review(report: &Value, counts_only: bool) -> Result<String> {
    use std::fmt::Write;

    let review = model::billing_report::FilingReview::deserialize(&report["filing_review"])
        .context("the site did not return complete filing-review totals")?;
    let mut text = String::from("Carrier   Claims awaiting filing review");
    if !counts_only {
        text.push_str("   Verified amount");
    }
    text.push('\n');
    for (carrier, total) in [("ARAG", review.arag), ("LegalEASE", review.legal_ease)]
        .into_iter()
        .chain(
            review
                .other_plans
                .iter()
                .map(|(name, total)| (name.as_str(), *total)),
        )
    {
        write!(
            text,
            "{carrier:<9} {:>28}",
            total.eligible_unsubmitted_count
        )?;
        if !counts_only {
            write!(
                text,
                "   {:>15}",
                crate::billing::money(Some(total.eligible_unsubmitted_cents))
            )?;
        }
        text.push('\n');
    }
    Ok(text)
}

fn read_configuration(file: &Path) -> Result<PlanBillingConfiguration> {
    let content = std::fs::read(file).context("read private Legal Plan configuration")?;
    serde_yaml::from_slice(&content)
        .map_err(|_| anyhow!("invalid private Legal Plan configuration; check the YAML schema"))
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
        LegalPlansCmd::Collections { totals_only, .. } => {
            let route = format!("{base}/app/api/firms/{firm}/legal-plan-collections");
            call(
                client
                    .get(route)
                    .query(&[("totals_only", totals_only)])
                    .bearer_auth(token),
                "collections",
            )
            .await
        }

        LegalPlansCmd::Configure { file, dry_run } => {
            let configuration = read_configuration(&file)?;
            let response = client
                .put(format!("{route}/configuration"))
                .query(&[("dry_run", dry_run)])
                .bearer_auth(token)
                .json(&configuration)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .context("request private Legal Plan configuration")?;
            let status = response.status();
            if !status.is_success() {
                return Err(anyhow!("Legal Plan configuration refused ({status})"));
            }
            Ok(json!({ "configuration": if dry_run { "validated" } else { "saved" } }))
        }
        LegalPlansCmd::ClaimFiling {
            plan,
            file_with,
            subject,
            invoice_to,
            form_title,
            proof,
        } => {
            let plans = call(client.get(&route).bearer_auth(token), "list").await?;
            let id = plans
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| {
                    row["name"]
                        .as_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case(plan.trim()))
                })
                .and_then(|row| row["id"].as_str())
                .ok_or_else(|| anyhow!("the Firm has no plan named {plan}"))?
                .to_owned();
            let body = json!({
                "file_with": file_with, "subject": subject, "invoice_to": invoice_to,
                "form_title": form_title, "proof": proof,
            });
            call(
                client
                    .put(format!("{route}/{id}/claim-filing"))
                    .bearer_auth(token)
                    .json(&body),
                "claim filing",
            )
            .await
        }
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
pub(crate) async fn project_route(
    host: Option<&str>,
    project: Option<&str>,
) -> Result<(String, String, Uuid)> {
    let (host, project) = project_target(host, project)?;
    let (base, token) = remote::resolve(host.as_deref())?;
    let id = match Uuid::parse_str(&project) {
        Ok(id) => id,
        Err(_) => surfaces::resolve_project_id(&base, &token, &project).await?,
    };
    Ok((format!("{base}/app/api/projects/{id}"), token, id))
}

/// `navigator project legal-plan show`: the matter's plan, Plan Case
/// Number, and plan dates.
pub async fn show(host: Option<&str>, project: Option<&str>) -> ExitCode {
    let result = async {
        let (route, token, _) = project_route(host, project).await?;
        call(
            reqwest::Client::new()
                .get(format!("{route}/legal-plan"))
                .bearer_auth(token),
            "show",
        )
        .await
    }
    .await;
    finish("project legal-plan", result)
}

/// `navigator project legal-plan set`: change only the fields given.
pub async fn set(host: Option<&str>, project: Option<&str>, fields: &PlanFields) -> ExitCode {
    let result = async {
        let patch = fields.patch()?;
        if patch.is_empty() {
            return Err(anyhow!(
                "name at least one field to set, for example --case-number"
            ));
        }
        let (route, token, _) = project_route(host, project).await?;
        let updated = call(
            reqwest::Client::new()
                .patch(format!("{route}/legal-plan"))
                .bearer_auth(token)
                .json(&patch),
            "set",
        )
        .await?;
        if let Some(plan) = fields.plan.as_deref().filter(|_| project.is_none() && host.is_none()) {
            let name = if plan.trim().is_empty() && updated.get("legal_plan") == Some(&Value::Null) {
                None
            } else {
                Some(updated.pointer("/legal_plan/name").and_then(Value::as_str)
                    .context("the site did not return the Legal Plan name")?)
            };
            crate::projects::set::declare_legal_plan(
                &std::env::current_dir()?,
                updated["code"].as_str().context("the site did not return the Project code")?,
                name,
            )
            .context("the site updated the Legal Plan. Rerun from this Project checkout to write navigator.yaml")?;
        }
        Ok(updated)
    }
    .await;
    finish("project legal-plan", result)
}

#[derive(Subcommand)]
pub enum ClaimCmd {
    /// Read the Project's claim status.
    Status,
    /// Record completed work and its verified fee terms, if available.
    Work(ClaimWork),
    /// Record verified or updated coverage terms.
    Authorize {
        #[command(flatten)]
        claim: ExistingClaim,
        #[command(flatten)]
        terms: ClaimTerms,
    },
    /// Prepare covered work for the lawyer's manual filing.
    Prepare(ExistingClaim),
    /// Write the claim form, itemized invoice, and proof as one PDF, and file
    /// it on the Project linked to the claim.
    Packet {
        /// Claim UUID printed by 'claim work' or 'claim status'.
        #[arg(long)]
        claim: Uuid,
        /// Where to write the PDF.
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Confirm an already-filed claim, by the provider's reference or, for a
    /// claim filed by email, the sent message's id.
    #[command(visible_alias = "file")]
    Submit(ClaimSubmission),
    /// Record the provider's reference for a claim filed without one.
    Reference {
        #[command(flatten)]
        claim: ExistingClaim,
        /// Claim reference from the provider.
        #[arg(long)]
        provider_reference: String,
    },
    /// Confirm a corrected filing for the same claim.
    Correct(ClaimCorrection),
    /// Record the provider's approval.
    Approve(ClaimAmountEvidence),
    /// Record the provider's denial.
    Deny {
        #[command(flatten)]
        claim: ExistingClaim,
        /// Identifier of the provider decision.
        #[arg(long)]
        reference: String,
    },
    /// Confirm an actual payment; a claim can have several remittance lines.
    #[command(visible_alias = "record-payment")]
    Pay(ClaimAmountEvidence),
    /// Confirm a refund or payment reversal.
    Reverse(ClaimAmountEvidence),
    /// Apply one lawyer-confirmed claim event from a private JSON file or stdin.
    Record {
        /// JSON file path, or '-' for stdin.
        #[arg(long)]
        input: std::path::PathBuf,
    },
}

#[derive(Args)]
pub struct ExistingClaim {
    /// Claim UUID printed by 'claim work' or 'claim status'.
    #[arg(long)]
    claim: Uuid,
    /// Date of the event (YYYY-MM-DD).
    #[arg(long)]
    on: NaiveDate,
}

#[derive(Args)]
pub struct ClaimSubmission {
    #[command(flatten)]
    claim: ExistingClaim,
    /// Claim reference from the provider's filing confirmation.
    #[arg(long, required_unless_present = "sent_message")]
    provider_reference: Option<String>,
    /// For a claim filed by email: the sent message's id (Gmail's message
    /// id). The claim shows as filed; add the provider's reference later
    /// with `reference`.
    #[arg(long)]
    sent_message: Option<String>,
    /// Filed amount in cents, if confirmed.
    #[arg(long)]
    claimed_cents: Option<i32>,
}

#[derive(Args)]
pub struct ClaimCorrection {
    #[command(flatten)]
    claim: ExistingClaim,
    /// Claim reference from the provider's corrected filing.
    #[arg(long)]
    provider_reference: String,
    /// Filed amount in cents, if confirmed.
    #[arg(long)]
    claimed_cents: Option<i32>,
}

#[derive(Args)]
pub struct ClaimAmountEvidence {
    #[command(flatten)]
    claim: ExistingClaim,
    /// Provider decision or remittance evidence identifier.
    #[arg(long)]
    reference: String,
    /// Confirmed amount in cents.
    #[arg(long)]
    cents: i32,
}

#[derive(Serialize, Copy, Clone, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum ClaimService {
    AragAdviceOnly,
    AragRepresentation,
    LegalEaseLegalGuard,
    LegalEasePaymentAuthorization,
    Manual,
}

#[derive(Args)]
pub struct ClaimWork {
    /// Verified coverage and submission path for the completed service.
    #[arg(long, value_enum)]
    service: ClaimService,
    /// Verified provider benefit or benefit code.
    #[arg(long)]
    benefit: String,
    /// Service completion date (YYYY-MM-DD).
    #[arg(long)]
    performed_on: NaiveDate,
    /// Actual attorney minutes when matching Time Entries are unavailable.
    #[arg(long, conflicts_with = "time_entry")]
    attorney_minutes: Option<i32>,
    /// Existing Time Entry UUID. Repeat for multiple entries.
    #[arg(long)]
    time_entry: Vec<Uuid>,
    /// Stable label to distinguish multiple claims for the same benefit, service, and date.
    #[arg(long, default_value = "default")]
    work_key: String,
    #[command(flatten)]
    terms: ClaimTerms,
}

#[derive(Args)]
pub struct ClaimTerms {
    /// Identifier of the case-specific written approval or verified plan benefit.
    #[arg(long)]
    authorization_reference: Option<String>,
    /// Case-specific approved fixed fee in cents.
    #[arg(
        long,
        requires = "authorization_reference",
        conflicts_with = "rate_cents_per_hour"
    )]
    fixed_cents: Option<i32>,
    /// Approved hourly rate in cents.
    #[arg(long, requires_all = ["authorization_reference", "covered_minutes"])]
    rate_cents_per_hour: Option<i32>,
    /// Maximum authorized minutes for this work.
    #[arg(long, requires = "rate_cents_per_hour")]
    covered_minutes: Option<i32>,
    /// Additional minutes awaiting written authorization.
    #[arg(long, requires = "rate_cents_per_hour")]
    pending_minutes: Option<i32>,
}

impl ClaimTerms {
    fn evidence(&self) -> Result<Option<Value>> {
        match (&self.authorization_reference, self.fixed_cents, self.rate_cents_per_hour) {
            (Some(reference), Some(cents), None) =>
                Ok(Some(json!({"type": "fixed_fee", "reference": reference, "cents": cents}))),
            (Some(reference), None, Some(rate)) => Ok(Some(json!({
                "type": "capped_hours", "reference": reference, "rate_cents_per_hour": rate,
                "covered_minutes": self.covered_minutes, "pending_minutes": self.pending_minutes.unwrap_or_default()
            }))),
            (None, None, None) => Ok(None),
            _ => Err(anyhow!("supply a verified authorization reference and either a fixed fee or capped hourly terms")),
        }
    }
}

fn claim_key(value: &Value) -> Uuid {
    let digest = Sha256::digest(value.to_string());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn claim_event(claim: Uuid, on: NaiveDate, event: &Value) -> Value {
    let event_id = claim_key(&json!(["navigator-plan-claim-event", claim, on, event]));
    json!({"claim_id": claim, "event_id": event_id, "occurred_on": on, "event": event})
}

impl ExistingClaim {
    fn event(self, event: &Value) -> Value {
        claim_event(self.claim, self.on, event)
    }
}

impl ClaimSubmission {
    fn event(self) -> Value {
        let mut event = json!({"type": "submission", "claimed_cents": self.claimed_cents});
        if let Some(reference) = self.provider_reference {
            event["provider_reference"] = reference.into();
        }
        if let Some(message) = self.sent_message {
            event["sent_message_id"] = message.into();
        }
        self.claim.event(&event)
    }
}

impl ClaimCorrection {
    fn event(self) -> Value {
        self.claim.event(&json!({
            "type": "correction", "provider_reference": self.provider_reference, "claimed_cents": self.claimed_cents,
        }))
    }
}

impl ClaimAmountEvidence {
    fn event(self, kind: &str) -> Value {
        self.claim
            .event(&json!({"type": kind, "reference": self.reference, "cents": self.cents}))
    }
}

impl ClaimCmd {
    fn into_body(self, project: Uuid) -> Result<Option<Value>> {
        let body = match self {
            Self::Status | Self::Packet { .. } => return Ok(None),
            Self::Work(work) => {
                let claim = claim_key(&json!([
                    "navigator-plan-claim",
                    project,
                    work.service,
                    work.benefit,
                    work.performed_on,
                    work.work_key,
                ]));
                let mut entries = work.time_entry;
                entries.sort_unstable();
                claim_event(
                    claim,
                    work.performed_on,
                    &json!({
                        "type": "work_recorded", "work": {
                            "service": work.service, "benefit": work.benefit, "performed_on": work.performed_on,
                            "attorney_minutes": work.attorney_minutes, "time_entry_ids": entries,
                            "authorization": work.terms.evidence()?,
                        }
                    }),
                )
            }
            Self::Authorize { claim, terms } => {
                let authorization = terms
                    .evidence()?
                    .ok_or_else(|| anyhow!("supply verified authorization terms"))?;
                claim.event(
                    &json!({"type": "authorization_confirmed", "authorization": authorization}),
                )
            }
            Self::Prepare(claim) => claim.event(&json!({"type": "prepared"})),
            Self::Submit(submission) => submission.event(),
            Self::Reference {
                claim,
                provider_reference,
            } => claim.event(
                &json!({"type": "provider_reference", "provider_reference": provider_reference}),
            ),
            Self::Correct(correction) => correction.event(),
            Self::Approve(evidence) => evidence.event("approval"),
            Self::Deny { claim, reference } => {
                claim.event(&json!({"type": "denial", "reference": reference}))
            }
            Self::Pay(evidence) => evidence.event("payment"),
            Self::Reverse(evidence) => evidence.event("reversal"),
            Self::Record { input } => {
                let decoded = if input == Path::new("-") {
                    serde_json::from_reader(std::io::stdin().lock())
                } else {
                    let file = std::fs::File::open(input).context("open private claim event")?;
                    serde_json::from_reader(file)
                };
                decoded.map_err(|_| anyhow!("claim event must be valid JSON"))?
            }
        };
        Ok(Some(body))
    }
}

pub async fn claim(host: Option<&str>, project: Option<&str>, action: ClaimCmd) -> ExitCode {
    let result = async {
        let (route, token, id) = project_route(host, project).await?;
        claim_command(&route, &token, id, action).await
    }
    .await;
    finish("project legal-plan claim", result)
}

pub(crate) async fn claim_command(
    project_route: &str,
    token: &str,
    project: Uuid,
    action: ClaimCmd,
) -> Result<Value> {
    let route = format!("{project_route}/legal-plan-claims");
    let client = remote::authenticated_client_builder(project_route)?.build()?;
    if let ClaimCmd::Packet { claim, out } = &action {
        return packet(&client, &route, token, *claim, out).await;
    }
    match action.into_body(project)? {
        None => call(client.get(route).bearer_auth(token), "claim status").await,
        Some(body) => {
            call(
                client.post(route).bearer_auth(token).json(&body),
                "claim record",
            )
            .await
        }
    }
}

/// File the claim packet and write its PDF to `out`.
async fn packet(
    client: &reqwest::Client,
    route: &str,
    token: &str,
    claim: Uuid,
    out: &Path,
) -> Result<Value> {
    use base64::Engine as _;
    let mut filed = call(
        client
            .post(format!("{route}/{claim}/packet"))
            .bearer_auth(token)
            .timeout(Duration::from_secs(120)),
        "claim packet",
    )
    .await?;
    let pdf = filed
        .as_object_mut()
        .and_then(|filed| filed.remove("pdf_base64"))
        .and_then(|pdf| pdf.as_str().map(str::to_owned))
        .context("the site returned no packet PDF")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(pdf)
        .context("decode the packet PDF")?;
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;
    filed["out"] = out.display().to_string().into();
    if filed["missing_proof"]
        .as_array()
        .is_some_and(|missing| !missing.is_empty())
    {
        eprintln!(
            "navigator: the plan asks for proof not filed on the Project: {}",
            filed["missing_proof"]
        );
    }
    Ok(filed)
}

#[cfg(test)]
mod tests {
    use super::{finish, firm_command, LegalPlansCmd, PlanFields};
    use clap::Parser;
    use std::process::ExitCode;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_resolves_a_display_name_through_the_authorized_firm_directory() {
        /// `navigator legal-plans`'s own shape: `--firm` and the action.
        #[derive(Parser)]
        struct LegalPlans {
            #[arg(long)]
            firm: String,
            #[command(subcommand)]
            action: LegalPlansCmd,
        }
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

        let LegalPlans { firm, action } =
            LegalPlans::try_parse_from(["legal-plans", "--firm", "example practice pc", "list"])
                .unwrap();
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
            serde_json::Value::Object(fields.patch().unwrap()),
            serde_json::json!({
                "legal_plan": "ARAG",
                "plan_case_number": "4900000-001",
                "plan_closed_on": "",
            })
        );
        assert!(PlanFields::default().patch().unwrap().is_empty());
    }

    #[test]
    fn verified_terms_use_the_benefit_shape_and_units() {
        let fields = PlanFields {
            plan: Some("LegalEASE".into()),
            benefit_code: Some("SAMPLE-BENEFIT".into()),
            cap_shape: Some(model::plan_benefit::PlanCapShape::HourlyCap),
            cap_minutes: Some(240),
            plan_rate_cents_per_hour: Some(9000),
            paralegal_rate_cents_per_hour: Some(4500),
            ..PlanFields::default()
        };
        let patch = fields.patch().unwrap();
        assert_eq!(patch["legal_plan"], "LegalEASE");
        assert_eq!(patch["benefit_code"], "SAMPLE-BENEFIT");
        let terms = &patch["benefit_terms"];
        assert_eq!(terms["cap_shape"], "hourly_cap");
        assert_eq!(terms["cap_minutes"], 240);
        assert_eq!(terms["plan_rate_cents_per_hour"], 9000);
        assert_eq!(terms["paralegal_rate_cents_per_hour"], 4500);
        let wrong_units = PlanFields {
            cap_cents: Some(5000),
            ..fields
        };
        assert!(wrong_units.patch().is_err());
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
                acceptance_sla_hours: Some(24),
                turnaround_sla_days: Some(30),
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

#[cfg(test)]
mod collections_tests {
    use super::*;
    use clap::Parser;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Parser)]
    struct ClaimCli {
        #[command(subcommand)]
        action: ClaimCmd,
    }

    fn parse(args: &[&str]) -> ClaimCmd {
        ClaimCli::try_parse_from(std::iter::once("claim").chain(args.iter().copied()))
            .unwrap()
            .action
    }

    #[test]
    fn navigator_project_registers_the_documented_work_command() {
        use crate::commands::Cli;
        assert!(Cli::try_parse_from([
            "navigator",
            "project",
            "legal-plan",
            "claim",
            "--project",
            "synthetic-matter",
            "work",
            "--service",
            "arag-advice-only",
            "--benefit",
            "SAMPLE",
            "--performed-on",
            "2026-11-01",
            "--attorney-minutes",
            "30",
        ])
        .is_ok());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One CLI walk exercises the documented actions.
    async fn claim_cli_sends_typed_work_through_payment_and_loads_the_worklist() {
        let server = MockServer::start().await;
        let firm = Uuid::now_v7();
        let project = Uuid::now_v7();
        let project_path = format!("/app/api/projects/{project}");
        let route = format!("{}{project_path}", server.uri());
        let work = [
            "work",
            "--service",
            "legal-ease-payment-authorization",
            "--benefit",
            "SAMPLE",
            "--performed-on",
            "2026-11-01",
            "--attorney-minutes",
            "90",
            "--authorization-reference",
            "sample-authorization",
            "--rate-cents-per-hour",
            "12000",
            "--covered-minutes",
            "60",
            "--pending-minutes",
            "30",
        ];
        let initial = parse(&work).into_body(project).unwrap().unwrap();
        assert_eq!(initial, parse(&work).into_body(project).unwrap().unwrap());
        assert_eq!(
            initial["event"]["work"]["authorization"],
            json!({
                "type": "capped_hours", "reference": "sample-authorization", "rate_cents_per_hour": 12000,
                "covered_minutes": 60, "pending_minutes": 30,
            })
        );
        let claim_id = initial["claim_id"].as_str().unwrap();
        let prepare = ["prepare", "--claim", claim_id, "--on", "2026-11-01"];
        let submit = [
            "submit",
            "--claim",
            claim_id,
            "--on",
            "2026-11-01",
            "--provider-reference",
            "sample-confirmation",
            "--claimed-cents",
            "12000",
        ];
        let pay = [
            "pay",
            "--claim",
            claim_id,
            "--on",
            "2026-11-09",
            "--reference",
            "sample-payment",
            "--cents",
            "7000",
        ];
        for (args, expected) in [
            (&work[..], initial.clone()),
            (
                &prepare[..],
                parse(&prepare).into_body(project).unwrap().unwrap(),
            ),
            (
                &submit[..],
                parse(&submit).into_body(project).unwrap().unwrap(),
            ),
            (&pay[..], parse(&pay).into_body(project).unwrap().unwrap()),
        ] {
            Mock::given(method("POST"))
                .and(path(format!("{project_path}/legal-plan-claims")))
                .and(header("authorization", "Bearer token"))
                .and(body_json(&expected))
                .respond_with(ResponseTemplate::new(200).set_body_json(&expected))
                .expect(1)
                .mount(&server)
                .await;
            let response = claim_command(&route, "token", project, parse(args))
                .await
                .unwrap();
            assert_eq!(response, expected);
        }
        let report = json!({"plans": {"Legal Ease": {"net_collected_cents": 7000}}});
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/firms/{firm}/legal-plan-collections"
            )))
            .and(query_param("totals_only", "false"))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&report))
            .expect(1)
            .mount(&server)
            .await;
        let result = firm_command(
            &server.uri(),
            "token",
            &firm.to_string(),
            LegalPlansCmd::Collections {
                totals_only: false,
                filing_review: false,
                counts_only: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(result, report);
    }
}

#[cfg(test)]
mod private_configuration_tests {
    use super::*;

    #[test]
    fn private_plan_configuration_rejects_case_data_without_echoing_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legal-plan.yaml");
        std::fs::write(
            &path,
            "name: Example Plan\nfiling_rules: []\ncase_number: SYNTHETIC_MARKER\n",
        )
        .unwrap();
        let error = read_configuration(&path).unwrap_err();
        assert!(!format!("{error:#}").contains("SYNTHETIC_MARKER"));
    }

    #[test]
    fn filing_review_can_omit_amounts_including_for_configured_plans() {
        let report = json!({"filing_review": {
            "ARAG": {"eligible_unsubmitted_count": 0, "eligible_unsubmitted_cents": 0},
            "LegalEASE": {"eligible_unsubmitted_count": 0, "eligible_unsubmitted_cents": 0},
            "Example Plan": {"eligible_unsubmitted_count": 1, "eligible_unsubmitted_cents": 100}
        }});
        let text = render_filing_review(&report, true).unwrap();
        assert!(text.contains("Example Plan"));
        assert!(!text.contains('$'));
        assert!(!text.contains("Verified amount"));
    }
}
