//! `store::project_modules` — the per-matter capability ledger (#684).
//!
//! Every Project opens as a blank slate and gains practice-area
//! capability through modules lawyers enable per matter. One engagement can
//! run litigation *and* a cap table at once, which is why this is a
//! ledger and not a type column.
//!
//! **Presence of a row is the enabled state.** Disabling deletes it.
//! There is no `enabled` flag and no `disabled_at`, so "is this module
//! on" has exactly one answer and a disabled module has no row for a
//! query to find — which is what makes the client lens toggle-blind by
//! construction rather than by remembering to filter.

use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::matter;
use crate::projects;
use serde_json::json;

/// A per-matter capability.
///
/// **Closed by design.** A new capability is a deliberate addition here,
/// with a migration to widen the database `CHECK`, rather than a
/// free-text value a call site can invent. `contract_review` and the
/// other candidates in #683 join this enum when they are built, not
/// before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Module {
    /// Cases and their per-stage records — many cases per matter.
    Litigation,
    /// The entity cap table surfaced in matter context. Owns the
    /// Project's `cap_table_holding` rows (`store::cap_table_holdings`).
    CapTable,
    /// The estate plan as an explicit capability, replacing the
    /// workflow-sniffing inference it supersedes (#685). Owns the Project's
    /// `fiduciary_role` and `estate_asset` rows (`store::fiduciary_roles`,
    /// `store::estate_assets`).
    Estate,
    /// Statutory and firm deadlines for the matter. Distinct from the
    /// litigation case docket, which belongs to [`Module::Litigation`].
    Deadlines,
}

impl Module {
    /// Every module, in declaration order.
    pub const ALL: &'static [Module] = &[
        Module::Litigation,
        Module::CapTable,
        Module::Estate,
        Module::Deadlines,
    ];

    /// The stored string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Module::Litigation => "litigation",
            Module::CapTable => "cap_table",
            Module::Estate => "estate",
            Module::Deadlines => "deadlines",
        }
    }

    /// Parse a stored value, or `None` when it is outside the closed set.
    #[must_use]
    pub fn parse(value: &str) -> Option<Module> {
        Self::ALL.iter().copied().find(|m| m.as_str() == value)
    }
}

/// Errors from the ledger commands.
#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
    /// A stored value is outside [`Module`], which means a row was written
    /// around the `CHECK`.
    #[error("`{0}` is not a recognized module")]
    UnknownModule(String),
    #[error("audit trail: {0}")]
    Audit(#[from] crate::relationship_logs::RelationshipLogError),
    #[error(transparent)]
    Project(#[from] projects::ProjectStoreError),
    #[error("writing a project module returned no usable row")]
    WriteReturnedNothing,
}

/// One enabled capability in the Project module ledger.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ProjectModule {
    pub id: Uuid,
    pub project_id: Uuid,
    pub module: String,
    pub enabled_at: String,
    pub enabled_by_person_id: Option<Uuid>,
    pub inserted_at: String,
    pub updated_at: String,
}

/// Enable `module` on `project_id`, recording who did it.
///
/// Idempotent: enabling an already-enabled module returns the existing row
/// and writes no second audit entry, so a double-submitted lawyer form does
/// not manufacture a toggle that did not happen.
///
/// With PostgreSQL configured, the ledger state and its Relationship Log audit
/// commit in one transaction.
///
/// # Errors
/// Propagates any database error.
pub async fn enable(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    module: Module,
    actor_person_id: Option<Uuid>,
) -> Result<ProjectModule, ModuleError> {
    let pg = matter::authority_for("project_module").await?;
    matter::toggle_module(&pg, project_id, module, actor_person_id, true)
        .await?
        .ok_or(ModuleError::WriteReturnedNothing)
}

/// Disable `module` on `project_id`, recording who did it.
///
/// Deletes the ledger row. **Module-owned data is untouched** — disabling
/// litigation hides the capability, it does not destroy the cases — so
/// re-enabling restores the matter to where it was.
///
/// Returns `true` when a row was removed and `false` when the module was
/// already off. An already-off module writes no audit entry, for the same
/// reason enabling twice does not.
///
/// # Errors
/// Propagates any database error.
pub async fn disable(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    module: Module,
    actor_person_id: Option<Uuid>,
) -> Result<bool, ModuleError> {
    let pg = matter::authority_for("project_module").await?;
    Ok(
        matter::toggle_module(&pg, project_id, module, actor_person_id, false)
            .await?
            .is_some(),
    )
}

/// Every module enabled on `project_id`.
///
/// Returns only what is **on**. There is no "all modules with their
/// state" read here on purpose: a caller that received the full set and
/// filtered would be one refactor away from shipping the disabled list to
/// a client, which is precisely what the toggle-blindness rule forbids.
/// A lawyer management surface composes the off-modules by differencing
/// this against [`Module::ALL`] on the server.
///
/// # Errors
/// [`ModuleError::UnknownModule`] if a stored value is outside the closed
/// set, or a database error.
pub async fn list_for_project(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<Module>, ModuleError> {
    let pg = matter::authority_for("project_module").await?;
    let rows: Vec<ProjectModule> = matter::select(
        &pg,
        "project_module",
        json!({"project_id": project_id}),
        &["module"],
    )
    .await?;
    rows.into_iter()
        .map(|row| Module::parse(&row.module).ok_or(ModuleError::UnknownModule(row.module)))
        .collect()
}

/// True when `module` is enabled on `project_id`.
///
/// # Errors
/// Propagates any database error.
pub async fn is_enabled(
    application_store: &ApplicationStore,
    project_id: Uuid,
    module: Module,
) -> Result<bool, ModuleError> {
    Ok(find(application_store, project_id, module).await?.is_some())
}

async fn find(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    module: Module,
) -> Result<Option<ProjectModule>, ModuleError> {
    let pg = matter::authority_for("project_module").await?;
    Ok(matter::find(
        &pg,
        "project_module",
        json!({"project_id": project_id, "module": module.as_str()}),
    )
    .await?)
}

#[cfg(test)]
mod tests {
    use super::Module;

    #[test]
    fn every_module_round_trips() {
        for module in Module::ALL {
            assert_eq!(Module::parse(module.as_str()), Some(*module));
        }
    }

    #[test]
    fn the_set_is_closed() {
        // Candidates named in #683 are deliberately absent until built.
        assert_eq!(Module::parse("contract_review"), None);
        assert_eq!(
            Module::parse("docket"),
            None,
            "docket belongs to litigation"
        );
        assert_eq!(Module::ALL.len(), 4);
    }
}
