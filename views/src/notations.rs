//! The `templates/README.md` body that `/notations` publishes.
//!
//! The page stays tied to the repository instructions: the README is baked in
//! at compile time, and its relative Markdown links are rewritten to the paths
//! the running site actually serves. Rendering lives here beside the rest of the
//! Markdown layer; the page itself is a Dioxus component in `webapp`.

use crate::markdown::render_with_link_filter;

const README: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../templates/README.md"
));

/// The rendered README body, with its links rewritten to site routes.
///
/// The page's hero owns the title, so the README's leading `# Notations`
/// heading is stripped to avoid a duplicate `<h1>`.
#[must_use]
pub fn readme_html() -> String {
    render_with_link_filter(strip_leading_h1(README), rewrite_link)
}

/// Drop the leading top-level `# ...` heading line (and the blank lines after
/// it) so the hero band, not the body, carries the page title.
fn strip_leading_h1(md: &str) -> &str {
    match md.split_once('\n') {
        Some((first, rest)) if first.starts_with("# ") => rest.trim_start_matches('\n'),
        _ => md,
    }
}

/// Map one of the README's relative Markdown links onto the URL that serves it:
/// a glossary term to its `/glossary#<slug>` anchor and a template to its raw
/// `/app/api/templates/...` route. Private source files keep their labels as
/// plain text on the public site. Absolute and in-page links pass through.
fn rewrite_link(dest: &str) -> Option<String> {
    if dest.starts_with("http://")
        || dest.starts_with("https://")
        || dest.starts_with("mailto:")
        || dest.starts_with('#')
    {
        return Some(dest.to_string());
    }
    let (path, anchor) = match dest.split_once('#') {
        Some((p, a)) => (p, Some(a)),
        None => (dest, None),
    };
    if let Some(stem) = path
        .strip_prefix("../docs/glossary/")
        .and_then(|rest| rest.strip_suffix(".md"))
        .filter(|stem| !stem.contains('/'))
    {
        return Some(if stem == "README" {
            "/glossary".to_string()
        } else {
            format!("/glossary#{stem}")
        });
    }
    if path.starts_with("../") {
        return None;
    }
    if let Some(stem) = path.strip_suffix(".md") {
        return Some(with_anchor(
            &format!("/app/api/templates/{}", crate::slug::to_url(stem)),
            anchor,
        ));
    }
    None
}

fn with_anchor(base: &str, anchor: Option<&str>) -> String {
    match anchor {
        Some(a) => format!("{base}#{a}"),
        None => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{readme_html, rewrite_link, strip_leading_h1, README};

    #[test]
    fn strip_leading_h1_drops_only_the_first_heading_line() {
        assert_eq!(
            strip_leading_h1("# Notations\n\nBody first line.\n"),
            "Body first line.\n"
        );
        assert_eq!(strip_leading_h1("No heading here.\n"), "No heading here.\n");
    }

    #[test]
    fn the_page_is_tied_to_the_readme() {
        assert!(README.starts_with("# Notations"));
        assert!(README.contains("## Naming convention"));
    }

    /// The hero owns the title, so the rendered body must not repeat the
    /// README's own `# Notations` heading.
    #[test]
    fn the_rendered_body_drops_the_readme_title() {
        let html = readme_html();
        assert!(
            !html.contains(">Notations</h1>"),
            "the README H1 must be stripped: {html}"
        );
        assert!(
            html.contains("Every notation has YAML frontmatter"),
            "the README body still renders: {html}"
        );
        assert!(
            !html.contains("github.com/neon-law-source-code/navigator"),
            "{html}"
        );
        assert!(html.contains("docs/frontmatter.md"), "{html}");
    }

    #[test]
    fn glossary_links_map_to_the_glossary_page() {
        assert_eq!(
            rewrite_link("../docs/glossary/draft.md"),
            Some("/glossary#draft".to_string())
        );
        assert_eq!(
            rewrite_link("../docs/glossary/README.md"),
            Some("/glossary".to_string())
        );
    }

    #[test]
    fn private_contributor_docs_keep_text_without_links() {
        assert_eq!(rewrite_link("../docs/notation.md#template"), None);
    }

    #[test]
    fn private_root_files_keep_text_without_links() {
        assert_eq!(rewrite_link("../README.md#trademarks"), None);
    }

    #[test]
    fn template_links_map_to_the_raw_api() {
        assert_eq!(
            rewrite_link("notations/forms/united_states/nevada/state/nv__llc_formation.md"),
            Some(
                "/app/api/templates/notations/forms/united-states/nevada/state/nv--llc-formation"
                    .to_string()
            )
        );
        assert_eq!(
            rewrite_link("notations/forms/united_states/nevada/state/nv__annual_report.md"),
            Some(
                "/app/api/templates/notations/forms/united-states/nevada/state/nv--annual-report"
                    .to_string()
            )
        );
    }

    #[test]
    fn unpublished_template_sidecars_keep_text_without_links() {
        assert_eq!(
            rewrite_link(
                "notations/forms/united_states/nevada/state/nv__llc_formation.fields.toml"
            ),
            None
        );
    }
}
