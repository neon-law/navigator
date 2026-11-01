//! Provision an existing Firm's Forgejo organization.
//!
//! A Firm's organization is named by its immutable [`Firm::slug`]
//! ([`super::forgejo`]). The installation lets only a site administrator
//! create organizations, so Navigator's provisioning identity
//! ([`cloud::forgejo_organization::ForgejoOrganizations`]) creates or
//! reconciles it, and places the account the Firm's own Forgejo access token
//! authenticates in its owners team. That account can then create and copy
//! the Firm's Project repositories there with the Firm's own token; nobody
//! holds site administration to do it.
//!
//! [`reconcile`] does the forge work; [`record`] writes the result to the
//! PostgreSQL `firm_forgejo_organization` row. Both converge: a re-run against
//! a Firm whose organization already exists creates nothing, adds nobody
//! twice, and updates the one row's `reconciled_at`.

use chrono::{DateTime, Utc};
use cloud::forge::ForgeError;
use cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV;
use cloud::forgejo_organization::{
    token_login, ForgejoOrganizations, OrganizationError, NAVIGATOR_FORGEJO_PROVISIONER_TOKEN_ENV,
};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use super::Firm;
use crate::firm_secrets::{IntegrationProvider, IntegrationSecretKind};
use crate::postgres::PostgresDb;
use crate::surreal::SurrealDb;

/// A Firm's organization as Navigator last reconciled it: one
/// `firm_forgejo_organization` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgejoOrganization {
    pub firm_id: Uuid,
    /// The organization's name: the Firm's slug.
    pub organization: String,
    /// Forgejo's numeric id for the organization.
    pub forgejo_id: i64,
    /// The account the Firm's own access token authenticates, kept in the
    /// organization's owners team.
    pub owner_login: String,
    pub provisioned_at: DateTime<Utc>,
    pub reconciled_at: DateTime<Utc>,
}

/// What one [`reconcile`] did on the forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reconciled {
    pub firm_id: Uuid,
    pub organization: String,
    pub forgejo_id: i64,
    /// Whether this run created the organization.
    pub created: bool,
    /// Whether this run changed the organization's visibility to private.
    pub made_private: bool,
    pub owner_login: String,
    /// Whether this run added [`Self::owner_login`] to the owners team.
    pub owner_added: bool,
}

