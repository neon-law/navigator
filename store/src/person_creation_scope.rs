//! Contact creation and Firm-role grants use the existing role and Firm boundaries.
use crate::persons::Role;

pub(crate) fn permits(actor: Role, requested: Role, firm: bool) -> bool {
    match actor {
        Role::Owner => true,
        Role::Admin => {
            requested == Role::Client || (firm && matches!(requested, Role::Lawyer | Role::Clerk))
        }
        Role::Lawyer => requested == Role::Client,
        Role::Clerk | Role::Client => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_creation_needs_no_project_but_firm_role_grants_keep_their_authority() {
        for actor in [Role::Owner, Role::Admin, Role::Lawyer] {
            assert!(permits(actor, Role::Client, false));
        }
        for role in [Role::Owner, Role::Admin, Role::Lawyer, Role::Clerk] {
            assert!(!permits(Role::Lawyer, role, true));
        }
        for role in [Role::Clerk, Role::Lawyer] {
            assert!(permits(Role::Admin, role, true));
            assert!(!permits(Role::Admin, role, false));
        }
        assert!(!permits(Role::Admin, Role::Admin, true));
        assert!(!permits(Role::Admin, Role::Owner, true));
        for actor in [Role::Clerk, Role::Client] {
            assert!(!permits(actor, Role::Client, true));
        }
        assert!(permits(Role::Owner, Role::Owner, false));
    }
}
