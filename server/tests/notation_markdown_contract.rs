use dioxus::prelude::*;
use scraper::{Html, Selector};
use std::collections::BTreeMap;
use std::path::Path;

const DOCUMENT: &str = r#"# Title

## Section

### Three

#### Four

##### Five

###### Six

> First paragraph
> with a continuation
>
> Second paragraph
>
> Third paragraph

1. **Bold**
2. *Italic*

- [Link](https://example.test)

{{client.signature}} {{firm.initials}} {{firm.date}}
"#;

#[test]
fn notation_markdown_surfaces_preserve_document_structure() {
    let outline = views::harvard_outline::parse(DOCUMENT);
    let matter_document = webapp::notation_outline::notation_document(&outline);
    let matter_page = dioxus_ssr::render_element(rsx! {
        webapp::components::NotationDocument {
            document: matter_document,
            id: "fixture-document".to_owned(),
            open: true,
        }
    });
    let surfaces = [
        (
            "lawyer review",
            views::notation::render_filled_in(DOCUMENT, &BTreeMap::new()),
        ),
        ("matter document", matter_page),
        (
            "matter outline",
            views::harvard_outline::stage_html(&outline),
        ),
        (
            "Draft preview",
            webapp::notation_draft::project_draft_source(DOCUMENT).stage_html,
        ),
        (
            "Notation preview",
            portal::notation_preview_doc::from_markdown("fixture", "", DOCUMENT).stage_html,
        ),
        (
            "email body",
            workflows::email::render_email_html(DOCUMENT, "https://example.test"),
        ),
    ];

    for (surface, html) in surfaces {
        let body = html
            .split_once("</head>")
            .map_or(html.as_str(), |(_, body)| body);
        let fragment = Html::parse_fragment(body);
        let select = |selector| Selector::parse(selector).unwrap();
        for heading in ["h1", "h2", "h3", "h4", "h5", "h6"] {
            assert!(
                fragment.select(&select(heading)).count() >= 1,
                "{surface}: missing {heading}: {body}"
            );
        }
        assert_eq!(
            fragment.select(&select("blockquote")).count(),
            1,
            "{surface}"
        );
        assert_eq!(
            fragment.select(&select("blockquote > p")).count(),
            3,
            "{surface}"
        );
        for selector in ["ol > li > strong", "ol > li > em", "ul > li > a"] {
            assert_eq!(
                fragment.select(&select(selector)).count(),
                1,
                "{surface}: {selector}"
            );
        }
        assert_eq!(
            fragment.select(&select(".notation-signing-field")).count(),
            3,
            "{surface}"
        );
        let text: String = fragment.root_element().text().collect();
        for syntax in ["#", "> ", "{{"] {
            assert!(!text.contains(syntax), "{surface}: {text}");
        }
    }
}

#[test]
fn document_renderer_call_sites_are_covered_by_the_surface_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let adapters = [
        (
            "markdown::render_document(",
            &[
                ("views/src/harvard_outline.rs", 2),
                ("views/src/notation.rs", 1),
                ("webapp/src/notation_outline.rs", 1),
                ("workflows/src/email/layout.rs", 1),
            ][..],
        ),
        (
            "harvard_outline::stage_html(",
            &[
                ("neon/src/firm_pages.rs", 1),
                ("portal/src/notation_preview_doc.rs", 1),
                ("webapp/src/notation_draft.rs", 1),
                ("webapp/src/notation_outline.rs", 1),
            ][..],
        ),
        (
            "notation_outline::notation_document(",
            &[
                ("webapp/src/design.rs", 1),
                ("webapp/src/sections/engagement.rs", 1),
                ("webapp/src/sections/notations.rs", 1),
            ][..],
        ),
    ];
    let mut found = vec![BTreeMap::new(); adapters.len()];
    for crate_dir in [
        "neon",
        "portal",
        "views",
        "webapp",
        "workflows",
        "workflows-service",
    ] {
        for entry in walkdir::WalkDir::new(root.join(crate_dir).join("src")) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file()
                || entry.path().extension().is_none_or(|ext| ext != "rs")
            {
                continue;
            }
            let path = entry.path().strip_prefix(root).unwrap().to_str().unwrap();
            let source = std::fs::read_to_string(entry.path()).unwrap();
            for ((adapter, _), found) in adapters.iter().zip(&mut found) {
                let calls = source.matches(adapter).count();
                if calls > 0 {
                    found.insert(path.to_owned(), calls);
                }
            }
        }
    }
    for ((adapter, expected), found) in adapters.into_iter().zip(found) {
        let expected = expected
            .iter()
            .map(|&(path, count)| (path.to_owned(), count))
            .collect();
        assert_eq!(
            found, expected,
            "Add the surface using {adapter} to the Markdown contract"
        );
    }
}
