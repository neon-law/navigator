//! `hudson site document slugless` — repair the live rows an inbound email
//! thread filed without a slug.
//!
//! `project gate --check` fails a Project on every live row with no slug.
//! The repair reads the matter's live documents and, for each slugless row
//! an inbound email filed, plans one of three outcomes:
//!
//! - **retire** a signature logo or inline image, and any row whose bytes
//!   another live row already holds — a soft-delete, recoverable for the
//!   retention window, never a purge;
//! - **slug** every other row as `mail/<asset id>/<YYYY.MM.DD>_<name>-0.<ext>`,
//!   dated by the day it was filed;
//! - **skip** a row the repair cannot name (no extension, an unreadable
//!   date), or one another channel filed, for an operator to slug by hand.
//!
//! The dry run validates every planned slug against the server and writes
//! nothing.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use serde::Serialize;
use uuid::Uuid;

use crate::remote::{DocumentClient, LiveDocumentSummary};

/// The `assets.source` an inbound email ingest writes.
const EMAIL_SOURCE: &str = "email";

/// One slugless row and what the repair does with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub asset_id: Uuid,
    pub filename: String,
    pub action: Action,
    /// The slug the row gets, for [`Action::Slug`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Slug,
    Retire,
    Skip,
}

/// Plan the repair for `documents`, a matter's live rows. Oldest first, so
/// the first copy of repeated bytes is the one kept; a slugged row always
/// wins over a slugless copy of its bytes.
#[must_use]
pub fn plan(documents: &[LiveDocumentSummary]) -> Vec<Step> {
    let mut holders: HashMap<&str, Uuid> = documents
        .iter()
        .filter(|document| {
            document
                .slug
                .as_deref()
                .is_some_and(|slug| !slug.is_empty())
        })
        .map(|document| (document.sha256_hex.as_str(), document.id))
        .collect();
    let mut slugless: Vec<&LiveDocumentSummary> = documents
        .iter()
        .filter(|document| document.slug.as_deref().is_none_or(str::is_empty))
        .collect();
    slugless.sort_by_key(|document| document.id);

    let mut steps = Vec::with_capacity(slugless.len());
    for document in slugless {
        let filename = document.filename.clone().unwrap_or_default();
        let step = |action, slug, reason: String| Step {
            asset_id: document.id,
            filename: filename.clone(),
            action,
            slug,
            reason,
        };
        if document.source.as_deref() != Some(EMAIL_SOURCE) {
            steps.push(step(
                Action::Skip,
                None,
                "not filed from inbound email; slug it with `hudson site document slug`".into(),
            ));
            continue;
        }
        let size = usize::try_from(document.byte_size).unwrap_or(usize::MAX);
        if model::mail_attachments::is_inline_image(&filename, "", size) {
            steps.push(step(
                Action::Retire,
                None,
                "inline or signature image".into(),
            ));
            continue;
        }
        if let Some(holder) = holders.get(document.sha256_hex.as_str()) {
            steps.push(step(
                Action::Retire,
                None,
                format!("same bytes as document {holder}"),
            ));
            continue;
        }
        if model::mail_attachments::filename_with_extension(&filename) != filename {
            steps.push(step(
                Action::Skip,
                None,
                "filename has no extension a slug can carry".into(),
            ));
            continue;
        }
        let Ok(inserted_at) = chrono::DateTime::parse_from_rfc3339(&document.inserted_at) else {
            steps.push(step(
                Action::Skip,
                None,
                format!("unreadable filing time `{}`", document.inserted_at),
            ));
            continue;
        };
        let slug = model::mail_attachments::slug(
            &document.id.to_string(),
            0,
            &filename,
            inserted_at.date_naive(),
        );
        holders.insert(document.sha256_hex.as_str(), document.id);
        steps.push(step(
            Action::Slug,
            Some(slug),
            "inbound email attachment".into(),
        ));
    }
    steps
}

