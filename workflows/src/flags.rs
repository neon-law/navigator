//! The per-workflow DBOS switches, readable without the `dbos` feature.
//!
//! Every caller of a ported service — `web`, the thin trigger binaries, the
//! worker's own Restate handlers — reads the same strict `true` / `false`
//! switch to choose between the worker's DBOS ingress and the Restate
//! ingress. Those callers do not host the executor, and several crates
//! depend on `workflows` without its `dbos` feature, so the parser lives
//! outside the feature gate. `dbos_engine` re-exports both items so the
//! established `workflows::dbos_engine::flag_enabled` path keeps resolving.

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

#[cfg(test)]
mod tests {
    use super::{flag_enabled, SettingsError};

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
