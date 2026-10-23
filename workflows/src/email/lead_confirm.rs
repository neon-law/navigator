//! Lead-confirmation template + render.
//!
//! Sent **directly from `web`** when someone submits a public lead form: the
//! signed link confirms the address, makes the lead a Client, and signs that
//! Client in — see `portal::lead_confirm`. Lives beside
//! [`email_confirm`](super::email_confirm) so all outbound copy shares one
//! home and one brand seam.

use super::Template;

/// Subject, resolved through the mounted firm brand. Mirrors the template's
/// `subject:` frontmatter.
#[must_use]
pub fn lead_confirm_subject() -> String {
    format!("Confirm your email for {}", super::layout::brand_name())
}

/// Raw template body (markdown with YAML frontmatter), bundled via
/// `include_str!`.
pub const LEAD_CONFIRM_TEMPLATE: &str = include_str!("../../content/email/lead_confirm.md");

/// Static [`Template`] entry used by [`super::template_for_slug`].
pub const TEMPLATE: Template = Template {
    subject: "Confirm your email for Neon Law",
    raw: LEAD_CONFIRM_TEMPLATE,
};

/// Render the body: strip the frontmatter, then substitute the recipient
/// tokens (`{{client_email}}`, `{{confirm_url}}`) and the brand tokens.
#[must_use]
pub fn render_lead_confirm_body(email: &str, confirm_url: &str) -> String {
    super::strip_frontmatter(LEAD_CONFIRM_TEMPLATE)
        .replace("{{client_email}}", email)
        .replace("{{confirm_url}}", confirm_url)
        .replace("{{brand}}", super::layout::brand_name())
        .replace("{{support_email}}", super::layout::support_email())
}

/// Render the HTML alternative wrapped in the firm email layout.
#[must_use]
pub fn render_lead_confirm_html(email: &str, confirm_url: &str, base_url: &str) -> String {
    super::layout::render_email_html(&render_lead_confirm_body(email, confirm_url), base_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_carries_the_link_and_the_google_offer_and_no_tokens() {
        let body = render_lead_confirm_body(
            "visitor@example.com",
            "https://example.test/leads/confirm?token=abc",
        );
        assert!(body.contains("`visitor@example.com`"), "{body}");
        assert!(
            body.contains("(https://example.test/leads/confirm?token=abc)"),
            "{body}"
        );
        assert!(body.contains("sign in with Google"), "{body}");
        assert!(!body.contains("{{"), "every token substituted: {body}");
        assert!(!body.starts_with("---"), "{body}");
    }
}
