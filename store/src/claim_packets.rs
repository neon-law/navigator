//! The facts a Legal Plan claim packet prints (LAW-585): the plan's claim
//! form fields from the Project's plan link, an itemized invoice from the
//! claim's Time Entries or fixed fee, and the proof the plan's filing rules
//! ask for. Rendering and filing the PDF are the caller's.
use chrono::NaiveDate;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    legal_plans::{ClaimFiling, ClaimProof},
    plan_claims::{self, Authorization, ClaimError, Service},
    postgres::PostgresDb,
};

/// One line of the itemized invoice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvoiceLine {
    pub performed_on: NaiveDate,
    pub timekeeper: String,
    pub minutes: i32,
    pub narrative: String,
    /// At the plan's hourly rate; absent under a fixed fee.
    pub amount_cents: Option<i64>,
}

/// A filed document the plan asks to see with the claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProofDocument {
    pub proof: ClaimProof,
    pub asset_id: Uuid,
    pub filename: String,
    #[serde(skip)]
    pub storage_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimPacket {
    pub claim_id: Uuid,
    pub claim_stage: &'static str,
    pub plan_name: String,
    pub form_title: String,
    pub filing: Option<ClaimFiling>,
    /// Where the claim is filed: the benefit's dated filing rule when one is
    /// in effect on the service date, else the plan's claim filing.
    pub file_with: Option<String>,
    /// Who files it, from the benefit's filing rule.
    pub filer: Option<String>,
    pub subject: Option<String>,
    pub firm_name: String,
    /// The Firm's billing entity, when it names one.
    pub billing_entity: Option<String>,
    pub matter: String,
    pub matter_code: String,
    pub plan_case_number: String,
    pub member_id: Option<String>,
    pub member_name: Option<String>,
    /// The plan type: the benefit code the plan assigned.
    pub plan_type: Option<String>,
    pub responsible_lawyer: Option<String>,
    pub service: &'static str,
    pub benefit: String,
    pub first_service_on: NaiveDate,
    pub last_service_on: NaiveDate,
    pub lines: Vec<InvoiceLine>,
    pub attorney_minutes: Option<i32>,
    pub rate_cents_per_hour: Option<i32>,
    pub fixed_fee_cents: Option<i32>,
    /// Fees for all the work at the plan's terms.
    pub total_cents: Option<i64>,
    /// What the plan covers under its verified terms.
    pub covered_cents: Option<i64>,
    /// The rest, owed by the member.
    pub member_share_cents: Option<i64>,
    pub claimed_cents: Option<i32>,
    pub proofs: Vec<ProofDocument>,
    /// Proof the plan asks for that is not filed on the Project.
    pub missing_proof: Vec<ClaimProof>,
}

const fn service_label(service: Service) -> &'static str {
    match service {
        Service::AragAdviceOnly => "Advice only",
        Service::AragRepresentation => "Representation",
        Service::LegalEaseLegalGuard => "LegalGuard",
        Service::LegalEasePaymentAuthorization => "Payment authorization",
        Service::Manual => "Covered service",
    }
}

async fn proof(
    pg: &PostgresDb,
    project: Uuid,
    kind: ClaimProof,
) -> Result<Option<ProofDocument>, sqlx::Error> {
    let row = match kind {
        ClaimProof::EngagementLetter => {
            sqlx::query(
                "SELECT id,coalesce(slug,filename) AS name,storage_key FROM asset
                 WHERE project_id=$1 AND deleted_at IS NULL AND kind='onboarding'
                   AND content_type='application/pdf' AND signing IS NOT NULL
                   AND coalesce(slug,filename) LIKE '%\\_signed.pdf'
                 ORDER BY inserted_at DESC, id DESC LIMIT 1",
            )
            .bind(project)
            .fetch_optional(&pg.0)
            .await?
        }
        ClaimProof::FiledDocument => {
            sqlx::query(
                "SELECT a.id,coalesce(a.slug,a.filename) AS name,a.storage_key
                 FROM efiling_transactions e JOIN asset a
                   ON a.id=coalesce(e.conformed_document_id,e.submitted_document_id)
                 WHERE e.project_id=$1 AND a.project_id=$1 AND a.deleted_at IS NULL
                   AND a.content_type='application/pdf'
                 ORDER BY e.inserted_at DESC, e.id DESC LIMIT 1",
            )
            .bind(project)
            .fetch_optional(&pg.0)
            .await?
        }
    };
    row.map(|row| {
        Ok(ProofDocument {
            proof: kind,
            asset_id: row.try_get("id")?,
            filename: row
                .try_get::<Option<String>, _>("name")?
                .unwrap_or_default(),
            storage_key: row.try_get("storage_key")?,
        })
    })
    .transpose()
}

