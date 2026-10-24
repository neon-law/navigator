//! Regression coverage for safeguards and current commands in the canonical Project contract.

const AGENT_CONTRACT: &str = include_str!("../src/projects/agent_contract.md");

#[test]
fn canonical_contract_preserves_project_boundaries_and_current_tool_guidance() {
    for required in [
        "navigator project sync",
        "navigator project gate",
        "`client:` lens",
        "navigator glossary list",
        "navigator glossary show \"<Term>\"",
        "Use available MCP servers and the `navigator` CLI",
        "Document bytes, archived authorities, and answers go through Navigator and never into Git.",
        "Use placeholders for client data and matter facts",
        "Gmail** — client instructions and correspondence. Read messages and create drafts only; never send, reply to,",
        "Google Drive** — meeting notes, transcripts, and large document intake.",
        "Keep client identities and matter facts out of",
        "notes/HANDOFF-<topic>.md.local",
        "never append a contradiction",
        "## Final documents only",
        "Never deliver a fill-in-the-blank document",
        "Matter Pages",
        "do not\nwrite application code",
    ] {
        assert!(
            AGENT_CONTRACT.contains(required),
            "canonical contract is missing required guidance: {required}"
        );
    }

    for retired_detail in [
        "verify` job",
        "documents` job",
        "project gate --ci",
        "--check --ci",
    ] {
        assert!(
            !AGENT_CONTRACT.contains(retired_detail),
            "canonical contract contains volatile CI implementation detail: {retired_detail}"
        );
    }
}

#[test]
fn canonical_contract_makes_all_email_draft_only() {
    for required in [
        "## Critical: email is draft-only",
        "Every email must remain a draft.",
        "An agent never sends, replies to, or forwards email.",
        "The lawyer reviews each draft",
        "and sends it personally.",
    ] {
        assert!(
            AGENT_CONTRACT.contains(required),
            "canonical contract is missing critical email guidance: {required}"
        );
    }
}

/// The per-Project branch contract, stated in the canonical `AGENTS.md` every
/// Project repository syncs and in the repository contract doc. The forge
/// enforces it; a hook is a convenience, so neither surface names one as the
/// enforcement.
#[test]
fn both_surfaces_state_the_branch_contract() {
    let doc = include_str!("../../docs/project-repositories.md");
    for (surface, contents) in [("AGENTS.md", AGENT_CONTRACT), ("docs", doc)] {
        for required in ["`main` is the only long-lived branch", "convenience only"] {
            assert!(
                contents.contains(required),
                "{surface} is missing the branch contract's {required:?}"
            );
        }
        assert!(
            !contents.contains("hooks install"),
            "{surface} names a hook installer that does not exist"
        );
    }
    for required in [
        "squash-merged",
        "deleted on merge",
        "Open pull requests ready for review by default",
        "`gh pr merge <number> --auto --squash`",
        "`autoMergeRequest` is non-null",
        "On another forge",
        "retain `visibility: internal`",
        "visibility: client",
        "`client:` lens",
        "Deadline",
    ] {
        assert!(
            AGENT_CONTRACT.contains(required),
            "AGENTS.md missing {required:?}"
        );
    }
    for required in [
        "squash-merged",
        "Open pull requests ready for review by default",
        "`gh pr merge <number> --auto --squash`",
        "`autoMergeRequest` is non-null",
        "On another forge",
        "The required check is `ci`, which runs `navigator project gate`",
        "retain `visibility: internal`",
        "visibility: client",
        "Claude Code Gmail send, reply and forward tools while preserving draft creation",
        "Deadline",
        "`navigator ops github setup` is deprecated",
        "Forgejo",
    ] {
        assert!(
            doc.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains(required),
            "docs missing {required:?}"
        );
    }
}
