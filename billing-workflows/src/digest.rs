//! Restate adapter for the shared `BillingDigest` core.

use serde::{Deserialize, Serialize};

pub use workflows::billing_digest::{
    build_digest_email, AccountCost, BillingDigestReport, DigestOutcome,
};

/// Request body for `BillingDigest::run`; the trigger supplies no fields.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RunRequest {}
