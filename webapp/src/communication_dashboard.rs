//! Parked call queue and per-Brand communications reporting.
use crate::people::ViewerRole;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

pub const PARKED_CALLS_PATH: &str = "/app/inbox/calls";
pub const BRAND_COMMUNICATIONS_PATH: &str = "/app/admin/communications";

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkedCall {
    pub conversation_id: String,
    pub provider_message_id: String,
    pub started_at: String,
    pub seconds: String,
    pub tenths: String,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandReport {
    pub brand: String,
    pub calls: usize,
    pub sms: usize,
    pub tenths: i64,
    pub parked: usize,
    pub numbers: Vec<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunicationsPage {
    pub role: ViewerRole,
    pub parked: Vec<ParkedCall>,
    pub brands: Vec<String>,
    pub report: Option<BrandReport>,
    pub logo: Option<crate::components::AppLogo>,
    pub tokens_href: String,
}

#[server]
pub async fn communications_page() -> Result<CommunicationsPage, ServerFnError> {
    #[derive(Deserialize, Default)]
    struct Filter {
        brand: Option<String>,
    }
    let role = crate::admin_listing::require_firm_person().await?;
    let db = consume_context::<store::surreal::SurrealDb>();
    let actor = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::portal_project_list::PersonId>,
        _,
    >()
    .await
    .ok()
    .and_then(|axum::Extension(id)| id.0)
    .and_then(|id| id.parse::<uuid::Uuid>().ok())
    .ok_or_else(|| ServerFnError::new("firm access required"))?;
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|_| ServerFnError::new("Call reporting is unavailable."))?;
    let filter =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<Filter>, _>()
            .await
            .map(|axum::extract::Query(filter)| filter)
            .unwrap_or_default();
    let report = if let Some(brand) = filter.brand.filter(|brand| !brand.is_empty()) {
        crate::admin_listing::require_admin().await?;
        let report = store::communication_dashboard::for_brand(&pg, &db, actor, &brand)
            .await
            .map_err(|_| ServerFnError::new("Brand communications are unavailable."))?;
        Some(BrandReport {
            brand: report.brand_key,
            calls: report.calls,
            sms: report.sms,
            tenths: report.billed_tenths,
            parked: report.parked_calls,
            numbers: report.registered_numbers,
        })
    } else {
        None
    };
    let parked = store::billed_calls::parked_calls(&pg, &db, actor)
        .await
        .map_err(|_| ServerFnError::new("Call reporting is unavailable."))?
        .into_iter()
        .map(|call| ParkedCall {
            conversation_id: call.conversation_id.to_string(),
            provider_message_id: call.provider_message_id,
            started_at: call.occurred_at.to_rfc3339(),
            seconds: call
                .duration_seconds
                .map_or_else(|| "Not recorded".into(), |seconds| seconds.to_string()),
            tenths: call
                .tenths
                .map_or_else(|| "Not billed".into(), |tenths| tenths.to_string()),
        })
        .collect();
    let brands = if role.is_admin_tier() {
        store::communication_dashboard::brand_keys_for_actor(&db, actor)
            .await
            .map_err(|_| ServerFnError::new("Brand communications are unavailable."))?
    } else {
        Vec::new()
    };
    Ok(CommunicationsPage {
        role,
        parked,
        brands,
        report,
        logo: crate::app_chrome::app_logo_from_context().await,
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
    })
}

#[component]
pub fn Communications() -> Element {
    let resource = use_server_future(communications_page)?;
    let state = resource.read();
    match &*state {
        Some(Ok(page)) => communications_body(page),
        Some(Err(_)) => rsx! { main { p { "Communications are unavailable." } } },
        None => rsx! { main { p { "Loading…" } } },
    }
}

pub fn communications_body(page: &CommunicationsPage) -> Element {
    rsx! {
        document::Title { "Communications" }
        document::Stylesheet { href:crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href:"{page.tokens_href}" }
        crate::components::AppNavbar {
            destinations:crate::app_chrome::app_destinations(page.role),logo:page.logo.clone(),
        }
        main { class:"nav-theme", id:"communications",
            header { class:"page-header", h1 { "Communications" }
                a { href:"/app/inbox", "Inbox" }
            }
            if page.role.is_admin_tier() {
                form { method:"get", action:BRAND_COMMUNICATIONS_PATH,
                    label { r#for:"communications-brand", "Brand" }
                    select { class: "nav-select", id:"communications-brand", name:"brand", required:true,
                        option { value:"", "Choose a Brand" }
                        for brand in &page.brands {
                            option {
                                value:"{brand}",
                                selected:page.report.as_ref().is_some_and(|report| report.brand == *brand),
                                "{brand}"
                            }
                        }
                    }
                    button { class: "nav-btn nav-btn--primary", r#type:"submit", "View communications" }
                }
            }
            if let Some(report)=&page.report {
                section { aria_label:"Brand communications",
                    h2 { "{report.brand}" }
                    dl {
                        dt { "Calls" } dd { "{report.calls}" }
                        dt { "SMS" } dd { "{report.sms}" }
                        dt { "Billed tenths" } dd { "{report.tenths}" }
                        dt { "Parked calls" } dd { "{report.parked}" }
                    }
                    h3 { "Registered numbers" }
                    if report.numbers.is_empty() { p { "No registered numbers." } }
                    ul { for number in &report.numbers { li { "{number}" } } }
                }
            }
            section { aria_label:"Parked calls",
                h2 { "Parked calls" }
                p { "Open a conversation and link it to a Project. Duration and billed tenths stay unchanged." }
                if page.parked.is_empty() { p { "No parked calls." } }
                table {
                    thead { tr { th { "Call" } th { "Started" } th { "Seconds" } th { "Tenths" } } }
                    tbody { for call in &page.parked {
                        tr { key:"{call.provider_message_id}",
                            td { a { href:"/app/inbox/{call.conversation_id}", "{call.provider_message_id}" } }
                            td { "{call.started_at}" } td { "{call.seconds}" } td { "{call.tenths}" }
                        }
                    } }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reporting_escapes_source_metadata_and_exposes_no_credential_fields() {
        let page = CommunicationsPage {
            role: ViewerRole::Admin,
            parked: vec![ParkedCall {
                conversation_id: "conversation".into(),
                provider_message_id: "<script>alert(1)</script>".into(),
                started_at: "2026-10-30T18:00:00Z".into(),
                seconds: "361".into(),
                tenths: "2".into(),
            }],
            brands: vec!["example".into()],
            report: Some(BrandReport {
                brand: "example".into(),
                calls: 1,
                sms: 2,
                tenths: 2,
                parked: 1,
                numbers: vec!["+15555550123".into()],
            }),
            logo: None,
            tokens_href: String::new(),
        };
        let html = dioxus_ssr::render_element(communications_body(&page));
        assert!(!html.contains("<script"), "{html}");
        assert!(
            html.contains("&lt;script") || html.contains("&#60;script"),
            "{html}"
        );
        assert!(html.contains("alert(1)"));
        assert!(html.contains("Billed tenths"));
        assert!(html.contains("/app/inbox/conversation"));
        assert!(!html.contains("auth_token_secret_ref"));
    }
}
