//! The canonical list of Restate services this worker registers, split by
//! shape, plus the naming guarantee that keeps the ingress legible.
//!
//! `host.rs` binds exactly these services onto the one endpoint Restate
//! discovers. Two shapes live there:
//!
//! - **Durable workflows** — one-shot, keyed-by-idempotency invocations
//!   (`Archives`, `GeneralNag`, the billing workflows). Their names are
//!   `PascalCase`, so a deployment's discovered service list reads like a
//!   catalogue of named things. The naming test below shares the
//!   `rules::is_pascal_case` predicate so every registered name is checked
//!   against one canonical definition of `PascalCase`. (Template `.md`
//!   basenames follow the *separate* `snake_case` convention `N103` now
//!   enforces — different artifact, different rule.)
//! - **Virtual objects** — keyed, addressable services (`notation`, invoked as
//!   `/notation/<id>/...`). Restate names objects in lowercase by convention,
//!   so this one is a *deliberate* exception to the `PascalCase` rule, pinned
//!   here so a future lowercase *workflow* still fails the test.
//!
//! This registry is the single source of truth for those names; the
//! `registered_services` test guards it against drift from `host.rs`'s actual
//! `.bind(...)` calls so adding a service without recording its name — or
//! recording one with the wrong case — fails the build.

/// Durable workflow services bound by the worker. Each name MUST be `PascalCase`
/// (enforced by `workflow_service_names_are_pascal_case`). Keep in lockstep
/// with the `.bind(...)` calls in `host.rs`.
pub const WORKFLOW_SERVICES: &[&str] = &[
    "EmailSummary",
    "Archives",
    "DriDigest",
    "FinanceAggregate",
    "GeneralNag",
    "InboundMessageAlert",
    "InboundTriage",
    "NightlyProjectUpdate",
    "NightlyProjectUpdateMembership",
    "NightlyProjectUpdateSlack",
    "NightlyProjectUpdateRepository",
    "NotationReset",
    "SocialPublisher",
    "SocialPublishReconciler",
    "YoutubeCommentsPoller",
];

/// Virtual-object services bound by the worker. Lowercase kebab-case by
/// deliberate exception — a virtual object is addressed by key
/// (`/notation/<id>/...`), not invoked as a one-shot workflow, so it follows
/// Restate's object-naming convention rather than the `PascalCase` template
/// convention.
pub const VIRTUAL_OBJECTS: &[&str] = &["notation", "project-slack"];

/// Internal DBOS operations have no Restate binding and share their parent caller switch.
pub const INTERNAL_DBOS_OPERATIONS: &[(&str, &str)] = &[
    (
        crate::dbos_email_summary::recovery::WORKFLOW,
        crate::dbos_email_summary::ENABLED_ENV,
    ),
    (
        crate::nightly_project_update::Tag::Calendar.service(),
        crate::dbos_nightly_project_update::ENABLED_ENV,
    ),
];

#[cfg(test)]
mod tests {
    use super::{VIRTUAL_OBJECTS, WORKFLOW_SERVICES};
    use crate::nightly_project_update::Tag;

