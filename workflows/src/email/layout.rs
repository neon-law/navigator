//! HTML email uses inline brand colors and font stacks so the message remains
//! legible in clients that strip stylesheets or block hosted fonts and images.

/// Env var carrying the public origin the logo PNG is served from.
/// Mirrors `portal::openapi`'s base-URL knob so a single value drives
/// both the OpenAPI doc and email assets.
const BASE_URL_ENV: &str = "NAV_BASE_URL";

/// OSS placeholder origin. Real deploys set [`BASE_URL_ENV`]; this
/// default matches the one in `portal::openapi` so the repo ships no
/// hard-coded NeonLaw hostname.
const DEFAULT_BASE_URL: &str = "https://www.your-domain.example";

/// Resolve the public origin for email assets from [`BASE_URL_ENV`],
/// falling back to [`DEFAULT_BASE_URL`]. Any trailing slash is left
/// for [`render_email_html`] to trim.
#[must_use]
pub fn base_url_from_env() -> String {
    if !views::brand::base_url().is_empty() {
        return views::brand::base_url().to_string();
    }
    std::env::var(BASE_URL_ENV).unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

/// The wordmark every email signs with, from the mounted brand bundle.
///
/// A rebranded fork greets under its own name without touching a template: the
/// subject lines, the salutations, and the footer all read this.
#[must_use]
pub fn brand_name() -> &'static str {
    views::brand::FIRM_BRAND.site_name
}

/// The inbound address every email's footer invites a reply to, from the
/// mounted bundle.
///
/// Deliberately not the envelope `From`, which is `support@` the serving
/// host's naked domain (`views::brand::sender_email_for_host`) or
/// [`crate::email::DEFAULT_FROM_EMAIL`]: what the site publishes and what the
/// pipeline sends from are allowed to differ.
#[must_use]
pub fn support_email() -> &'static str {
    views::brand::firm_email()
}

/// Render Markdown in the request brand's email layout.
#[must_use]
pub fn render_email_html(content_markdown: &str, base_url: &str) -> String {
    render_branded_email_html(content_markdown, base_url, None)
}

/// Apply the same catalog overrides as the brand's site and prefer its uploaded raster logo.
#[must_use]
pub fn render_branded_email_html(
    content_markdown: &str,
    base_url: &str,
    brand: Option<&store::brands::Brand>,
) -> String {
    render_email_html_with_contact(content_markdown, base_url, brand, support_email())
}

