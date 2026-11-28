//! Propose Time Entries from a Project's dated record (LAW-584).
//!
//! Each Communication, filed document, completed Deadline, and e-filing
//! becomes at most one pending proposal, with minutes from the published
//! [`DEFAULT_MINUTES`] table. A source already proposed — pending, approved,
//! or discarded — is skipped, so a re-run adds nothing twice. Nothing is
//! approved here.
use chrono::{NaiveDate, Utc};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use super::{ProposalError, SourceKind};
use crate::postgres::PostgresDb;

/// One row of the published minutes table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DefaultMinutes {
    /// The record the rule reads, for example `email sent`.
    pub kind: &'static str,
    pub minutes: i32,
    pub rule: &'static str,
}

/// The minutes each kind of record proposes. Shown with every backfill.
pub const DEFAULT_MINUTES: &[DefaultMinutes] = &[
    DefaultMinutes {
        kind: "call",
        minutes: 12,
        rule: "the call's stated duration rounded up to a tenth of an hour; 12 when none is stated",
    },
    DefaultMinutes {
        kind: "message sent",
        minutes: 12,
        rule: "an email or message the Firm sent",
    },
    DefaultMinutes {
        kind: "message received",
        minutes: 6,
        rule: "reading an email or message the Project received",
    },
    DefaultMinutes {
        kind: "internal note",
        minutes: 6,
        rule: "a note or comment on the Project",
    },
    DefaultMinutes {
        kind: "document",
        minutes: 30,
        rule: "a document filed on the Project, other than a mail attachment, a DocuSign copy, an invoice, or a receipt",
    },
    DefaultMinutes {
        kind: "deadline",
        minutes: 30,
        rule: "a completed Deadline; its recorded effort when one is set",
    },
    DefaultMinutes {
        kind: "hearing",
        minutes: 60,
        rule: "a completed hearing; its recorded effort when one is set",
    },
    DefaultMinutes {
        kind: "e-filing",
        minutes: 18,
        rule: "an e-filing transaction",
    },
];

fn default_minutes(kind: &str) -> i32 {
    DEFAULT_MINUTES
        .iter()
        .find(|row| row.kind == kind)
        .map_or(6, |row| row.minutes)
}

/// One proposed Time Entry and the record it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub source: SourceKind,
    pub source_id: Uuid,
    pub performed_on: NaiveDate,
    pub minutes: i32,
    pub narrative: String,
    pub timekeeper_id: Uuid,
    /// The proposal written, absent on a dry run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Backfill {
    pub dry_run: bool,
    pub default_minutes: &'static [DefaultMinutes],
    pub proposals: Vec<Candidate>,
}

/// Round minutes up to the next six-minute unit, within one day.
fn tenths(minutes: i64) -> i32 {
    let units = (minutes.max(1) + 5) / 6;
    i32::try_from((units * 6).min(i64::from(crate::time_entries::MAX_MINUTES_PER_DAY)))
        .unwrap_or(crate::time_entries::MAX_MINUTES_PER_DAY)
}

fn clip(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= 200 {
        text
    } else {
        format!("{}…", text.chars().take(199).collect::<String>())
    }
}

fn communication(
    channel: &str,
    direction: &str,
    subject: Option<&str>,
    call_minutes: Option<i64>,
) -> (i32, String) {
    let subject = subject.filter(|subject| !subject.trim().is_empty());
    let about = |prefix: &str| {
        subject.map_or_else(
            || prefix.to_owned(),
            |subject| format!("{prefix}: {}", clip(subject)),
        )
    };
    if channel == crate::communications::channel::CALL {
        let minutes = call_minutes
            .filter(|minutes| *minutes > 0)
            .map_or_else(|| default_minutes("call"), tenths);
        return (minutes, about("Call"));
    }
    if channel == crate::communications::channel::NOTE
        || channel == crate::communications::channel::DOCUMENT_COMMENT
    {
        return (default_minutes("internal note"), about("Internal note"));
    }
    match direction {
        "outbound" => (
            default_minutes("message sent"),
            about("Correspondence sent"),
        ),
        "inbound" => (
            default_minutes("message received"),
            about("Reviewed correspondence"),
        ),
        _ => (default_minutes("internal note"), about("Internal note")),
    }
}

/// The firm-side participant credited with the work: the record's own
/// author or owner when they are firm-side, else the lawyer running the
/// backfill.
async fn timekeeper(
    pg: &PostgresDb,
    project: Uuid,
    candidate: Option<Uuid>,
    actor: Uuid,
) -> Result<Uuid, ProposalError> {
    if let Some(person) = candidate {
        if super::require_timekeeper(pg, project, person).await.is_ok() {
            return Ok(person);
        }
    }
    super::require_timekeeper(pg, project, actor).await?;
    Ok(actor)
}

