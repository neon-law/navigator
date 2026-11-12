//! Forgejo repository governance. The client and policy live in
//! [`model::forgejo_governance`] so the lean `navigator` CLI can drive the
//! same review loop; the trait that joins it to [`crate::forge`] lives here.

pub use model::forgejo_governance::*;

/// A forge that both holds Project repositories and governs them.
pub trait GovernedForge: crate::forge::ForgeService + RepositoryGovernance {}
impl<T: crate::forge::ForgeService + RepositoryGovernance + ?Sized> GovernedForge for T {}
