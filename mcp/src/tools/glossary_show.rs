//! `glossary_show` MCP tool.
//!
//! `navigator glossary show <term>` as a tool: one term's full
//! definition, found by title in any case through
//! [`store::glossary::find`], the lookup the CLI uses.
//!
//! An unknown term is refused without echoing it, for the same reason the
//! CLI does not: the caller already has what they typed, and a tool error
//! travels into logs and model transcripts.

use serde::Deserialize;
use serde_json::{json, Value};

use super::ToolError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    term: String,
}

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "glossary_show",
        "description": "Show one Neon Law Navigator glossary term's full definition. The term is \
                        its title as glossary_list prints it, in any case (\"Lawyer Review\"). \
                        Use this to ground an answer in the firm's own vocabulary.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "term": {
                    "type": "string",
                    "description": "The term's title, as glossary_list prints it."
                }
            },
            "required": ["term"],
            "additionalProperties": false
        }
    })
}

#[allow(clippy::unused_async)]
pub async fn call(arguments: &Value) -> Result<Value, ToolError> {
    let args: Args = super::decode_args(arguments)?;
    let term = store::glossary::find(args.term.trim()).ok_or_else(|| {
        ToolError::NotFound(
            "no glossary term has that title; call glossary_list for every title".into(),
        )
    })?;
    let body = term.body.trim();

    Ok(json!({
        "content": [{ "type": "text", "text": format!("## {}\n\n{body}", term.title) }],
        "structuredContent": {
            "title": term.title,
            "description": term.description,
            "body": body,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{call, descriptor};
    use serde_json::json;

    #[test]
    fn descriptor_requires_exactly_a_term() {
        let d = descriptor();
        assert_eq!(d["name"], "glossary_show");
        assert_eq!(d["inputSchema"]["required"], json!(["term"]));
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
    }

    #[tokio::test]
    async fn text_is_the_cli_heading_and_body() {
        let r = call(&json!({ "term": "  Lawyer Review " })).await.unwrap();
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("## Lawyer Review\n\n"), "{text}");
    }

    #[tokio::test]
    async fn an_unexpected_argument_is_invalid() {
        let error = call(&json!({ "term": "Matter", "extra": 1 }))
            .await
            .unwrap_err();
        assert!(matches!(error, super::ToolError::InvalidArguments(_)));
    }
}