#[allow(clippy::too_many_lines)] // One read per kind of dated record.
async fn candidates(
    pg: &PostgresDb,
    project: Uuid,
    since: Option<NaiveDate>,
    actor: Uuid,
) -> Result<Vec<Candidate>, ProposalError> {
    let today = Utc::now().date_naive();
    let mut found = Vec::new();

    for row in sqlx::query(
        "SELECT c.id,timezone('UTC',c.occurred_at)::date AS day,c.channel,c.direction,c.subject,
         c.author_person_id,(c.call->>'duration_minutes')::bigint AS call_minutes
         FROM communication c WHERE c.project_id=$1
         AND ($2::date IS NULL OR timezone('UTC',c.occurred_at)::date >= $2) AND NOT EXISTS (SELECT 1 FROM time_entry_proposal p WHERE p.project_id=$1
         AND p.source_kind='communication' AND p.source_id=c.id)",
    )
    .bind(project)
    .bind(since)
    .fetch_all(&pg.0)
    .await?
    {
        let channel: String = row.try_get("channel")?;
        let direction: String = row.try_get("direction")?;
        let subject: Option<String> = row.try_get("subject")?;
        let (minutes, narrative) = communication(
            &channel,
            &direction,
            subject.as_deref(),
            row.try_get("call_minutes")?,
        );
        found.push(Candidate {
            source: SourceKind::Communication,
            source_id: row.try_get("id")?,
            performed_on: row.try_get("day")?,
            minutes,
            narrative,
            timekeeper_id: timekeeper(pg, project, row.try_get("author_person_id")?, actor).await?,
            proposal_id: None,
        });
    }

    // Mail attachments are read with their message, DocuSign copies are the
    // signing record of work already counted, and invoices and receipts are
    // billing, not work on the matter.
    for row in sqlx::query(
        "SELECT a.id,timezone('UTC',a.inserted_at)::date AS day,coalesce(a.slug,a.filename,a.kind) AS name
         FROM asset a WHERE a.project_id=$1 AND a.deleted_at IS NULL
         AND a.source IS DISTINCT FROM 'docusign' AND a.source_message_id IS NULL
         AND coalesce(a.kind,'') NOT IN ('invoice','receipt')
         AND NOT EXISTS (SELECT 1 FROM communication c WHERE c.asset_id=a.id)
         AND NOT EXISTS (SELECT 1 FROM inbox_message_asset m WHERE m.asset_id=a.id)
         AND ($2::date IS NULL OR timezone('UTC',a.inserted_at)::date >= $2) AND NOT EXISTS (SELECT 1 FROM time_entry_proposal p WHERE p.project_id=$1
         AND p.source_kind='document' AND p.source_id=a.id)",
    )
    .bind(project)
    .bind(since)
    .fetch_all(&pg.0)
    .await?
    {
        let name: Option<String> = row.try_get("name")?;
        found.push(Candidate {
            source: SourceKind::Document,
            source_id: row.try_get("id")?,
            performed_on: row.try_get("day")?,
            minutes: default_minutes("document"),
            narrative: format!(
                "Prepared and filed {}",
                clip(name.as_deref().unwrap_or("a document"))
            ),
            timekeeper_id: timekeeper(pg, project, None, actor).await?,
            proposal_id: None,
        });
    }

    for row in sqlx::query(
        "SELECT d.id,timezone('UTC',d.due_at)::date AS day,d.category,d.effort_minutes,
         coalesce(d.title,d.authority) AS name,d.owner_person_id
         FROM project_deadlines d WHERE d.project_id=$1 AND d.status='complete'
         AND ($2::date IS NULL OR timezone('UTC',d.due_at)::date >= $2) AND NOT EXISTS (SELECT 1 FROM time_entry_proposal p WHERE p.project_id=$1
         AND p.source_kind='deadline' AND p.source_id=d.id)",
    )
    .bind(project)
    .bind(since)
    .fetch_all(&pg.0)
    .await?
    {
        let category: Option<String> = row.try_get("category")?;
        let hearing = category.as_deref() == Some("hearing");
        let effort: i32 = row.try_get("effort_minutes")?;
        let name: String = row.try_get("name")?;
        let day: NaiveDate = row.try_get("day")?;
        found.push(Candidate {
            source: SourceKind::Deadline,
            source_id: row.try_get("id")?,
            performed_on: day.min(today),
            minutes: if effort > 0 {
                tenths(i64::from(effort))
            } else {
                default_minutes(if hearing { "hearing" } else { "deadline" })
            },
            narrative: format!(
                "{}: {}",
                if hearing {
                    "Attended hearing"
                } else {
                    "Completed"
                },
                clip(&name)
            ),
            timekeeper_id: timekeeper(pg, project, row.try_get("owner_person_id")?, actor).await?,
            proposal_id: None,
        });
    }

    for row in sqlx::query(
        "SELECT e.id,coalesce(CASE WHEN e.filed_on ~ '^\\d{4}-\\d{2}-\\d{2}$' THEN e.filed_on::date END,
           timezone('UTC',e.inserted_at)::date) AS day,e.court,e.note
         FROM efiling_transactions e WHERE e.project_id=$1 AND NOT EXISTS (SELECT 1 FROM time_entry_proposal p WHERE p.project_id=$1
         AND p.source_kind='efiling' AND p.source_id=e.id)",
    )
    .bind(project)
    .fetch_all(&pg.0)
    .await?
    {
        let day: NaiveDate = row.try_get("day")?;
        if since.is_some_and(|since| day < since) {
            continue;
        }
        let court: String = row.try_get("court")?;
        let note: String = row.try_get("note")?;
        let note = note.trim();
        found.push(Candidate {
            source: SourceKind::Efiling,
            source_id: row.try_get("id")?,
            performed_on: day.min(today),
            minutes: default_minutes("e-filing"),
            narrative: if note.is_empty() {
                format!("E-filed with {}", clip(&court))
            } else {
                format!("E-filed with {}: {}", clip(&court), clip(note))
            },
            timekeeper_id: timekeeper(pg, project, None, actor).await?,
            proposal_id: None,
        });
    }
    found.sort_by(|a, b| {
        (a.performed_on, a.source.as_str(), a.source_id).cmp(&(
            b.performed_on,
            b.source.as_str(),
            b.source_id,
        ))
    });
    Ok(found)
}

