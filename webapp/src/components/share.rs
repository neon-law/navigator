//! `?via=` share-reference tags, as one pure URL builder.
//!
//! Shareable and outbound links on a brand's pages carry a `?via=<source-tag>`
//! query parameter so Plausible segments pageviews by tag with zero code
//! changes — pageview URLs keep their query string. The tag records which
//! surface the visitor followed (for example `home-qr-testimonials`); it is
//! never personal data, and landing on a tagged URL changes nothing about the
//! rendered page or lead capture (lead capture allowlists bare paths only).
//!
//! Tags in use on Cyber Injury Law:
//! - `home-nav-testimonials` — the header nav testimonial link
//! - `home-booking` — the header booking link to `/consultation`
//! - `home-qr-android`, `home-qr-iphone`, `home-qr-testimonials` — the
//!   connect-section QR links
//!
//! Add new tags here as share surfaces ship.

/// Append a `via` share-reference tag to an href.
///
/// An href that already carries a `via` parameter is returned unchanged —
/// one link points from one surface, so a second tag would misattribute it.
/// Anything that is not a tag-shaped value (lowercase letters, digits, and
/// hyphens) leaves the href untouched: tags are code constants, and a
/// malformed one must never corrupt a link.
pub fn via_tagged(href: &str, tag: &str) -> String {
    if !is_tag(tag) {
        return href.to_string();
    }
    let (base, fragment) = match href.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (href, None),
    };
    match base.split_once('?') {
        Some((_path, query)) if query.split('&').any(is_via_param) => href.to_string(),
        Some((path, query)) if query.is_empty() => finish(&format!("{path}?via={tag}"), fragment),
        Some((path, query)) => finish(&format!("{path}?{query}&via={tag}"), fragment),
        None => finish(&format!("{base}?via={tag}"), fragment),
    }
}

/// A bare `via` or `via=<any value>` parameter.
fn is_via_param(param: &str) -> bool {
    param == "via" || param.starts_with("via=")
}

fn is_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn finish(tagged: &str, fragment: Option<&str>) -> String {
    match fragment {
        Some(f) => format!("{tagged}#{f}"),
        None => tagged.to_string(),
    }
}