    #[test]
    fn deployment_admission_covers_actual_worker_bindings_and_caller_selectors() {
        let mut runtime = vec![
            ("EmailSummary", crate::dbos_email_summary::ENABLED_ENV),
            ("Archives", archives::workflow::DBOS_ENABLED_ENV),
            ("DriDigest", workflows::dbos_engine::dri_digest::ENABLED_ENV),
            (
                "FinanceAggregate",
                crate::dbos_finance_aggregate::ENABLED_ENV,
            ),
            (
                "GeneralNag",
                workflows::dbos_engine::general_nag::ENABLED_ENV,
            ),
            (
                "InboundMessageAlert",
                crate::dbos_inbound_message_alert::ENABLED_ENV,
            ),
            (
                "InboundTriage",
                workflows::trigger::INBOUND_TRIAGE_DBOS_ENABLED_ENV,
            ),
            (
                "NightlyProjectUpdate",
                crate::dbos_nightly_project_update::ENABLED_ENV,
            ),
            (
                "NightlyProjectUpdateMembership",
                crate::dbos_nightly_project_update::ENABLED_ENV,
            ),
            (
                "NightlyProjectUpdateSlack",
                crate::dbos_nightly_project_update::ENABLED_ENV,
            ),
            (
                "NightlyProjectUpdateRepository",
                crate::dbos_nightly_project_update::ENABLED_ENV,
            ),
            (
                "NotationReset",
                workflows::dbos_engine::notation_reset::ENABLED_ENV,
            ),
            (
                "SocialPublisher",
                crate::dbos_social_publisher::PUBLISHER_ENABLED_ENV,
            ),
            (
                "SocialPublishReconciler",
                crate::dbos_social_publisher::RECONCILER_ENABLED_ENV,
            ),
            (
                "YoutubeCommentsPoller",
                workflows::trigger::YOUTUBE_COMMENTS_DBOS_ENABLED_ENV,
            ),
            ("notation", crate::dbos_notation::ENABLED_ENV),
            ("project-slack", crate::dbos_project_slack::ENABLED_ENV),
        ];
        runtime.sort_unstable();
        let mut admission: Vec<_> = store::workflow_engine::WORKFLOWS
            .iter()
            .map(|row| (row.name, row.flag))
            .collect();
        admission.sort_unstable();
        assert_eq!(runtime, admission);
        let mut registered: Vec<_> = WORKFLOW_SERVICES
            .iter()
            .chain(VIRTUAL_OBJECTS)
            .copied()
            .collect();
        registered.sort_unstable();
        assert_eq!(
            registered,
            admission.iter().map(|(name, _)| *name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn internal_operations_use_registered_parent_selectors() {
        for (child, parent) in [
            (
                crate::dbos_email_summary::recovery::WORKFLOW,
                "EmailSummary",
            ),
            (Tag::Calendar.service(), "NightlyProjectUpdate"),
        ] {
            assert!(!WORKFLOW_SERVICES.contains(&child));
            let selection = store::workflow_engine::WORKFLOWS
                .iter()
                .find(|selection| selection.name == parent)
                .expect("parent has a registered engine selector");
            assert!(super::INTERNAL_DBOS_OPERATIONS.contains(&(child, selection.flag)));
        }
    }

    /// Every registered durable workflow is named `PascalCase`, checked with
    /// the canonical `rules::is_pascal_case` predicate so all registered names
    /// share one definition. (Template `.md` basenames follow the separate
    /// `snake_case` convention `N103` enforces — a different artifact.)
    #[test]
    fn workflow_service_names_are_pascal_case() {
        for name in WORKFLOW_SERVICES {
            assert!(
                rules::is_pascal_case(name),
                "registered workflow `{name}` is not PascalCase — Restate workflow names are \
                 PascalCase by convention"
            );
        }
    }

    /// The virtual-object carve-out is exactly that — a carve-out. Pin it to
    /// lowercase so a future *workflow* mistakenly added as a lowercase name
    /// still trips `workflow_service_names_are_pascal_case` instead of being
    /// quietly parked here.
    #[test]
    fn virtual_objects_are_the_documented_lowercase_exception() {
        for name in VIRTUAL_OBJECTS {
            assert!(
                !rules::is_pascal_case(name),
                "`{name}` is PascalCase — it should be a WORKFLOW_SERVICE, not a virtual object"
            );
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "virtual object `{name}` must be lowercase kebab-case"
            );
        }
    }

    /// No name appears twice, and no workflow is mis-filed as an object.
    #[test]
    fn registry_names_are_unique_across_both_lists() {
        let mut all: Vec<&str> = WORKFLOW_SERVICES
            .iter()
            .chain(VIRTUAL_OBJECTS.iter())
            .copied()
            .collect();
        all.sort_unstable();
        let len = all.len();
        all.dedup();
        assert_eq!(len, all.len(), "duplicate service name in the registry");
    }

    /// The Nightly Project Update parent and every tagged child are registered;
    /// the retired single-workflow name is not.
    #[test]
    fn the_nightly_project_update_and_its_tagged_children_are_registered() {
        assert!(WORKFLOW_SERVICES.contains(&"NightlyProjectUpdate"));
        for tag in Tag::ALL {
            let registered = if Tag::COMPATIBILITY.contains(tag) {
                WORKFLOW_SERVICES.contains(&tag.service())
            } else {
                super::INTERNAL_DBOS_OPERATIONS.contains(&(
                    tag.service(),
                    crate::dbos_nightly_project_update::ENABLED_ENV,
                ))
            };
            assert!(
                registered,
                "tag `{}` has no registered child `{}`",
                tag.name(),
                tag.service()
            );
        }
        assert!(!WORKFLOW_SERVICES.contains(&"ProjectSlackMembershipSync"));
    }
}