/// Propose Time Entries from the Project's dated record. A dry run writes
/// nothing; a real run writes each proposal `pending_review`, never approved.
///
/// # Errors
/// [`ProposalError::NotParticipant`] when the caller is not firm-side on the
/// Project, or a database failure.
pub async fn backfill(
    pg: &PostgresDb,
    project: Uuid,
    actor: Uuid,
    since: Option<NaiveDate>,
    dry_run: bool,
) -> Result<Backfill, ProposalError> {
    super::require_timekeeper(pg, project, actor).await?;
    let mut proposals = candidates(pg, project, since, actor).await?;
    if !dry_run {
        let mut tx = pg.0.begin().await?;
        let mut written = Vec::with_capacity(proposals.len());
        for mut candidate in proposals {
            // The source is the retry key: a concurrent run writes it once.
            let id: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO time_entry_proposal (id,project_id,timekeeper_id,performed_on,minutes,narrative,
                 source_kind,source_id,request_id,status,created_by)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$1,'pending_review',$9)
                 ON CONFLICT DO NOTHING RETURNING id",
            )
            .bind(Uuid::now_v7())
            .bind(project)
            .bind(candidate.timekeeper_id)
            .bind(candidate.performed_on)
            .bind(candidate.minutes)
            .bind(&candidate.narrative)
            .bind(candidate.source.as_str())
            .bind(candidate.source_id)
            .bind(actor)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(id) = id {
                candidate.proposal_id = Some(id);
                written.push(candidate);
            }
        }
        tx.commit().await?;
        proposals = written;
    }
    Ok(Backfill {
        dry_run,
        default_minutes: DEFAULT_MINUTES,
        proposals,
    })
}

#[cfg(test)]
mod unit {
    use super::{communication, tenths};

    #[test]
    fn a_stated_call_duration_rounds_up_to_a_tenth_of_an_hour() {
        assert_eq!(tenths(1), 6);
        assert_eq!(tenths(6), 6);
        assert_eq!(tenths(7), 12);
        assert_eq!(tenths(10_000), 1_440);
        assert_eq!(communication("call", "internal", None, Some(25)).0, 30);
        assert_eq!(communication("call", "internal", None, None).0, 12);
    }

    #[test]
    fn each_message_direction_reads_its_published_minutes() {
        assert_eq!(
            communication("email_outbound", "outbound", Some("Status  update"), None),
            (12, "Correspondence sent: Status update".to_owned())
        );
        assert_eq!(
            communication("email_inbound", "inbound", None, None),
            (6, "Reviewed correspondence".to_owned())
        );
        assert_eq!(
            communication("note", "internal", Some(" "), None).1,
            "Internal note"
        );
    }
}