/// Why a Firm's organization could not be provisioned.
#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    #[error("no such Firm")]
    NotFound,
    /// Every missing setting, named at once.
    #[error("Forgejo organization provisioning needs {0}")]
    Unavailable(String),
    #[error(transparent)]
    Organization(#[from] OrganizationError),
    #[error("database: {0}")]
    Db(String),
    #[error("PostgreSQL: {0}")]
    Postgres(#[from] sqlx::Error),
    /// The Firm row could not be mirrored into PostgreSQL before recording.
    #[error("mirror the Firm into PostgreSQL: {0}")]
    Mirror(#[from] crate::postgres_mirror::MirrorError),
    /// The stored row names another organization; the slug is immutable, so
    /// this needs a human, not a retry.
    #[error("the Firm's recorded Forgejo organization differs from its slug")]
    RecordedElsewhere,
}

impl From<ForgeError> for ProvisionError {
    fn from(error: ForgeError) -> Self {
        Self::Organization(OrganizationError::Forge(error))
    }
}

/// [`reconcile`] the Firm's organization on the forge, then [`record`] it on
/// the Firm. The Firm row is mirrored into PostgreSQL first, so a Firm created
/// before the mirror ran still has the row its record points at.
///
/// # Errors
/// Any [`ProvisionError`] from either step. A forge failure records nothing.
pub async fn provision(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    firm_id: Uuid,
    kms: &dyn cloud::RuntimeKms,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<(Reconciled, ForgejoOrganization), ProvisionError> {
    let reconciled = reconcile(surreal, firm_id, kms, lookup).await?;
    crate::postgres_mirror::mirror(
        surreal,
        pg,
        &crate::postgres_mirror::MirrorKey::Firm(firm_id),
    )
    .await?;
    let recorded = record(pg, &reconciled).await?;
    Ok((reconciled, recorded))
}

/// Create or reconcile `firm_id`'s Forgejo organization with the provisioning
/// identity, and keep the Firm's own Forgejo account in its owners team.
///
/// `lookup` reads [`NAVIGATOR_FORGEJO_ROOT_URL_ENV`] and
/// [`NAVIGATOR_FORGEJO_PROVISIONER_TOKEN_ENV`]. The Firm's account comes from
/// the Firm's own `forgejo_access_token` integration secret; nothing falls
/// back to another Firm's token. Every missing piece is named together before
/// any request is made.
///
/// # Errors
/// [`ProvisionError::NotFound`] for an unknown Firm,
/// [`ProvisionError::Unavailable`] naming each missing setting, or the forge's
/// refusal.
pub async fn reconcile(
    surreal: &SurrealDb,
    firm_id: Uuid,
    kms: &dyn cloud::RuntimeKms,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Reconciled, ProvisionError> {
    let firm: Firm = super::find_by_id(surreal, firm_id)
        .await
        .map_err(|error| ProvisionError::Db(error.to_string()))?
        .ok_or(ProvisionError::NotFound)?;
    let setting = |key: &str| lookup(key).filter(|value| !value.trim().is_empty());
    let root = setting(NAVIGATOR_FORGEJO_ROOT_URL_ENV);
    let provisioner = setting(NAVIGATOR_FORGEJO_PROVISIONER_TOKEN_ENV);
    let firm_token = crate::firm_secrets::resolve_firm_credential(
        surreal,
        firm.id,
        IntegrationProvider::Forgejo,
        IntegrationSecretKind::ForgejoAccessToken,
        kms,
    )
    .await
    .ok();
    let mut missing = Vec::new();
    if root.is_none() {
        missing.push(NAVIGATOR_FORGEJO_ROOT_URL_ENV);
    }
    if provisioner.is_none() {
        missing.push(NAVIGATOR_FORGEJO_PROVISIONER_TOKEN_ENV);
    }
    if firm_token.is_none() {
        missing.push("the Firm's forgejo_access_token integration secret");
    }
    let (Some(root), Some(provisioner), Some(firm_token)) = (root, provisioner, firm_token) else {
        return Err(ProvisionError::Unavailable(missing.join(", ")));
    };
    let organizations = ForgejoOrganizations::new(&root, &provisioner)?;
    let owner_login = token_login(&root, firm_token.expose_for_provider_call()).await?;
    let organization = organizations.ensure(&firm.slug).await?;
    let owner_added = organizations
        .ensure_owner(&organization.name, &owner_login)
        .await?;
    Ok(Reconciled {
        firm_id: firm.id,
        organization: organization.name,
        forgejo_id: organization.id,
        created: organization.created,
        made_private: organization.made_private,
        owner_login,
        owner_added,
    })
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<ForgejoOrganization, sqlx::Error> {
    Ok(ForgejoOrganization {
        firm_id: row.try_get("firm_id")?,
        organization: row.try_get("organization")?,
        forgejo_id: row.try_get("forgejo_id")?,
        owner_login: row.try_get("owner_login")?,
        provisioned_at: row.try_get("provisioned_at")?,
        reconciled_at: row.try_get("reconciled_at")?,
    })
}

/// Record a [`reconcile`] result on the Firm: insert its one row, or update
/// the existing row's forge id, owner, and `reconciled_at`. `provisioned_at`
/// keeps the first run's time.
///
/// # Errors
/// [`ProvisionError::RecordedElsewhere`] when the Firm's row names another
/// organization; [`ProvisionError::Postgres`] for a database failure.
pub async fn record(
    pg: &PostgresDb,
    reconciled: &Reconciled,
) -> Result<ForgejoOrganization, ProvisionError> {
    let row = sqlx::query(
        "INSERT INTO firm_forgejo_organization (firm_id,organization,forgejo_id,owner_login) \
         VALUES ($1,$2,$3,$4) \
         ON CONFLICT (firm_id) DO UPDATE SET forgejo_id=EXCLUDED.forgejo_id, \
         owner_login=EXCLUDED.owner_login, reconciled_at=now() \
         WHERE firm_forgejo_organization.organization=EXCLUDED.organization \
         RETURNING firm_id,organization,forgejo_id,owner_login,provisioned_at,reconciled_at",
    )
    .bind(reconciled.firm_id)
    .bind(&reconciled.organization)
    .bind(reconciled.forgejo_id)
    .bind(&reconciled.owner_login)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(ProvisionError::RecordedElsewhere)?;
    Ok(from_row(&row)?)
}

/// The Firm's recorded organization, if it has been provisioned.
///
/// # Errors
/// [`ProvisionError::Postgres`] for a database failure.
pub async fn find(
    pg: &PostgresDb,
    firm_id: Uuid,
) -> Result<Option<ForgejoOrganization>, ProvisionError> {
    let row = sqlx::query(
        "SELECT firm_id,organization,forgejo_id,owner_login,provisioned_at,reconciled_at \
         FROM firm_forgejo_organization WHERE firm_id=$1",
    )
    .bind(firm_id)
    .fetch_optional(pg.pool())
    .await?;
    Ok(row.as_ref().map(from_row).transpose()?)
}

#[cfg(test)]
mod tests {
    use super::{find, record, Reconciled};
    use crate::postgres_mirror::tests::{connect, fresh_schema};
    use uuid::Uuid;

    fn reconciled(firm_id: Uuid, organization: &str, forgejo_id: i64) -> Reconciled {
        Reconciled {
            firm_id,
            organization: organization.into(),
            forgejo_id,
            created: true,
            made_private: false,
            owner_login: "firm-account".into(),
            owner_added: true,
        }
    }

    // Needs a live PostgreSQL; self-skips without one.
    #[tokio::test]
    async fn a_rerun_updates_the_one_row_and_keeps_the_first_provisioning_time() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let firm_id = Uuid::new_v4();
        sqlx::query("INSERT INTO firm (id,name,slug,status) VALUES ($1,'Example Law PC','example-law-pc','active')")
            .bind(firm_id)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(find(&pg, firm_id).await.unwrap().is_none());

        let first = record(&pg, &reconciled(firm_id, "example-law-pc", 7))
            .await
            .unwrap();
        assert_eq!(first.organization, "example-law-pc");
        assert_eq!(first.forgejo_id, 7);
        let second = record(&pg, &reconciled(firm_id, "example-law-pc", 9))
            .await
            .unwrap();
        assert_eq!(
            second.forgejo_id, 9,
            "a recreated organization's id is kept"
        );
        assert_eq!(second.provisioned_at, first.provisioned_at);
        assert!(second.reconciled_at >= first.reconciled_at);
        assert_eq!(find(&pg, firm_id).await.unwrap(), Some(second));
        let rows: i64 =
            sqlx::query_scalar("SELECT count(*) FROM firm_forgejo_organization WHERE firm_id=$1")
                .bind(firm_id)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(rows, 1);

        assert!(matches!(
            record(&pg, &reconciled(firm_id, "another-org", 9)).await,
            Err(super::ProvisionError::RecordedElsewhere)
        ));
    }
}
