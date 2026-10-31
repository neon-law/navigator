//! Tests for the sandbox skill catalog: embedding, frontmatter metadata,
//! graceful degradation on unknown ids, and system-context rendering.

use sandbox_skills::{catalog, get, render_system_context, SkillId};

#[test]
fn catalog_contains_first_wave_skills() {
    let ids: Vec<&str> = catalog().iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["draft", "research-start", "eligibility-screening"]
    );
}

#[test]
fn every_vendored_file_has_frontmatter_metadata() {
    for skill in catalog() {
        assert!(!skill.name.is_empty(), "{} has no name", skill.id);
        assert!(
            !skill.description.is_empty(),
            "{} has no description",
            skill.id
        );
    }
}

#[test]
fn embedded_markdown_is_the_vendored_body() {
    let draft = get(&SkillId::new("draft")).expect("draft skill is vendored");
    assert!(draft.markdown.contains("# /draft"));
    assert!(draft.markdown.contains("[FACT NEEDED"));
}

#[test]
fn unknown_skill_id_degrades_to_none() {
    assert!(get(&SkillId::new("nonexistent")).is_none());
}

#[test]
fn rendering_with_only_unknown_ids_is_empty_not_an_error() {
    let context = render_system_context(&[SkillId::new("nonexistent")]);
    assert!(context.is_empty());
}

#[test]
fn rendering_renders_selected_skills_into_system_context() {
    let context = render_system_context(&[SkillId::new("draft"), SkillId::new("research-start")]);
    assert!(context.contains("# Legal Sandbox skill: draft"));
    assert!(context.contains("# /draft"));
    assert!(context.contains("# Legal Sandbox skill: research-start"));
    // Both sections present, each with its header.
    assert_eq!(context.matches("# Legal Sandbox skill:").count(), 2);
}

#[test]
fn rendering_skips_unselected_skills() {
    let context = render_system_context(&[SkillId::new("draft")]);
    assert!(!context.contains("# Legal Sandbox skill: eligibility-screening"));
}

#[test]
fn repeated_ids_render_once() {
    let context = render_system_context(&[SkillId::new("draft"), SkillId::new("draft")]);
    assert_eq!(context.matches("# Legal Sandbox skill: draft").count(), 1);
}

#[test]
fn empty_selection_renders_nothing() {
    assert!(render_system_context(&[]).is_empty());
}
