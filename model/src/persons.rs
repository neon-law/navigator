//! The authorization tier a Person holds.

use serde::{Deserialize, Serialize};

/// System-wide authorization tier for a Person. Stored as a `string`
/// with an `ASSERT $value IN [...]` on the field, which is what closes
/// the vocabulary. Anonymous callers have no row at all.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The accountable principal who owns the Navigator application. Owner
    /// inherits every Admin and Lawyer capability and alone may assign the
    /// Owner role.
    Owner,
    /// A licensed lawyer with system-administration authority. Bypasses
    /// project-scoping entirely and is a member of the lawyer tier.
    Admin,
    /// A licensed lawyer authorized to perform legal work through Navigator.
    /// The lawyer tier may perform work only on assigned projects and supervise
    /// Clerk work where a future Clerk-specific capability permits it. This is
    /// not an employment, email-domain, or source-forge membership grant.
    Lawyer,
    /// A supervised non-lawyer firm worker. This role is intentionally
    /// outside the lawyer tier and receives no `/app/lawyer`, MCP, Git, or
    /// legal-work authority merely by existing. Narrow Clerk project work
    /// must name its own route and supervision boundary.
    Clerk,
    /// A person the firm represents on at least one matter. Sees
    /// only projects with a matching `person_project_roles` row.
    ///
    /// The default, for both seeded rows and freshly-created ones:
    /// promotion above Client is always opt-in.
    #[default]
    Client,
}

impl Role {
    /// String form used in embedded Rego policy inputs, the URL-encoded
    /// admin form, and the stored `role` field.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Lawyer => "lawyer",
            Self::Clerk => "clerk",
            Self::Client => "client",
        }
    }

    /// The role named by its stored spelling, or `None` for anything
    /// else. The inverse of [`Role::as_str`], and the only way a stored
    /// `role` becomes a [`Role`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "lawyer" => Some(Self::Lawyer),
            "clerk" => Some(Self::Clerk),
            "client" => Some(Self::Client),
            _ => None,
        }
    }

    /// The role's place in the application authority ladder.
    ///
    /// Higher roles may govern lower roles. Participation remains a separate
    /// matter-scope fact and is not represented here.
    #[must_use]
    pub const fn authority_rank(self) -> u8 {
        match self {
            Self::Owner => 4,
            Self::Admin => 3,
            Self::Lawyer => 2,
            Self::Clerk => 1,
            Self::Client => 0,
        }
    }

    /// `true` for Owner and Admin — the system-wide tiers that gate
    /// `/admin/*` and bypass project scoping.
    #[must_use]
    pub fn is_admin_tier(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }

    /// `true` only for the accountable Owner tier.
    #[must_use]
    pub fn is_owner(self) -> bool {
        matches!(self, Self::Owner)
    }

    /// `true` for `Lawyer`, `Admin`, and `Owner` — the lawyer tiers that gate
    /// `/app/lawyer/*` legal work. Clerk is deliberately excluded: its
    /// non-lawyer work must be granted by a separate, supervised capability.
    #[must_use]
    pub fn is_lawyer_tier(self) -> bool {
        matches!(self, Self::Owner | Self::Admin | Self::Lawyer)
    }

    /// `true` when this role is the explicit non-lawyer Clerk tier.
    #[must_use]
    pub fn is_clerk(self) -> bool {
        matches!(self, Self::Clerk)
    }
}
