//! Leads by social platform, read from Plausible for the social dashboard.
//!
//! Navigator stores no source on a lead. Plausible already knows it: the
//! social publisher tags every brand link with `?ref=<platform>`, Plausible
//! records that as the visit's source, and the server-side conversion goals
//! (`portal::plausible::ConversionReporter`) join the same visit. This module
//! asks Plausible's Stats API for those goals broken down by source, keeps the
//! social platforms, and renders the counts.
//!
//! Read-only and optional: with no `PLAUSIBLE_STATS_API_KEY` the section says
//! how to turn it on, and a brand Plausible cannot answer for is skipped.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// The Plausible goal a confirmed contact-form submission reports.
pub const CONTACT_GOAL: &str = "Contact: Submitted";
/// The Plausible goal a client's confirmed "start" reports.
pub const INTAKE_GOAL: &str = "Intake: Started";

/// The environment variable holding a Plausible Stats API key.
pub const STATS_API_KEY_ENV: &str = "PLAUSIBLE_STATS_API_KEY";

/// The window every count covers.
pub const DATE_RANGE: &str = "30d";

/// One brand's conversions from one social platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadSourceRow {
    pub brand: String,
    pub platform: String,
    pub contacts: u64,
    pub intakes: u64,
}

/// The section's data: whether Plausible is connected, and the rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LeadSourcesView {
    pub configured: bool,
    pub rows: Vec<LeadSourceRow>,
}

/// The social platform a Plausible source name stands for, as its display
/// label. Plausible names a tagged visit by its `ref` value (`linkedin`) or,
/// for an untagged one, by the referrer it recognised (`LinkedIn`, `t.co`).
#[must_use]
pub fn platform_label(source: &str) -> Option<&'static str> {
    Some(match source.trim().to_ascii_lowercase().as_str() {
        "linkedin" | "lnkd.in" => "LinkedIn",
        "facebook" | "fb" | "m.facebook.com" | "l.facebook.com" => "Facebook",
        "instagram" | "ig" | "l.instagram.com" => "Instagram",
        "threads" | "threads.net" => "Threads",
        "youtube" | "youtube.com" => "YouTube",
        "x" | "twitter" | "t.co" => "X",
        "tiktok" | "tiktok.com" => "TikTok",
        _ => return None,
    })
}

/// The Stats API v2 query body for `domain`: goal events by visit source.
#[must_use]
pub fn stats_query(domain: &str) -> serde_json::Value {
    serde_json::json!({
        "site_id": domain,
        "metrics": ["events"],
        "date_range": DATE_RANGE,
        "dimensions": ["visit:source", "event:goal"],
        "filters": [["is", "event:goal", [CONTACT_GOAL, INTAKE_GOAL]]],
    })
}

/// Fold a Stats API response into `brand`'s per-platform rows, dropping
/// sources that are not social platforms. Rows come back in platform order.
#[must_use]
pub fn rows_from_response(brand: &str, body: &serde_json::Value) -> Vec<LeadSourceRow> {
    let mut rows: Vec<LeadSourceRow> = Vec::new();
    let results = body["results"].as_array().map_or(&[][..], Vec::as_slice);
    for result in results {
        let (Some(source), Some(goal), Some(count)) = (
            result["dimensions"][0].as_str(),
            result["dimensions"][1].as_str(),
            result["metrics"][0].as_u64(),
        ) else {
            continue;
        };
        let Some(platform) = platform_label(source) else {
            continue;
        };
        let index = if let Some(index) = rows.iter().position(|row| row.platform == platform) {
            index
        } else {
            rows.push(LeadSourceRow {
                brand: brand.to_string(),
                platform: platform.to_string(),
                contacts: 0,
                intakes: 0,
            });
            rows.len() - 1
        };
        match goal {
            CONTACT_GOAL => rows[index].contacts += count,
            INTAKE_GOAL => rows[index].intakes += count,
            _ => {}
        }
    }
    rows.sort_by(|a, b| a.platform.cmp(&b.platform));
    rows
}

/// Ask Plausible at `origin` for `domain`'s goal breakdown. `None` when
/// Plausible refuses or cannot be reached; the caller skips that brand.
#[cfg(feature = "server")]
pub async fn fetch_breakdown(
    http: &reqwest::Client,
    origin: &str,
    api_key: &str,
    domain: &str,
) -> Option<serde_json::Value> {
    let response = http
        .post(format!("{origin}/api/v2/query"))
        .bearer_auth(api_key)
        .json(&stats_query(domain))
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => response.json().await.ok(),
        Ok(response) => {
            tracing::warn!(status = %response.status(), domain, "plausible stats query refused");
            None
        }
        Err(error) => {
            tracing::warn!(error = %error, domain, "plausible stats query failed");
            None
        }
    }
}

