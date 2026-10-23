//! The feed: a vertical list of dated events on a timeline rail, one card per
//! event, with a year marker wherever the year changes.
//!
//! Presentation only. The posts carry a matter's record, so the page that
//! mounts the feed loads them; this component never does.

use dioxus::prelude::*;

use super::{is_external_href, Badge, BadgeTone, ExternalLink};

/// The token family a post's avatar disc draws from. Named for the token, not
/// a colour, so the name stays true under every brand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeedAccent {
    Brand,
    #[default]
    Link,
    Danger,
    Success,
    Warning,
    Neutral,
}

impl FeedAccent {
    fn name(self) -> &'static str {
        match self {
            FeedAccent::Brand => "brand",
            FeedAccent::Link => "link",
            FeedAccent::Danger => "danger",
            FeedAccent::Success => "success",
            FeedAccent::Warning => "warning",
            FeedAccent::Neutral => "neutral",
        }
    }
}

/// A link from a post to the record behind it: a filing, an order, a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedSource {
    pub label: String,
    pub href: String,
}

/// One event in a [`Feed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedPost {
    pub id: String,
    /// ISO date (`YYYY-MM-DD`). Drives the `<time>` element and year markers.
    pub date: String,
    /// The date as shown on the card.
    pub date_label: String,
    pub actor: String,
    /// A secondary line under the actor: firm, side, department.
    pub role: Option<String>,
    /// One to three characters for the avatar disc.
    pub initials: String,
    pub accent: FeedAccent,
    /// The event category, shown as a badge beside the title.
    pub kind: Option<String>,
    /// The tone of the kind badge.
    pub tone: BadgeTone,
    pub title: String,
    pub body: String,
    pub sources: Vec<FeedSource>,
}

impl FeedPost {
    fn year(&self) -> &str {
        self.date.get(..4).unwrap_or(&self.date)
    }
}

/// Posts in display order, with a year marker where the year changes.
#[component]
pub fn Feed(aria_label: String, posts: Vec<FeedPost>) -> Element {
    rsx! {
        ol { class: "nav-feed", "aria-label": "{aria_label}",
            for (index, post) in posts.iter().enumerate() {
                li { key: "{post.id}", class: "nav-feed__item",
                    if index == 0 || posts[index - 1].year() != post.year() {
                        p { class: "nav-feed__year", "{post.year()}" }
                    }
                    FeedCard { post: post.clone() }
                }
            }
        }
    }
}

#[component]
fn FeedCard(post: FeedPost) -> Element {
    rsx! {
        article { class: "nav-feed__post", "data-post": "{post.id}",
            header { class: "nav-feed__head",
                span {
                    class: "nav-feed__avatar",
                    "data-accent": post.accent.name(),
                    "aria-hidden": "true",
                    "{post.initials}"
                }
                div {
                    p { class: "nav-feed__actor", "{post.actor}" }
                    if let Some(role) = &post.role {
                        p { class: "nav-feed__role", "{role}" }
                    }
                }
                time { class: "nav-feed__date", datetime: "{post.date}", "{post.date_label}" }
            }
            h3 { class: "nav-feed__title",
                "{post.title}"
                if let Some(kind) = &post.kind {
                    " "
                    Badge { tone: post.tone, "{kind}" }
                }
            }
            p { class: "nav-feed__body", "{post.body}" }
            if !post.sources.is_empty() {
                footer { class: "nav-feed__sources",
                    for source in post.sources.iter() {
                        if is_external_href(&source.href) {
                            ExternalLink { href: source.href.clone(), "{source.label}" }
                        } else {
                            a { href: "{source.href}", "{source.label}" }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(id: &str, date: &str) -> FeedPost {
        FeedPost {
            id: id.to_string(),
            date: date.to_string(),
            date_label: date.to_string(),
            actor: "Libra".to_string(),
            role: Some("Counsel".to_string()),
            initials: "LI".to_string(),
            accent: FeedAccent::Brand,
            kind: Some("Filing".to_string()),
            tone: BadgeTone::Ready,
            title: "Complaint filed".to_string(),
            body: "Filed with the clerk.".to_string(),
            sources: vec![FeedSource {
                label: "Docket".to_string(),
                href: "/design".to_string(),
            }],
        }
    }

    #[test]
    fn marks_the_year_only_where_it_changes() {
        fn app() -> Element {
            rsx! {
                Feed {
                    aria_label: "Matter history".to_string(),
                    posts: vec![post("a", "2025-11-02"), post("b", "2025-12-09"), post("c", "2026-01-15")],
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(
            html.starts_with(r#"<ol class="nav-feed" aria-label="Matter history">"#),
            "{html}"
        );
        assert_eq!(html.matches("nav-feed__year").count(), 2, "{html}");
        assert!(
            html.contains(r#"<time class="nav-feed__date" datetime="2025-11-02""#),
            "{html}"
        );
        assert!(
            html.contains(r#"data-accent="brand" aria-hidden="true""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="nav-badge nav-badge--ready">Filing</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"<a href="/design">Docket</a>"#), "{html}");
    }

    #[test]
    fn an_off_site_source_opens_in_a_new_tab() {
        fn app() -> Element {
            let mut p = post("a", "2026-01-15");
            p.sources = vec![FeedSource {
                label: "Court record".to_string(),
                href: "https://example.com/record".to_string(),
            }];
            rsx! { Feed { aria_label: "History".to_string(), posts: vec![p] } }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains(r#"target="_blank""#), "{html}");
        assert!(html.contains("opens in a new tab"), "{html}");
    }
}
