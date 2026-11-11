//! Consuming a journaled Forgejo push receipt: analysis on the row, the
//! forge API only once.
//!
//! [`cloud::forgejo_push_analysis`] reduces a push to the facts
//! the firm reads. This wrapper owns the receipt lifecycle around it, so
//! a redelivery or a post-crash replay never re-reads the forge API: a
//! receipt whose journal row already carries analysis columns answers
//! from the row ([`persisted_analysis`]); a fresh receipt's analysis is
//! persisted with the `analyzed` status
//! ([`super::forgejo_push_webhook_journal::record_analysis`]) before
//! delivery can begin; and a push naming no `push/*` bookmark is marked
//! `skipped` — observable, never analyzed.

use cloud::forgejo_forge::ForgejoForge;
use cloud::forgejo_push_analysis as analysis;

use super::forgejo_push_webhook_journal as journal;
use super::forgejo_push_webhook_journal::ForgejoPushReceipt;
use super::PostgresDb;

/// Why a journaled push receipt could not be consumed.
#[derive(Debug, thiserror::Error)]
pub enum ConsumptionError {
    /// Analysis failed — a malformed ref update or a forge read.
    #[error("analyzing pushed change: {0}")]
    Analysis(#[from] analysis::AnalysisError),
    /// A journal write failed.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// The analysis columns a receipt already carries, as an analysis value.
/// `record_analysis` writes the five columns together, so the row is
/// all-or-nothing: a partially analyzed row reads as unanalyzed and is
/// analyzed again rather than reconstructed from missing parts.
#[must_use]
pub fn persisted_analysis(receipt: &ForgejoPushReceipt) -> Option<journal::PushAnalysis> {
    Some(journal::PushAnalysis {
        change_id: receipt.change_id.as_ref()?.clone(),
        author: receipt.author.as_ref()?.clone(),
        description_summary: receipt.description_summary.as_ref()?.clone(),
        stats: receipt.stats.as_ref()?.clone(),
        conflict_signal: receipt.conflict_signal?,
    })
}

/// The cloud crate's analysis value as the journal persists it — the
/// statistics flattened to the JSON column the delivery stage renders.
fn persist(analysis: &analysis::PushAnalysis) -> journal::PushAnalysis {
    journal::PushAnalysis {
        change_id: analysis.change_id.clone(),
        author: analysis.author.clone(),
        description_summary: analysis.description_summary.clone(),
        stats: serde_json::json!({
            "insertions": analysis.stats.insertions,
            "deletions": analysis.stats.deletions,
            "files": analysis.stats.files,
        }),
        conflict_signal: analysis.conflict_signal,
    }
}

/// Drive a journaled push receipt through analysis. A terminal receipt
/// (`delivered` or `skipped`) re-runs nothing; a receipt already carrying
/// analysis answers from its row; otherwise the push is analyzed, the
/// analysis persisted with the `analyzed` status, and a push naming no
/// `push/*` bookmark is marked `skipped`.
///
/// Returns the analysis when the receipt carries or gained one — the
/// delivery stage's input — and `None` for a terminal or skipped receipt.
///
/// # Errors
/// [`ConsumptionError`] when analysis fails or a journal write fails.
pub async fn consume_receipt(
    db: &PostgresDb,
    forge: &ForgejoForge,
    receipt: &ForgejoPushReceipt,
) -> Result<Option<journal::PushAnalysis>, ConsumptionError> {
    if receipt.status.is_terminal() {
        return Ok(None);
    }
    if let Some(persisted) = persisted_analysis(receipt) {
        return Ok(Some(persisted));
    }
    let Some(analyzed) =
        analysis::analyze_push(forge, &receipt.repo_full_name, &receipt.ref_updates).await?
    else {
        journal::mark_skipped(db, receipt.id).await?;
        return Ok(None);
    };
    let persisted = persist(&analyzed);
    journal::record_analysis(db, receipt.id, &persisted).await?;
    Ok(Some(persisted))
}
