//! A record citation with a cropped passage and the original highlighted source page.
use dioxus::prelude::*;

/// Coordinates are fractions of the original page, measured from its top left.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct CitationRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Load the same progressive enhancement for server-rendered citations and dynamic review queues.
pub(crate) fn citation_highlight_assets() -> Element {
    rsx! {
        document::Stylesheet { href: "/public/css/citation-highlight.css" }
        document::Script { src: "/public/js/citation-highlight.js", defer: true }
    }
}

/// Show the cited page and optional pinned, authenticated Forgejo provenance.
#[component]
pub fn CitationHighlight(
    label: String,
    page: u32,
    image_url: String,
    rects: Vec<CitationRect>,
    #[props(default)] forgejo_url: Option<String>,
    #[props(default)] forgejo_title: Option<String>,
) -> Element {
    let forgejo_title = forgejo_title.filter(|_| forgejo_url.is_some());
    let regions = serde_json::to_string(&rects).unwrap_or_default();
    rsx! {
        {citation_highlight_assets()}
        nav-citation-highlight {
            "data-label": "{label}",
            "data-page": "{page}",
            "data-src": "{image_url}",
            "data-rects": "{regions}",
            "data-forgejo": forgejo_url,
            "data-forgejo-title": forgejo_title,
            // The citation remains readable until the interactive crop is available.
            p { "{label}, page {page}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_citation_is_readable_before_javascript_loads() {
        let mut dom = VirtualDom::new(|| {
            rsx! {
                CitationHighlight {
                    label: "Service certificate", page: 2, image_url: "/sample/page-2.png",
                    rects: vec![CitationRect { x: 0.1, y: 0.2, width: 0.3, height: 0.04 }],
                }
            }
        });
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains(">Service certificate, page 2</p>"), "{html}");
        assert!(!html.contains("data-forgejo"), "{html}");
    }
}