/// Plan the repair against the live matter and, unless `dry_run`, apply it.
/// The dry run asks the server to validate each planned slug and writes
/// nothing; a real run needs `yes`.
///
/// # Errors
/// A listing, validation, or write failure, or a real run without `yes`.
pub async fn repair(
    client: &DocumentClient,
    dry_run: bool,
    yes: bool,
) -> Result<serde_json::Value> {
    if !dry_run && !yes {
        return Err(anyhow!(
            "retiring and slugging rows needs an explicit confirmation; review --dry-run, then pass --yes"
        ));
    }
    let steps = plan(&client.list_documents().await?);
    for step in &steps {
        match (step.action, step.slug.as_deref()) {
            (Action::Slug, Some(slug)) => {
                client
                    .assign_slug(step.asset_id, slug, None, dry_run)
                    .await?;
            }
            (Action::Retire, _) if !dry_run => {
                client.retire(step.asset_id).await?;
            }
            _ => {}
        }
    }
    let count = |action| steps.iter().filter(|step| step.action == action).count();
    Ok(serde_json::json!({
        "dry_run": dry_run,
        "slugged": count(Action::Slug),
        "retired": count(Action::Retire),
        "skipped": count(Action::Skip),
        "steps": steps,
    }))
}

#[cfg(test)]
mod tests {
    use super::{plan, Action};
    use crate::remote::LiveDocumentSummary;
    use uuid::Uuid;

    fn row(filename: &str, sha: &str, size: i64, slug: Option<&str>) -> LiveDocumentSummary {
        LiveDocumentSummary {
            metadata: None,
            id: Uuid::now_v7(),
            byte_size: size,
            sha256_hex: sha.repeat(64),
            filename: Some(filename.into()),
            kind: Some("unclassified".into()),
            visibility: "internal".into(),
            slug: slug.map(str::to_string),
            inserted_at: "2026-11-12T23:20:05.123Z".into(),
            source: Some("email".into()),
            signing: None,
            notarization: Vec::new(),
            folder: None,
        }
    }

    #[test]
    fn signatures_and_repeated_bytes_retire_and_the_first_copy_is_slugged() {
        let filed = row("Notice.pdf", "a", 90_000, Some("Correspondence/notice.pdf"));
        let refiled = row("notice (1).pdf", "a", 90_000, None);
        let logo = row("Email Signature.png", "b", 120_000, None);
        let first = row("Intake Form.pdf", "c", 50_000, None);
        let second = row("Intake Form.pdf", "c", 50_000, None);
        let mut upload = row("scan.pdf", "d", 50_000, None);
        upload.source = Some("upload".into());
        let bare = row("README", "e", 50_000, None);

        let steps = plan(&[
            filed.clone(),
            refiled.clone(),
            logo.clone(),
            first.clone(),
            second.clone(),
            upload.clone(),
            bare.clone(),
        ]);
        let action_of = |id| steps.iter().find(|step| step.asset_id == id).unwrap();

        assert_eq!(steps.len(), 6, "the slugged row is not touched");
        assert_eq!(action_of(refiled.id).action, Action::Retire);
        assert!(action_of(refiled.id).reason.contains(&filed.id.to_string()));
        assert_eq!(action_of(logo.id).action, Action::Retire);
        assert_eq!(action_of(first.id).action, Action::Slug);
        assert_eq!(
            action_of(first.id).slug.as_deref(),
            Some(format!("mail/{}/2026.11.12_intake-form-0.pdf", first.id).as_str())
        );
        assert_eq!(action_of(second.id).action, Action::Retire);
        assert!(action_of(second.id).reason.contains(&first.id.to_string()));
        assert_eq!(action_of(upload.id).action, Action::Skip);
        assert_eq!(action_of(bare.id).action, Action::Skip);
    }

    #[test]
    fn an_unreadable_filing_time_is_skipped_not_guessed() {
        let mut document = row("letter.pdf", "f", 50_000, None);
        document.inserted_at = "yesterday".into();
        let steps = plan(&[document]);
        assert_eq!(steps[0].action, Action::Skip);
    }
}
