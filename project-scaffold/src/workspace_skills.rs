//! The Agent Skills a lawyer's workspace root carries.
//!
//! The workspace root is the directory that holds a lawyer's Project
//! repositories. Work started there spans matters: triaging an inbox, finding
//! which court takes a paper, filing and serving it, researching authority.
//! Every skill here is generic: each instruction holds for any Firm using
//! Navigator. A Firm's own vendors, accounts, people, and voice live in that
//! Firm's companion skills, never in this list.

/// `(name, SKILL.md)` for every workspace-level skill, each embedded from
/// Navigator's canonical `.agents/skills/`.
pub const WORKSPACE_SKILLS: &[(&str, &str)] = &[
    (
        "benton-county-wa-efiling",
        include_str!("../../.agents/skills/benton-county-wa-efiling/SKILL.md"),
    ),
    (
        "browser-handoff",
        include_str!("../../.agents/skills/browser-handoff/SKILL.md"),
    ),
    (
        "calendar-conflict-check",
        include_str!("../../.agents/skills/calendar-conflict-check/SKILL.md"),
    ),
    (
        "california-oah-special-ed-filing",
        include_str!("../../.agents/skills/california-oah-special-ed-filing/SKILL.md"),
    ),
    (
        "certified-mail",
        include_str!("../../.agents/skills/certified-mail/SKILL.md"),
    ),
    (
        "clark-county-nv-efiling",
        include_str!("../../.agents/skills/clark-county-nv-efiling/SKILL.md"),
    ),
    (
        "clark-county-wa-efiling",
        include_str!("../../.agents/skills/clark-county-wa-efiling/SKILL.md"),
    ),
    (
        "delaware-bankruptcy-ecf",
        include_str!("../../.agents/skills/delaware-bankruptcy-ecf/SKILL.md"),
    ),
    (
        "douglas-county-nv-filing",
        include_str!("../../.agents/skills/douglas-county-nv-filing/SKILL.md"),
    ),
    (
        "efile-and-serve-sync",
        include_str!("../../.agents/skills/efile-and-serve-sync/SKILL.md"),
    ),
    (
        "email-draft",
        include_str!("../../.agents/skills/email-draft/SKILL.md"),
    ),
    (
        "harris-county-tx-efiling",
        include_str!("../../.agents/skills/harris-county-tx-efiling/SKILL.md"),
    ),
    (
        "human-readable",
        include_str!("../../.agents/skills/human-readable/SKILL.md"),
    ),
    (
        "legal-authority",
        include_str!("../../.agents/skills/legal-authority/SKILL.md"),
    ),
    (
        "legal-plan-acceptance",
        include_str!("../../.agents/skills/legal-plan-acceptance/SKILL.md"),
    ),
    (
        "legal-writing",
        include_str!("../../.agents/skills/legal-writing/SKILL.md"),
    ),
    (
        "los-angeles-superior-efiling",
        include_str!("../../.agents/skills/los-angeles-superior-efiling/SKILL.md"),
    ),
    (
        "message-processing",
        include_str!("../../.agents/skills/message-processing/SKILL.md"),
    ),
    (
        "navigator-version-bump",
        include_str!("../../.agents/skills/navigator-version-bump/SKILL.md"),
    ),
    (
        "outlook-triage",
        include_str!("../../.agents/skills/outlook-triage/SKILL.md"),
    ),
    (
        "records-subpoena",
        include_str!("../../.agents/skills/records-subpoena/SKILL.md"),
    ),
    (
        "signer-identity",
        include_str!("../../.agents/skills/signer-identity/SKILL.md"),
    ),
    (
        "texas-entity-lookup",
        include_str!("../../.agents/skills/texas-entity-lookup/SKILL.md"),
    ),
    ("today", include_str!("../../.agents/skills/today/SKILL.md")),
    (
        "triage-email",
        include_str!("../../.agents/skills/triage-email/SKILL.md"),
    ),
    (
        "washington-court-records",
        include_str!("../../.agents/skills/washington-court-records/SKILL.md"),
    ),
    (
        "washington-superior-court-efiling",
        include_str!("../../.agents/skills/washington-superior-court-efiling/SKILL.md"),
    ),
    (
        "washoe-county-nv-efiling",
        include_str!("../../.agents/skills/washoe-county-nv-efiling/SKILL.md"),
    ),
    (
        "analyze-memo",
        include_str!("../../.agents/skills/analyze-memo/SKILL.md"),
    ),
    (
        "case-timeline",
        include_str!("../../.agents/skills/case-timeline/SKILL.md"),
    ),
    (
        "client-council",
        include_str!("../../.agents/skills/client-council/SKILL.md"),
    ),
    (
        "council",
        include_str!("../../.agents/skills/council/SKILL.md"),
    ),
    (
        "court-filing-drafts",
        include_str!("../../.agents/skills/court-filing-drafts/SKILL.md"),
    ),
    (
        "legal-council",
        include_str!("../../.agents/skills/legal-council/SKILL.md"),
    ),
    ("ocr", include_str!("../../.agents/skills/ocr/SKILL.md")),
    (
        "opposing-communication-review",
        include_str!("../../.agents/skills/opposing-communication-review/SKILL.md"),
    ),
    (
        "record-search",
        include_str!("../../.agents/skills/record-search/SKILL.md"),
    ),
    (
        "word-editing",
        include_str!("../../.agents/skills/word-editing/SKILL.md"),
    ),
];

/// Workspace skills that Project checkouts can still carry as previously synced copies.
pub const MIGRATED_WORKSPACE_SKILLS: &[&str] = &[
    "analyze-memo",
    "browser-handoff",
    "calendar-conflict-check",
    "case-timeline",
    "client-council",
    "council",
    "court-filing-drafts",
    "human-readable",
    "legal-council",
    "legal-writing",
    "message-processing",
    "ocr",
    "opposing-communication-review",
    "record-search",
    "word-editing",
];

#[cfg(test)]
mod tests {
    use super::WORKSPACE_SKILLS;
    use std::collections::HashSet;

    /// The `name:` a skill's frontmatter declares.
    fn frontmatter_name(contents: &str) -> Option<String> {
        let (frontmatter, _) = rules::frontmatter::split(contents)?;
        let parsed: serde_yaml::Value = serde_yaml::from_str(frontmatter).ok()?;
        parsed["name"].as_str().map(str::to_owned)
    }

    #[test]
    fn every_workspace_skill_declares_its_own_name_once() {
        let mut seen = crate::SYNCED_SKILLS
            .iter()
            .map(|(name, _)| *name)
            .collect::<HashSet<_>>();
        for (name, contents) in WORKSPACE_SKILLS {
            assert_eq!(frontmatter_name(contents).as_deref(), Some(*name));
            assert!(seen.insert(*name), "{name} is listed twice");
        }
    }
}
