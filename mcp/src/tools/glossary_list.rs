//! `glossary_list` MCP tool.
//!
//! `navigator glossary list` as a tool: the title and one-sentence
//! description of every term in `docs/glossary/`, read from
//! [`store::glossary::terms`] — the same embedded entries the CLI and the
//! `/glossary` page read. Sorted by title, as the CLI prints them.

use serde_json::{json, Value};

use super::ToolError;

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "glossary_list",
        "description": "List every term in the Neon Law Navigator glossary with its one-sentence \
                        description, sorted by title. Use this to find the firm's word for a \
                        concept before naming or explaining it, then call glossary_show for a \
                        term's full definition. Takes no arguments.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }
    })
}

#[allow(clippy::unused_async)]
pub async fn call(_arguments: &Value) -> Result<Value, ToolError> {
    let mut entries: Vec<_> = store::glossary::terms().iter().collect();
    entries.sort_by(|a, b| a.title.cmp(&b.title));

    let terms: Vec<Value> = entries
        .iter()
        .map(|term| json!({ "title": term.title, "description": term.description }))
        .collect();
    let text = entries
        .iter()
        .map(|term| format!("{} — {}", term.title, term.description))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": {
            "count": terms.len(),
            "terms": terms,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{call, descriptor};
    use serde_json::json;

    #[test]
    fn descriptor_names_the_tool_and_takes_no_arguments() {
        let d = descriptor();
        assert_eq!(d["name"], "glossary_list");
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
        assert!(d["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn text_matches_the_cli_plain_listing() {
        let r = call(&json!({})).await.unwrap();
        let text = r["content"][0]["text"].as_str().unwrap();
        let matter = store::glossary::find("Matter").unwrap();
        assert!(text
            .lines()
            .any(|line| line == format!("Matter — {}", matter.description)));
    }
}