/// Render a Brand's layout with its resolved contact address.
#[must_use]
pub fn render_email_html_with_contact(
    content_markdown: &str,
    base_url: &str,
    brand: Option<&store::brands::Brand>,
    contact: &str,
) -> String {
    let key = views::brand::brand_key();
    let (face, palette) = views::brand::resolve_presentation(
        brand.and_then(|b| b.typeface.as_deref()),
        brand.and_then(|b| b.primary_color.as_deref()),
        Some(key),
    )
    .expect("invariant: every compiled brand has a typeface and palette");
    let scheme = &palette.light;
    let font_css = views::brand_presentation::webfont_css_at(face, base_url).unwrap_or_default();
    let font = face.stack.replace('"', "'");
    let primary = scheme.primary;
    let link = scheme.link;
    let background = scheme.bg.unwrap_or(scheme.surface_subtle);
    let surface = scheme.surface.unwrap_or("#ffffff");
    let text = scheme.text.unwrap_or("#212529");
    let muted = scheme.text_muted.unwrap_or("#59636f");
    let border = scheme.border.unwrap_or("#dee2e6");

    let content_html = views::markdown::render_document(content_markdown)
        .replace("<a href=", &format!(r#"<a style="color:{link};" href="#));
    let alt = escape_attribute(brand_name());
    let support = escape_attribute(contact);
    let uploaded = brand
        .filter(|b| b.logo_content_type.as_deref() == Some("image/png"))
        .and_then(|b| b.logo_object_key.as_deref())
        .map(|key| format!("/assets/{key}"));
    let logo = uploaded
        .as_deref()
        .unwrap_or(views::brand::FIRM_BRAND.social_image);
    let mark = if logo.is_empty() {
        format!("<strong>{alt}</strong>")
    } else {
        let src = escape_attribute(&views::assets::absolute_url_at(base_url, logo));
        format!(
            r#"<img src="{src}" alt="{alt}" width="120" style="display:block;width:120px;height:auto;border:0;">"#
        )
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>{font_css}</style>
</head>
<body style="margin:0;padding:0;background:{background};">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:{background};">
<tr><td align="center" style="padding:24px 12px;">
<table role="presentation" width="600" cellpadding="0" cellspacing="0" style="width:100%;max-width:600px;background:{surface};border-top:4px solid {primary};border-radius:8px;">
<tr><td style="padding:32px 32px 0;font-family:{font};font-size:22px;color:{text};">
{mark}
</td></tr>
<tr><td style="padding:8px 32px 24px;font-family:{font};font-size:16px;line-height:1.5;color:{text};">
{content_html}</td></tr>
<tr><td style="padding:16px 32px 28px;border-top:1px solid {border};font-family:{font};font-size:13px;line-height:1.5;color:{muted};">
{alt} · Reach us any time at <a href="mailto:{support}" style="color:{link};">{support}</a>.
</td></tr>
</table>
</td></tr>
</table>
</body>
</html>
"#
    )
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::{base_url_from_env, render_email_html};

    #[test]
    fn renders_markdown_body_into_html() {
        let html = render_email_html("Hi **Aries**", "https://example.test");
        assert!(
            html.contains("<strong>Aries</strong>"),
            "markdown is rendered"
        );
        assert!(html.starts_with("<!doctype html>"), "full document");
    }

    #[test]
    fn embeds_logo_png_at_base_url_and_trims_trailing_slash() {
        let html = render_email_html("body", "https://example.test/");
        // Served from the exempt `/public` mount, not a gated site root.
        assert!(html.contains(r#"src="https://example.test/public/logo.png""#));
        assert!(html.contains(r#"alt="Neon Law""#));
        // No double slash from a trailing-slash base.
        assert!(!html.contains("example.test//public/logo.png"));
        // Never reference the SVG — clients won't render it.
        assert!(!html.contains("logo.svg"));
    }

    #[tokio::test]
    async fn brand_without_a_raster_uses_a_wordmark_and_its_own_theme() {
        let key = views::brand::BrandKey::LexTecnica;
        views::brand::scope(
            key.resolve_branding(&views::brand::DEFAULT_BRANDING),
            async {
                let html =
                    render_email_html("[Reset](https://mail.test/reset)", "https://mail.test");
                assert!(html.contains("<strong>Lex Tecnica</strong>"));
                assert!(!html.contains("<img"));
                assert!(html.contains("font-family:'Tinos', 'Times New Roman', serif"));
                assert!(html.contains("https://mail.test/public/fonts/tinos/Tinos-Regular.woff2"));
                assert!(html.contains(r#"style="color:#000000;" href="https://mail.test/reset""#));
            },
        )
        .await;
    }

    #[test]
    fn footer_carries_the_brand_support_address() {
        // The address a recipient is invited to write back to is the one the
        // public site publishes (`views::brand`'s `firm_email`), not the envelope
        // `From` — that is `DEFAULT_FROM_EMAIL`, still `support@`, and the two are
        // deliberately allowed to differ.
        let firm = render_email_html("body", "https://b.test");
        assert!(
            firm.contains("mailto:contact@neonlaw.com"),
            "the footer should carry the firm's published address: {firm}"
        );
    }

    #[test]
    fn font_origin_cannot_break_out_of_the_email_style_block() {
        // The font origin reaches a single-quoted CSS `url('…')` inside a raw
        // `<style>` block. A hostile asset base carrying a quote or `</style>`
        // (here via the site-URL fallback) must be CSS-escaped so it can
        // neither terminate the rule nor close the style element.
        let html = render_email_html(
            "body",
            "https://evil.test/x'</style><script>alert(1)</script>",
        );
        // Inspect the raw `<style>` element in isolation: nothing between its
        // open tag and the first `</style>` may carry markup, or the injected
        // `</style>` would have closed the element early.
        let style_block = html
            .split_once("<style>")
            .and_then(|(_, rest)| rest.split_once("</style>"))
            .map(|(inner, _)| inner)
            .expect("email head must contain a <style> block");
        assert!(
            !style_block.contains('<'),
            "no raw markup may reach the email <style> block: {style_block}"
        );
        assert!(
            style_block.contains(r"\3C /style\3E \3C script\3E "),
            "angle brackets must be CSS-escaped inside the email font url: {style_block}"
        );
    }

    #[test]
    fn autolinks_in_angle_brackets_become_anchors() {
        let html = render_email_html("Visit <https://neonlaw.example> today", "https://b.test");
        assert!(html.contains(r#"href="https://neonlaw.example""#));
    }

    #[test]
    fn base_url_from_env_defaults_to_oss_placeholder_when_unset() {
        // The real value is injected via NAV_BASE_URL in deploys; the
        // default must stay a generic placeholder (no NeonLaw host).
        if std::env::var("NAV_BASE_URL").is_err() {
            assert_eq!(base_url_from_env(), "https://www.your-domain.example");
        }
    }
}
