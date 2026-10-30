//! Sandbox skills: vendored Legal Aid Plugin instruction files, loaded per
//! Legal Sandbox session.
//!
//! The files under `../vendor/legal-aid-plugin/skills/` are open-source
//! (Apache-2.0) markdown instructions from `LawDroid`'s Legal Aid Plugin. They
//! are embedded with `include_str!` at compile time — no filesystem paths, no
//! `LawDroid` service, no client data. A missing skill id degrades to "no
//! skill", never an error.

use std::collections::HashSet;

/// Identifier for one vendored sandbox skill, e.g. `"draft"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SkillId(String);

impl SkillId {
    pub fn new(id: impl Into<String>) -> Self {
        SkillId(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SkillId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One vendored skill: frontmatter metadata plus the full markdown body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSkill {
    pub id: SkillId,
    pub name: String,
    pub description: String,
    /// The complete `SKILL.md` contents, frontmatter included. This is the
    /// text a model gateway renders into the session's system context.
    pub markdown: &'static str,
}

const DRAFT: &str = include_str!("../../vendor/legal-aid-plugin/skills/draft/SKILL.md");
const RESEARCH_START: &str =
    include_str!("../../vendor/legal-aid-plugin/skills/research-start/SKILL.md");
const ELIGIBILITY_SCREENING: &str =
    include_str!("../../vendor/legal-aid-plugin/skills/eligibility-screening/SKILL.md");

/// Every vendored skill, in catalog order.
pub fn catalog() -> &'static [SandboxSkill] {
    static CATALOG: std::sync::OnceLock<[SandboxSkill; 3]> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        [
            skill("draft", DRAFT),
            skill("research-start", RESEARCH_START),
            skill("eligibility-screening", ELIGIBILITY_SCREENING),
        ]
    })
}

fn skill(id: &str, markdown: &'static str) -> SandboxSkill {
    let (name, description) = parse_frontmatter(markdown);
    SandboxSkill {
        id: SkillId::new(id),
        name,
        description,
        markdown,
    }
}

/// Extract `name` and `description` from the YAML frontmatter of a vendored
/// SKILL.md. Unknown metadata falls back to the id-shaped defaults so a
/// malformed upstream file degrades rather than panics.
fn parse_frontmatter(markdown: &str) -> (String, String) {
    let mut name = String::new();
    let mut description = String::new();
    if let Some(rest) = markdown.strip_prefix("---\n") {
        for line in rest.split('\n').take_while(|l| *l != "---") {
            if let Some(v) = line.strip_prefix("name:") {
                name = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("description:") {
                // Multi-line YAML block scalars: keep the first line; the
                // full text is in `markdown` anyway.
                description = v.trim().to_string();
            }
        }
    }
    (name, description)
}

/// Look up one skill by id. `None` for unknown ids — the caller renders no
/// skill rather than erroring.
#[must_use]
pub fn get(id: &SkillId) -> Option<&'static SandboxSkill> {
    catalog().iter().find(|s| s.id == *id)
}

/// Render the selected skills into system-context text. Unknown ids in the
/// request are skipped silently — a session with only unknown ids gets an
/// empty context, never an error.
#[must_use]
pub fn render_system_context(skill_ids: &[SkillId]) -> String {
    let requested: HashSet<&SkillId> = skill_ids.iter().collect();
    let sections: Vec<String> = catalog()
        .iter()
        .filter(|skill| requested.contains(&skill.id))
        .map(|skill| {
            format!(
                "# Legal Sandbox skill: {}\n\n{}",
                skill.name, skill.markdown
            )
        })
        .collect();
    sections.join("\n\n---\n\n")
}
