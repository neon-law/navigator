//! Shared billing cores and explicit recovery adapters.
//!
//! Application hosts disable billing automation on both engines. Retained trigger
//! entrypoints are inert. The opt-in adapters preserve recovery access while
//! operators disposition retained executions and provider effects.

pub mod canary;
pub mod digest;
pub mod reconcile;
pub mod sandbox_reset;

pub use canary::{run_canary, CanaryReport, RunRequest};
pub use digest::{build_digest_email, AccountCost, BillingDigestReport, DigestOutcome};
pub use reconcile::{reconcile_once, ReconcileReport, ReconcileRequest};
pub use sandbox_reset::{overage_invoice_key, SandboxResetReport, SandboxResetRequest};
