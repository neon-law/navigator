//! `store::notation_drafts` — a stored, addressable notation **draft**
//! (LAW-29): a preview artifact, not an executed instrument.
//!
//! `navigator notation preview` pushes a template to the Project it
//! belongs to, so an author can open the *real* portal — production
//! chrome, production questionnaire engine — against it. Since LAW-203 the
//! push also opens a real notation run beside the draft, so the preview is
//! answerable; the draft row carries the [`crate::notations::Notation`] it
//! opened when one opened, and carries none when the push predates the
//! link or the run could not open.
//!
//! Creating a draft row **deliberately never**:
//! * creates a [`crate::notations::Notation`] row,
//! * starts a [`workflows`] state-machine instance, or
//! * journals a [`crate::notation_events`] event (`intake_submitted`
//!   included).
//!
//! Opening the run is the *door's* job (`portal::notation_draft`), not the
//! store write's. A draft is a preview, not a filing: no PDF, no
//! signature. It carries a short [`TTL`] rather than accumulating.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::matter::{self, MatterAuthorityError};

pub const TABLE: &str = "notation_draft";

/// How long a draft stays reachable. Drafts are a preview artifact with a
/// short life, not a place to accumulate — a reaper can delete rows past
/// this age, and [`find_live`] already treats them as gone.
pub const TTL: Duration = Duration::hours(24);

/// A stored, addressable notation preview. Since LAW-203 a preview push
/// opens a real notation run beside the draft; `notation_id` carries the
/// run the draft opened, and is `None` for a draft whose push predates the
/// link or whose run could not open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationDraft {
    pub id: Uuid,
    pub project_id: Uuid,
    /// The `{slug}` the draft is addressed at — the same kebab-case a
    /// published notation's URL would use.
    pub slug: String,
    pub title: String,
    /// The template's raw Markdown source (frontmatter and body), exactly
    /// as [`webapp::notation_preview::PreviewDoc`] parses a preview from.
    pub source: String,
    /// The notation run this draft opened, when it opened one.
    pub notation_id: Option<Uuid>,
    pub inserted_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Errors from the notation-draft commands.
#[derive(Debug, thiserror::Error)]
pub enum NotationDraftError {
    #[error(transparent)]
    Postgres(#[from] MatterAuthorityError),
    /// A write reported success but returned no row this module could read
    /// back.
    #[error("writing a notation draft returned no usable row")]
    WriteReturnedNothing,
}

/// What a new [`NotationDraft`] needs.
#[derive(Debug, Clone)]
pub struct NewNotationDraft<'a> {
    pub project_id: Uuid,
    pub slug: &'a str,
    pub title: &'a str,
    pub source: &'a str,
    /// The notation run this draft opened, when the caller opened one
    /// beside it. `None` for a run-less draft.
    pub notation_id: Option<Uuid>,
}

/// Store a notation draft.
///
/// # Errors
/// Propagates any database error.
pub async fn create(
    _db: &ApplicationStore,
    new: &NewNotationDraft<'_>,
) -> Result<NotationDraft, NotationDraftError> {
    let id = Uuid::now_v7();
    let inserted_at = Utc::now();
    let expires_at = inserted_at + TTL;
    let pg = matter::authority().await?;
    let draft = NotationDraft {
        id,
        project_id: new.project_id,
        slug: new.slug.to_owned(),
        title: new.title.to_owned(),
        source: new.source.to_owned(),
        notation_id: new.notation_id,
        inserted_at,
        expires_at,
    };
    Ok(matter::upsert(&pg, TABLE, &draft).await?)
}

/// Fetch a draft by id — `None` when it does not exist *or has expired*,
/// so an expired draft's door 404s exactly like an unknown one rather than
/// serving stale content past its stated life. The row itself is left
/// alone; expiry is a read-time filter, not a delete, so a reaper (or an
/// operator) can still find and clean up what expired.
///
/// # Errors
/// Propagates any database error.
pub async fn find_live(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<NotationDraft>, NotationDraftError> {
    let draft: Option<NotationDraft> = {
        let pg = matter::authority().await?;
        matter::find(&pg, TABLE, serde_json::json!({"id": id})).await?
    };
    Ok(draft.filter(|draft| draft.expires_at > Utc::now()))
}
