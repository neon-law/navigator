//! The two names a Project repository may give its authored Notation sources.
//!
//! Selection depends on directory presence, not on whether a requested code
//! happens to exist under one name. Every caller must reject an ambiguous tree
//! before looking up a Template.

/// The selected root for Project-authored Template files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlueprintDirectory {
    Templates,
    Drafts,
}

impl BlueprintDirectory {
    /// The root directory name in a Project repository.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Templates => "templates",
            Self::Drafts => "drafts",
        }
    }
}

/// The result of checking the two Project blueprint roots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlueprintSelection {
    Absent,
    Selected(BlueprintDirectory),
    Ambiguous,
}

/// Choose from root-directory presence in a checkout or a pinned Git tree.
#[must_use]
pub const fn select(templates_present: bool, drafts_present: bool) -> BlueprintSelection {
    match (templates_present, drafts_present) {
        (false, false) => BlueprintSelection::Absent,
        (true, false) => BlueprintSelection::Selected(BlueprintDirectory::Templates),
        (false, true) => BlueprintSelection::Selected(BlueprintDirectory::Drafts),
        (true, true) => BlueprintSelection::Ambiguous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_directory_presence_combination_has_one_result() {
        assert_eq!(select(false, false), BlueprintSelection::Absent);
        assert_eq!(
            select(true, false),
            BlueprintSelection::Selected(BlueprintDirectory::Templates)
        );
        assert_eq!(
            select(false, true),
            BlueprintSelection::Selected(BlueprintDirectory::Drafts)
        );
        assert_eq!(select(true, true), BlueprintSelection::Ambiguous);
    }
}
