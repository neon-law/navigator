//! Public firm team index and individual profile copy.

use serde::{Deserialize, Serialize};

/// Copy for the short `/team` directory and its individual profile pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TeamCopy {
    pub head_title: String,
    pub meta_description: String,
    /// The `og:description` a shared link previews. Absent falls back to
    /// `meta_description`.
    #[serde(default)]
    pub og_description: Option<String>,
    pub page_title: String,
    #[serde(default)]
    pub members: Vec<TeamMemberCopy>,
    pub profile: TeamProfileCopy,
}

/// A directory entry that links to its own profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TeamMemberCopy {
    pub name: String,
    pub href: String,
}

/// One person's short public biography and professional link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TeamProfileCopy {
    pub name: String,
    pub biography: String,
    pub linkedin_url: String,
}
