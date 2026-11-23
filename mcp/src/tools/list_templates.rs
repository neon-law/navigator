//! Read-only discovery of public, bundled notation Templates.

use serde::Deserialize;
use serde_json::{json, Value};

use super::ToolError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    intent: Option<String>,
}

/// Clarify a bare request to create Work In Progress before suggesting a Template.
#[must_use]
pub fn clarify_intent(intent: &str) -> Option<&'static str> {
    let normalized = intent
        .trim()
        .trim_end_matches(['.', '?', '!'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "create a work in progress" | "create work in progress" | "create a wip" | "create wip"
    )
    .then_some(
        "Do you want a Work In Progress summary for a matter, or do you want to record time or a cost?",
    )
}

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "list_templates",
        "description": "List public Templates from Navigator's bundled shared catalog. For a short \
                        request such as “Create a work in progress”, pass that phrase as intent. Omit personal and matter details. \
                        Ask any returned clarification before suggesting a Template or taking action. \
                        Show intake_url to start or resume Nevada LLC formation. Use preview_url to browse agreements. \
                        Keep questionnaire answers in Navigator. \
                        Use the titles and descriptions to match clear requests, and ask when several fit.",
        "annotations": { "readOnlyHint": true, "destructiveHint": false, "openWorldHint": false },
        "inputSchema": {
            "type": "object",
            "properties": { "intent": { "type": "string" } },
            "additionalProperties": false
        }
    })
}

#[derive(serde::Serialize)]
struct CatalogEntry {
    #[serde(flatten)]
    template: store::seed::PublicTemplate,
    #[serde(skip_serializing_if = "Option::is_none")]
    intake_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_url: Option<String>,
}

pub fn call(arguments: &Value) -> Result<Value, ToolError> {
    let arguments: Arguments = serde_json::from_value(arguments.clone())
        .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
    if let Some(question) = arguments.intent.as_deref().and_then(clarify_intent) {
        return Ok(json!({
            "content": [{ "type": "text", "text": question }],
            "structuredContent": { "clarification": question }
        }));
    }

    let host = std::env::var("NAVIGATOR_PUBLIC_HOST").ok();
    let site = views::brand::BrandKey::Neon.public_home_href_for(host.as_deref());
    let templates: Vec<_> = store::seed::public_seeded_templates()
        .map_err(|error| ToolError::Internal(error.to_string()))?
        .into_iter()
        .map(|mut template| {
            let preview_url = template
                .start_path
                .as_deref()
                .and_then(|path| path.strip_suffix("/start"))
                .map(|path| format!("{site}{path}"));
            let intake_url = if template.code == store::personal_projects::FORMATION {
                template
                    .start_path
                    .as_ref()
                    .map(|path| format!("{site}{path}"))
            } else {
                template.start_path = None;
                None
            };
            CatalogEntry {
                template,
                intake_url,
                preview_url,
            }
        })
        .collect();
    let text = templates
        .iter()
        .map(|entry| {
            use std::fmt::Write as _;
            let template = &entry.template;
            let mut line = format!("{} ({})", template.title, template.code);
            if let Some(description) = &template.description {
                let _ = write!(line, " — {description}");
            }
            if let Some(url) = &entry.intake_url {
                let _ = write!(line, " — Start or resume intake: {url}");
            } else if let Some(url) = &entry.preview_url {
                let _ = write!(line, " — Preview: {url}");
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": { "templates": templates }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_returns_public_metadata_or_a_clarification() {
        assert!(super::super::is_advertised("list_templates"));
        assert!(!super::super::is_side_effecting("list_templates"));

        let result = call(&json!({})).unwrap();
        let templates = result["structuredContent"]["templates"].as_array().unwrap();
        assert!(templates.iter().all(|row| {
            row["description"]
                .as_str()
                .is_some_and(|description| !description.trim().is_empty())
        }));
        let text = result["content"][0]["text"].as_str().unwrap();
        for (code, slug) in [
            ("advisor_agreement", "advisor-agreement"),
            ("master_services_agreement", "master-services-agreement"),
            ("mutual_nda", "mutual-nda"),
            ("one_way_nda", "one-way-nda"),
        ] {
            let agreement = templates.iter().find(|row| row["code"] == code).unwrap();
            let preview = agreement["preview_url"].as_str().unwrap();
            assert!(preview.starts_with("https://"));
            assert!(preview.ends_with(&format!("/notations/{slug}")));
            assert!(agreement.get("intake_url").is_none());
            assert!(agreement.get("start_path").is_none());
            assert!(text.contains(preview));
            assert!(!text.contains(&format!("{preview}/start")));
        }
        assert!(templates
            .iter()
            .all(|row| row["code"] != "onboarding__letter"));
        let formation = templates
            .iter()
            .find(|row| row["code"] == "nv__llc_formation")
            .unwrap();
        assert_eq!(
            formation["start_path"],
            "/notations/nevada-llc-formation/start"
        );
        let link = formation["intake_url"].as_str().unwrap();
        assert!(
            link.starts_with("https://") && link.ends_with("/notations/nevada-llc-formation/start")
        );
        assert!(text.contains(link));
        assert!(templates
            .iter()
            .all(|row| row.get("body").is_none() && row.get("questionnaire").is_none()));

        let result = call(&json!({ "intent": "Create a work in progress!" })).unwrap();
        assert!(result["structuredContent"]["clarification"].is_string());
        assert!(result["structuredContent"].get("templates").is_none());
    }
}
