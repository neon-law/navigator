//! The per-workflow DBOS switches, readable without the `dbos` feature.
//!
//! Every caller of a ported service — `web`, the thin trigger binaries, the
//! worker's own Restate handlers — reads the same strict `true` / `false`
//! switch to choose between the worker's DBOS ingress and the Restate
//! ingress. Those callers do not host the executor, and several crates
//! depend on `workflows` without its `dbos` feature, so the parser lives
//! outside the feature gate. `dbos_engine` re-exports both items so the
//! established `workflows::dbos_engine::flag_enabled` path keeps resolving.

/// Automatic weekly starts, independent of BillingCanary's native registration.
pub const BILLING_CANARY_SCHEDULE_ENABLED_ENV: &str =
    "NAVIGATOR_DBOS_BILLING_CANARY_SCHEDULE_ENABLED";
/// Automatic daily starts, independent of invoice reconciliation's native registration.
pub const RECONCILE_INVOICES_SCHEDULE_ENABLED_ENV: &str =
    "NAVIGATOR_DBOS_RECONCILE_INVOICES_SCHEDULE_ENABLED";

/// A DBOS switch or connection the environment does not complete.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("{0} is required to run DBOS")]
    Missing(&'static str),
    #[error("{0} is not valid for DBOS")]
    Invalid(&'static str),
}

/// Parse one workflow-specific switch with the same strict `true` / `false`
/// contract as the executor switch.
///
/// # Errors
///
/// Refuses any value other than `true` or `false`, so a typo cannot silently
/// leave the switch off.
pub fn flag_enabled(name: &'static str, value: Option<&str>) -> Result<bool, SettingsError> {
    match value.map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(SettingsError::Invalid(name)),
    }
}

/// Resolve an automatic timer independently of native handler registration.
/// Automatic starts default off independently of handler registration or topology.
/// Explicit activation still uses the strict flag parser.
///
/// # Errors
/// Returns a malformed timer or engine-selector key without its value.
pub fn schedule_enabled(
    name: &'static str,
    get: &impl Fn(&str) -> Option<String>,
) -> Result<bool, SettingsError> {
    let value = get(name);
    flag_enabled(name, value.as_deref())
}

#[cfg(test)]
mod tests {
    use super::{flag_enabled, SettingsError};

    #[test]
    fn billing_timers_default_off_independently_of_workflow_ownership() {
        for key in [
            super::BILLING_CANARY_SCHEDULE_ENABLED_ENV,
            super::RECONCILE_INVOICES_SCHEDULE_ENABLED_ENV,
        ] {
            assert!(!super::schedule_enabled(key, &|_| None).unwrap());
            // A timer reader must not consult or validate retired engine selectors.
            assert!(!super::schedule_enabled(key, &|name| {
                assert_eq!(name, key);
                None
            })
            .unwrap());
            for (value, expected) in [("true", true), ("false", false)] {
                assert_eq!(
                    super::schedule_enabled(key, &|name| {
                        assert_eq!(name, key);
                        Some(value.into())
                    })
                    .unwrap(),
                    expected
                );
            }
            assert_eq!(
                super::schedule_enabled(key, &|_| Some("invalid".into())),
                Err(SettingsError::Invalid(key))
            );
        }
    }

    #[test]
    fn flag_enabled_is_strict() {
        assert!(!flag_enabled("FLAG", None).unwrap_or(true));
        assert!(!flag_enabled("FLAG", Some("")).unwrap_or(true));
        assert!(!flag_enabled("FLAG", Some(" false ")).unwrap_or(true));
        assert!(flag_enabled("FLAG", Some("true")).unwrap_or(false));
        assert_eq!(
            flag_enabled("FLAG", Some("yes")),
            Err(SettingsError::Invalid("FLAG"))
        );
    }
}
