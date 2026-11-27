//! Disabled billing workflows and their retired configuration keys.

/// Disabled billing job and its historical toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowSelection {
    pub name: &'static str,
    pub flag: &'static str,
}

/// Disabled billing automations are neither DBOS requirements nor Restate fallbacks.
/// Their names fence stale callers while retained executions receive operator disposition.
pub const DISABLED_BILLING: &[WorkflowSelection] = &[
    WorkflowSelection {
        name: "BillingCanary",
        flag: "NAVIGATOR_DBOS_BILLING_CANARY_ENABLED",
    },
    WorkflowSelection {
        name: "BillingDigest",
        flag: "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED",
    },
    WorkflowSelection {
        name: "ReconcileInvoices",
        flag: "NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED",
    },
    WorkflowSelection {
        name: "SandboxCreditReset",
        flag: "NAVIGATOR_DBOS_SANDBOX_CREDIT_RESET_ENABLED",
    },
];

#[must_use]
pub fn disabled_billing_service(service: &str) -> bool {
    DISABLED_BILLING.iter().any(|row| row.name == service)
}

#[must_use]
pub fn disabled_billing_flag(flag: &str) -> bool {
    DISABLED_BILLING.iter().any(|row| row.flag == flag)
}
