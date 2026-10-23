//! Which durable engine this worker process runs.
//!
//! `NAVIGATOR_WORKFLOW_ENGINE` unset or `restate` serves the Restate endpoint
//! for every service still on Restate. `dbos` also runs the DBOS executor here
//! ([`workflows::dbos_engine::serve`]) while keeping that endpoint available,
//! so unported Restate services remain reachable during migration.

/// The variable selecting the durable engine.
pub const ENGINE_ENV: &str = "NAVIGATOR_WORKFLOW_ENGINE";

/// Which durable engine the worker runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Restate,
    Dbos,
}

/// An engine selection the worker does not know.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{ENGINE_ENV} must be `restate` or `dbos`")]
pub struct UnknownEngine;

impl Engine {
    /// Read the engine from `lookup`. Unset or empty is Restate.
    ///
    /// # Errors
    ///
    /// Refuses any value other than `restate` or `dbos`.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, UnknownEngine> {
        match lookup(ENGINE_ENV).as_deref().map(str::trim) {
            None | Some("" | "restate") => Ok(Self::Restate),
            Some("dbos") => Ok(Self::Dbos),
            Some(_) => Err(UnknownEngine),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Engine, UnknownEngine, ENGINE_ENV};

    fn lookup(value: Option<&'static str>) -> impl Fn(&str) -> Option<String> {
        move |key| {
            (key == ENGINE_ENV)
                .then_some(value)
                .flatten()
                .map(str::to_owned)
        }
    }

    #[test]
    fn restate_is_the_default_engine() {
        assert_eq!(Engine::from_lookup(lookup(None)), Ok(Engine::Restate));
        assert_eq!(
            Engine::from_lookup(lookup(Some("restate"))),
            Ok(Engine::Restate)
        );
        assert_eq!(Engine::from_lookup(lookup(Some("dbos"))), Ok(Engine::Dbos));
        assert_eq!(
            Engine::from_lookup(lookup(Some("temporal"))),
            Err(UnknownEngine)
        );
    }
}
