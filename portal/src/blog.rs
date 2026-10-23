//! A Brand's Posts live in the `post` table ([`store::posts`]);
//! the historical Posts are seeded from `store/seeds/neon/Post.yaml`. A Post
//! stores its authored Markdown, and this module renders it when served: the
//! CommonMark rendering with image sources routed through the asset seam, the
//! sanitizer every body passes through, and the copy-button decoration. The
//! rendering reads the deployment's asset base, which is why it happens at
//! request time rather than when the row is written.

/// The sanitizer every Post body passes through: the default safe tag and
/// URL-scheme allowlists, plus `class` for the authored layout grids.
fn sanitize(html: &str) -> String {
    ammonia::Builder::default()
        .add_generic_attributes(["class"])
        .clean(html)
        .to_string()
}

/// The served HTML for a Post's Markdown body.
#[must_use]
pub fn render_body(markdown: &str) -> String {
    views::components::code::decorate_copy_buttons(&sanitize(
        &crate::marketing::loader::render_markdown_html(markdown),
    ))
}

/// The served HTML for a Post: its Markdown rendered, or, for a row written
/// before Markdown was stored, its stored HTML.
#[must_use]
pub fn body_html(post: &store::posts::Post) -> String {
    post.body_markdown
        .as_deref()
        .map_or_else(|| post.body_html.clone(), render_body)
}

#[cfg(test)]
mod tests {
    use super::render_body;

    #[test]
    fn markdown_renders_and_is_sanitized() {
        let html = render_body("_Hi_\n\n<script>alert(1)</script>\n\n```\ncode\n```\n");
        assert!(html.contains("<em>Hi</em>"));
        assert!(!html.contains("<script"));
        assert!(html.contains("code"));
    }

    #[test]
    fn mermaid_fences_survive_sanitization_as_escaped_viewer_input() {
        let html = render_body("```mermaid\nflowchart TD\nA[<script>bad()</script>] --> B\n```\n");
        assert!(html.contains("class=\"language-mermaid\""));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("data-copy-code"));
    }

    #[test]
    fn images_route_through_the_asset_seam() {
        let html = render_body("![x](img/thanks-apple/collage-6.jpg)");
        assert!(
            !html.contains("src=\"img/thanks-apple"),
            "a repo-relative image source is rewritten: {html}"
        );
    }
}