/// The proof documents filed on the Project, and the kinds it lacks.
async fn proofs(
    pg: &PostgresDb,
    project: Uuid,
    wanted: Vec<ClaimProof>,
) -> Result<(Vec<ProofDocument>, Vec<ClaimProof>), sqlx::Error> {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for kind in wanted {
        match proof(pg, project, kind).await? {
            Some(document) => found.push(document),
            None => missing.push(kind),
        }
    }
    Ok((found, missing))
}

async fn invoice_lines(
    pg: &PostgresDb,
    project: Uuid,
    claim: &plan_claims::PacketClaim,
    rate: Option<i32>,
) -> Result<Vec<InvoiceLine>, ClaimError> {
    let amount =
        |minutes: i32| rate.map(|rate| crate::time_entries::entry_amount_cents(minutes, rate));
    if claim.work.time_entry_ids.is_empty() {
        return Ok(claim
            .work
            .attorney_minutes
            .map(|minutes| InvoiceLine {
                performed_on: claim.work.performed_on,
                timekeeper: String::new(),
                minutes,
                narrative: format!(
                    "{} ({})",
                    claim.work.benefit,
                    service_label(claim.work.service)
                ),
                amount_cents: amount(minutes),
            })
            .into_iter()
            .collect());
    }
    let rows = sqlx::query(
        "SELECT t.performed_on,p.name,t.minutes,t.description FROM time_entry t
         JOIN person p ON p.id=t.attorney_id
         WHERE t.project_id=$1 AND t.id=ANY($2) ORDER BY t.performed_on,t.id",
    )
    .bind(project)
    .bind(&claim.work.time_entry_ids)
    .fetch_all(&pg.0)
    .await?;
    rows.iter()
        .map(|row| {
            let minutes: i32 = row.try_get("minutes")?;
            Ok(InvoiceLine {
                performed_on: row.try_get("performed_on")?,
                timekeeper: row.try_get("name")?,
                minutes,
                narrative: row.try_get("description")?,
                amount_cents: amount(minutes),
            })
        })
        .collect()
}

/// Where the claim is filed and by whom: the benefit's dated filing rule in
/// effect on the service date, else the plan's claim filing.
async fn destination(
    pg: &PostgresDb,
    claim: &plan_claims::PacketClaim,
    filing: Option<&ClaimFiling>,
) -> Result<(Option<String>, Option<String>), sqlx::Error> {
    let rule = crate::legal_plans::filing_rule(
        &mut *pg.0.acquire().await?,
        claim.plan_id,
        &claim.work.benefit,
        claim.work.performed_on,
    )
    .await?;
    Ok(match rule {
        Some(rule) => (Some(rule.filing_destination), Some(rule.filer)),
        None => (filing.map(|filing| filing.file_with.clone()), None),
    })
}

/// The first and last service dates the invoice covers.
fn service_span(lines: &[InvoiceLine], performed_on: NaiveDate) -> (NaiveDate, NaiveDate) {
    let dates = lines.iter().map(|line| line.performed_on);
    let first = dates.clone().min().unwrap_or(performed_on);
    let last = dates.max().unwrap_or(performed_on).max(performed_on);
    (first, last)
}