#[server]
pub async fn lead_sources() -> Result<LeadSourcesView, ServerFnError> {
    let role = crate::admin_listing::require_admin().await?;
    let store_role = match role {
        crate::people::ViewerRole::Owner => store::persons::Role::Owner,
        crate::people::ViewerRole::Admin => store::persons::Role::Admin,
        _ => return Err(ServerFnError::new("admin access required")),
    };
    let Some(api_key) = std::env::var(STATS_API_KEY_ENV)
        .ok()
        .filter(|key| !key.trim().is_empty())
    else {
        return Ok(LeadSourcesView::default());
    };
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let person = crate::admin_listing::injected_person_id()
        .await
        .ok_or_else(|| ServerFnError::new("admin access required"))?;
    let brands = store::posts::manageable_brand_keys(&surreal, store_role, person)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let mut rows = Vec::new();
    for brand in brands {
        let Some(key) = views::brand::BrandKey::parse(&brand) else {
            continue;
        };
        let domain = key
            .resolve_branding(&views::brand::DEFAULT_BRANDING)
            .primary_domain;
        if let Some(body) = fetch_breakdown(&http, "https://plausible.io", &api_key, domain).await {
            rows.extend(rows_from_response(&brand, &body));
        }
    }
    Ok(LeadSourcesView {
        configured: true,
        rows,
    })
}

/// The social dashboard's "Leads by platform" section. Loads on its own, so a
/// slow Plausible never holds up the publishing schedule above it.
#[component]
pub fn LeadSourcesSection() -> Element {
    let resource = use_server_future(lead_sources)?;
    let state = resource.read();
    match &*state {
        Some(Ok(view)) => lead_sources_body(view),
        Some(Err(_)) => rsx! {
            section { id: "lead-sources", h2 { "Leads by platform" }
                p { "Lead counts are unavailable right now." } }
        },
        None => rsx! {
            section { id: "lead-sources", h2 { "Leads by platform" } p { "Loading lead counts…" } }
        },
    }
}

fn lead_sources_body(view: &LeadSourcesView) -> Element {
    rsx! {
        section { id: "lead-sources", "aria-labelledby": "lead-sources-heading",
            h2 { id: "lead-sources-heading", "Leads by platform" }
            if !view.configured {
                p { "Set {STATS_API_KEY_ENV} to a Plausible Stats API key to see which platforms bring in leads." }
            } else if view.rows.is_empty() {
                p { "No leads from social platforms in the last 30 days." }
            } else {
                p { "Contact forms submitted and intakes started in the last 30 days, by the platform the visit came from." }
                table {
                    thead { tr { th { "Brand" } th { "Platform" } th { "Contact forms" } th { "Intakes started" } } }
                    tbody {
                        for row in &view.rows {
                            tr { key: "{row.brand}-{row.platform}",
                                td { "{row.brand}" } td { "{row.platform}" }
                                td { "{row.contacts}" } td { "{row.intakes}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        lead_sources_body, platform_label, rows_from_response, stats_query, LeadSourceRow,
        LeadSourcesView,
    };

    #[test]
    fn tagged_and_referred_sources_resolve_to_one_platform() {
        for (source, want) in [
            ("linkedin", Some("LinkedIn")),
            ("LinkedIn", Some("LinkedIn")),
            ("t.co", Some("X")),
            ("Twitter", Some("X")),
            ("fb", Some("Facebook")),
            ("Google", None),
            ("Direct / None", None),
        ] {
            assert_eq!(platform_label(source), want, "{source}");
        }
    }

    #[test]
    fn the_query_asks_for_both_goals_by_source() {
        let query = stats_query("neonlaw.com");
        assert_eq!(query["site_id"], "neonlaw.com");
        assert_eq!(query["dimensions"][0], "visit:source");
        assert_eq!(
            query["filters"][0][2],
            serde_json::json!(["Contact: Submitted", "Intake: Started"])
        );
    }

    #[test]
    fn a_response_folds_into_per_platform_rows_without_other_sources() {
        let body = serde_json::json!({ "results": [
            { "dimensions": ["linkedin", "Contact: Submitted"], "metrics": [3] },
            { "dimensions": ["LinkedIn", "Contact: Submitted"], "metrics": [2] },
            { "dimensions": ["linkedin", "Intake: Started"], "metrics": [1] },
            { "dimensions": ["fb", "Intake: Started"], "metrics": [4] },
            { "dimensions": ["Google", "Contact: Submitted"], "metrics": [9] },
        ]});
        assert_eq!(
            rows_from_response("neon", &body),
            vec![
                LeadSourceRow {
                    brand: "neon".into(),
                    platform: "Facebook".into(),
                    contacts: 0,
                    intakes: 4
                },
                LeadSourceRow {
                    brand: "neon".into(),
                    platform: "LinkedIn".into(),
                    contacts: 5,
                    intakes: 1
                },
            ]
        );
        assert!(rows_from_response("neon", &serde_json::json!({})).is_empty());
    }

    fn render(view: &LeadSourcesView) -> String {
        dioxus_ssr::render_element(lead_sources_body(view))
    }

    #[test]
    fn the_section_explains_setup_empty_and_counted_states() {
        assert!(render(&LeadSourcesView::default()).contains("PLAUSIBLE_STATS_API_KEY"));
        assert!(render(&LeadSourcesView {
            configured: true,
            rows: vec![]
        })
        .contains("No leads from social platforms"));
        let html = render(&LeadSourcesView {
            configured: true,
            rows: vec![LeadSourceRow {
                brand: "neon".into(),
                platform: "LinkedIn".into(),
                contacts: 5,
                intakes: 1,
            }],
        });
        assert!(html.contains("<td>LinkedIn</td>"), "{html}");
        assert!(html.contains("<td>5</td>"), "{html}");
    }
}
