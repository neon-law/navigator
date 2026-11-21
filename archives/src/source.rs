//! Scheduled archive source selection and PostgreSQL admission.

#[cfg(any(test, feature = "restate-compatibility"))]
use anyhow::{bail, Result};
use store::postgres::{PostgresDb, PostgresError};

#[derive(Clone, Copy)]
pub(crate) enum ArchiveSource {
    Postgres,
    #[cfg(any(test, feature = "restate-compatibility"))]
    All,
}

impl ArchiveSource {
    #[cfg(any(test, feature = "restate-compatibility"))]
    pub(crate) fn from_legacy_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        match get("NAVIGATOR_ARCHIVE_SOURCE").as_deref() {
            None | Some("postgres") => Ok(Self::Postgres),
            Some("all") => Ok(Self::All),
            Some(_) => bail!("NAVIGATOR_ARCHIVE_SOURCE must be postgres or all"),
        }
    }

    pub(crate) async fn postgres(self) -> Result<PostgresDb, PostgresError> {
        match self {
            Self::Postgres => PostgresDb::application().await,
            // Explicit compatibility exports remain available during migration
            // and recovery, before normal application admission is possible.
            #[cfg(any(test, feature = "restate-compatibility"))]
            Self::All => PostgresDb::from_env().await,
        }
    }
}

#[cfg(test)]
mod tests;
