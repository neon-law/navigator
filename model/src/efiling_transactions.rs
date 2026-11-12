//! E-filing transaction statuses and the transaction id rule.

const MAX_TRANSACTION_ID: usize = 128;

pub const STATUS_SUBMITTED: &str = "submitted";

pub const STATUS_ACCEPTED: &str = "accepted";

pub const STATUS_REJECTED: &str = "rejected";

pub const STATUS_UNKNOWN: &str = "unknown";

/// Trim and check a transaction id: non-empty, no whitespace, bounded.
///
/// # Errors
/// The rule the id broke, as a sentence.
pub fn normalize_transaction_id(raw: &str) -> Result<String, String> {
    let id = raw.trim();
    if id.is_empty() {
        return Err("transaction_id is required".into());
    }
    if id.len() > MAX_TRANSACTION_ID || id.chars().any(char::is_whitespace) {
        return Err(format!(
            "transaction_id must be at most {MAX_TRANSACTION_ID} characters with no spaces"
        ));
    }
    Ok(id.to_string())
}
