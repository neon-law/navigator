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
/// Complete native cohosting defaults off; mixed and standalone hosts retain
/// their existing default. Explicit activation still uses the strict flag parser.
///
/// # Errors
/// Returns a malformed timer or engine-selector key without its value.
pub fn schedule_enabled(
    name: &'static str,
    get: &impl Fn(&str) -> Option<String>,
) -> Result<bool, SettingsError> {
    let native =
        store::workflow_engine::dbos_only(get).map_err(|error| SettingsError::Invalid(error.0))?;
    store::workflow_engine::missing_dbos_flags(get)
        .map_err(|error| SettingsError::Invalid(error.0))?;
    let value = get(name);
    let default = if native { "false" } else { "true" };
    flag_enabled(name, value.as_deref().or(Some(default)))
}

#[cfg(test)]
mod tests {
    use super::{flag_enabled, SettingsError};

    #[test]
    fn automatic_timers_default_off_only_for_complete_native_ownership() {
        use store::workflow_engine as engine;
        let native = |key: &str| match key {
            engine::COHOST_ENV | engine::DBOS_ENABLED_ENV => Some("true".into()),
            engine::ENGINE_ENV => Some("dbos".into()),
            _ if engine::WORKFLOWS
                .iter()
                .any(|selection| selection.flag == key) =>
            {
                Some("true".into())
            }
            _ => None,
        };
        for key in [
            super::BILLING_CANARY_SCHEDULE_ENABLED_ENV,
            super::RECONCILE_INVOICES_SCHEDULE_ENABLED_ENV,
        ] {
            assert!(!super::schedule_enabled(key, &native).unwrap());
            assert!(super::schedule_enabled(key, &|_| None).unwrap());
            assert!(
                super::schedule_enabled(key, &|name| if name == engine::COHOST_ENV {
                    None
                } else {
                    native(name)
                })
                .unwrap()
            );
            for selection in engine::WORKFLOWS {
                let partial = |name: &str| {
                    if name == selection.flag {
                        Some("false".into())
                    } else {
                        native(name)
                    }
                };
                assert!(
                    super::schedule_enabled(key, &partial).unwrap(),
                    "{} keeps the mixed default",
                    selection.name
                );
            }
            for (value, expected) in [("true", true), ("false", false)] {
                assert_eq!(
                    super::schedule_enabled(key, &|name| if name == key {
                        Some(value.into())
                    } else {
                        native(name)
                    })
                    .unwrap(),
                    expected
                );
            }
            assert_eq!(
                super::schedule_enabled(key, &|name| if name == key {
                    Some("invalid".into())
                } else {
                    native(name)
                }),
                Err(SettingsError::Invalid(key))
            );
            let selection = engine::WORKFLOWS[0].flag;
            assert_eq!(
                super::schedule_enabled(key, &|name| if name == selection {
                    Some("invalid".into())
                } else {
                    None
                }),
                Err(SettingsError::Invalid(selection))
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