/// Gather one claim's packet. The plan's filing rules choose the proof;
/// without them the signed engagement letter is the proof.
///
/// # Errors
/// [`ClaimError::NotFound`] outside the Project and Firm; a database
/// failure.
pub async fn read(
    pg: &PostgresDb,
    project: Uuid,
    firm: Uuid,
    claim_id: Uuid,
) -> Result<ClaimPacket, ClaimError> {
    let claim = plan_claims::for_packet(pg, project, firm, claim_id).await?;
    let plan = crate::legal_plans::find(pg, firm, claim.plan_id)
        .await
        .map_err(|_| ClaimError::NotFound)?
        .ok_or(ClaimError::NotFound)?;
    let facts = sqlx::query(
        "SELECT p.name,p.code,p.plan_member_id,p.benefit_code,f.name AS firm_name,e.name AS entity_name,
         (SELECT c.name FROM person c WHERE c.id=coalesce(p.client_dri_person_id,
             (SELECT r.person_id FROM person_project_role r WHERE r.project_id=p.id AND r.is_client_dri LIMIT 1))) AS member_name,
         (SELECT l.name FROM person_project_role r JOIN person l ON l.id=r.person_id
             WHERE r.project_id=p.id AND r.is_lawyer_dri LIMIT 1) AS lawyer_name
         FROM project p JOIN firm f ON f.id=p.firm_id LEFT JOIN entity e ON e.id=f.entity_id
         WHERE p.id=$1 AND p.firm_id=$2",
    )
    .bind(project)
    .bind(firm)
    .fetch_optional(&pg.0)
    .await?
    .ok_or(ClaimError::NotFound)?;
    let (rate, fixed) = match &claim.work.authorization {
        Some(Authorization::CappedHours {
            rate_cents_per_hour,
            ..
        }) => (Some(*rate_cents_per_hour), None),
        Some(Authorization::FixedFee { cents, .. }) => (None, Some(*cents)),
        None => (None, None),
    };
    let lines = invoice_lines(pg, project, &claim, rate).await?;
    let minutes = claim
        .work
        .attorney_minutes
        .or_else(|| (!lines.is_empty()).then(|| lines.iter().map(|line| line.minutes).sum()));
    let total = match (rate, fixed) {
        (Some(rate), _) => {
            minutes.map(|minutes| crate::time_entries::entry_amount_cents(minutes, rate))
        }
        (None, Some(fixed)) => Some(i64::from(fixed)),
        (None, None) => None,
    };
    let member_share = total
        .zip(claim.covered_cents)
        .map(|(total, covered)| (total - covered).max(0));
    let wanted = plan.claim_filing.as_ref().map_or_else(
        || vec![ClaimProof::EngagementLetter],
        |filing| filing.proof.clone(),
    );
    let (proofs, missing_proof) = proofs(pg, project, wanted).await?;
    let (file_with, filer) = destination(pg, &claim, plan.claim_filing.as_ref()).await?;
    let (first, last) = service_span(&lines, claim.work.performed_on);
    Ok(ClaimPacket {
        claim_id: claim.id,
        claim_stage: claim.stage,
        form_title: plan
            .claim_filing
            .as_ref()
            .and_then(|filing| filing.form_title.clone())
            .unwrap_or_else(|| format!("{} claim form", plan.name)),
        subject: plan
            .claim_filing
            .as_ref()
            .and_then(|filing| filing.subject_for(&claim.plan_case_number)),
        file_with,
        filer,
        filing: plan.claim_filing,
        plan_name: claim.plan_name,
        firm_name: facts.try_get("firm_name")?,
        billing_entity: facts.try_get("entity_name")?,
        matter: facts.try_get("name")?,
        matter_code: facts.try_get("code")?,
        plan_case_number: claim.plan_case_number,
        member_id: facts.try_get("plan_member_id")?,
        member_name: facts.try_get("member_name")?,
        plan_type: facts.try_get("benefit_code")?,
        responsible_lawyer: facts.try_get("lawyer_name")?,
        service: service_label(claim.work.service),
        benefit: claim.work.benefit,
        first_service_on: first,
        last_service_on: last,
        lines,
        attorney_minutes: minutes,
        rate_cents_per_hour: rate,
        fixed_fee_cents: fixed,
        total_cents: total,
        covered_cents: claim.covered_cents,
        member_share_cents: member_share,
        claimed_cents: claim.claimed_cents,
        proofs,
        missing_proof,
    })
}
