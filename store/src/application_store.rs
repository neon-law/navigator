//! PostgreSQL connection carried by application state and workflow providers.

use crate::postgres::{PostgresDb, PostgresError};

#[derive(Clone, Debug)]
pub struct ApplicationStore(Option<PostgresDb>);

impl ApplicationStore {
    /// Admit the application's configured PostgreSQL database.
    pub async fn connect() -> Result<Self, PostgresError> {
        PostgresDb::application().await.map(Self::from)
    }

    /// Resolve the database for this operation and verify its application history.
    pub async fn postgres(&self) -> Result<PostgresDb, PostgresError> {
        match &self.0 {
            Some(postgres) => {
                postgres.require_application_history().await?;
                Ok(postgres.clone())
            }
            None => PostgresDb::application().await,
        }
    }

    /// Construct connection-free state for tests whose handlers do not use storage.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub const fn uninitialized() -> Self {
        Self(None)
    }
}

impl From<PostgresDb> for ApplicationStore {
    fn from(postgres: PostgresDb) -> Self {
        Self(Some(postgres))
    }
}
