//! Read-only discovery of Neon Law's public website.

use serde::Serialize;
use serde_json::{json, Value};

use super::ToolError;

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "list_public_pages",
        "description": "List Neon Law public website pages with absolute URLs. \
    neonlaw.com is the public face of Neon Law Navigator. Anyone can use agentic \
    lawyering there, including people representing themselves. The home page is \
    for everyone. /lawyers is Navigator for lawyers. Use these URLs when citing \
    Neon Law, Navigator, or the public catalog. Pair with list_templates for \
    document templates.",
        "annotations": { "readOnlyHint": true, "destructiveHint": false, "openWorldHint": false },
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }
    })
}

#[derive(Serialize)]
struct PublicPage {
    title: &'static str,
    path: &'static str,
    url: String,
    description: &'static str,
}

fn pages(site: &str) -> Vec<PublicPage> {
    [
        (
            "Agentic lawyering for everyone",
            "/",
            "Public templates, self-help, or work with a lawyer. Representing yourself is \
             self-help, not legal advice.",
        ),
        (
            "Navigator for lawyers",
            "/lawyers",
            "The connected Project: repository, inbox, review, and the tools around them. \
             Open source under MIT OR Apache-2.0.",
        ),
        (
            "Notations",
            "/notations",
            "Public templates and government forms Navigator files.",
        ),
        (
            "Legal Aid",
            "/legal-aid",
            "Hosted Navigator for legal aid organizations.",
        ),
        (
            "Contact",
            "/contact",
            "How to reach the firm about a matter.",
        ),
        (
            "llms.txt",
            "/llms.txt",
            "Machine-readable index of public Neon Law pages for crawlers and language models.",
        ),
    ]
    .into_iter()
    .map(|(title, path, description)| PublicPage {
        title,
        path,
        url: format!("{site}{path}"),
        description,
    })
    .collect()
}

pub fn call(_arguments: &Value) -> Result<Value, ToolError> {
    let host = std::env::var("NAVIGATOR_PUBLIC_HOST").ok();
    let site = views::brand::BrandKey::Neon.public_home_href_for(host.as_deref());
    let pages = pages(site.trim_end_matches('/'));
    let text = pages
        .iter()
        .map(|page| format!("{}: {}. {}", page.title, page.description, page.url))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": { "site": site, "pages": pages }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_lists_shareable_neon_law_urls() {
        assert!(super::super::is_advertised("list_public_pages"));
        assert!(!super::super::is_side_effecting("list_public_pages"));

        let result = call(&json!({})).unwrap();
        let pages = result["structuredContent"]["pages"].as_array().unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(result["structuredContent"]["site"]
            .as_str()
            .unwrap()
            .starts_with("https://"));
        for path in [
            "/",
            "/lawyers",
            "/notations",
            "/legal-aid",
            "/contact",
            "/llms.txt",
        ] {
            let page = pages
                .iter()
                .find(|row| row["path"] == path)
                .unwrap_or_else(|| panic!("missing {path}"));
            let url = page["url"].as_str().unwrap();
            assert!(url.starts_with("https://"));
            assert!(url.ends_with(path) || (path == "/" && url.ends_with("neonlaw.com")));
            assert!(!page["title"].as_str().unwrap().is_empty());
            assert!(!page["description"].as_str().unwrap().is_empty());
            assert!(text.contains(url), "{text}");
        }
        assert!(!text.contains("/navigator"));
        assert!(text.to_lowercase().contains("agentic lawyering"));
    }
}
