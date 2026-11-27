//! Source links for the information a Notation is assembled from.
//!
//! Email, text messages, and conversations first enter the privileged
//! [`crate::communications`] log. This satellite records only their identity,
//! never a copied message body. Questionnaire sources have no communication
//! row and use their stable questionnaire reference instead.

use crate::postgres::matter;
use serde_json::json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;

const TABLE: &str = "notation_source";

/// Kinds of intake Navigator can trace into a Notation.
pub mod kind {
    pub const EMAIL: &str = "email";
    pub const TEXT_MESSAGE: &str = "text_message";
    pub const CONVERSATION: &str = "conversation";
    pub const QUESTIONNAIRE: &str = "questionnaire";
}

/// Link one source to a notation in lawyer-review order.
#[derive(Debug, Clone)]
pub struct NewNotationSource {
    pub notation_id: Uuid,
    pub kind: String,
    pub communication_id: Option<Uuid>,
    /// A question code for questionnaire facts or an external source id.
    pub source_ref: Option<String>,
    pub position: i32,
}

/// Persist a source link without copying privileged intake content.
///
/// # Errors
///
/// Propagates database failures, including duplicate review positions.
pub async fn attach(
    _db: &ApplicationStore,
    source: &NewNotationSource,
) -> Result<(), crate::postgres::matter::MatterAuthorityError> {
    let pg = matter::authority_for(TABLE).await?;
    let now = chrono::Utc::now();
    let _: serde_json::Value = matter::upsert(
        &pg,
        TABLE,
        &json!({
            "id": Uuid::now_v7(), "notation_id": source.notation_id, "kind": source.kind,
            "communication_id": source.communication_id, "source_ref": source.source_ref,
            "position": source.position, "inserted_at": now, "updated_at": now
        }),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {}
