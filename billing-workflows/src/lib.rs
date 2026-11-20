//! Shared billing workflow cores and opt-in Restate replay adapters.
//!
//! Native DBOS callers use the provider-agnostic billing functions. The
//! `restate-compatibility` feature exposes the worker adapters for retained runs.
//! Provider and store effects remain in the shared billing functions.

pub mod canary;
pub mod digest;
pub mod reconcile;
pub mod sandbox_reset;

#[cfg(feature = "restate-compatibility")]
pub use canary::BillingCanaryService;
pub use canary::{run_canary, CanaryReport, RunRequest};
pub use digest::{build_digest_email, AccountCost, BillingDigestReport, DigestOutcome};
pub use reconcile::{reconcile_once, ReconcileReport, ReconcileRequest};
pub use sandbox_reset::{overage_invoice_key, SandboxResetReport, SandboxResetRequest};

#[cfg(feature = "restate-compatibility")]
pub use digest::BillingDigestService;
#[cfg(feature = "restate-compatibility")]
pub use reconcile::ReconcileInvoicesService;
#[cfg(feature = "restate-compatibility")]
pub use sandbox_reset::SandboxCreditResetService;
