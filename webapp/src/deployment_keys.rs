//! Owner credentials editor; browser models contain metadata only.
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::Field;
/// Owner-only social app keys page.
pub const PATH: &str = "/app/owner/keys";
/// Safe configuration metadata for one preset.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyRow {
    pub provider: String,
    pub key: String,
    pub secret: bool,
    pub configured: bool,
    pub identifier: String,
    pub change: ChangeState,
}
/// Operator reconciliation state for an encrypted replacement.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum ChangeState {
    None,
    Pending,
    Expired,
}
/// Metadata rendered by the owner editor, never credential values.
#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct KeysView {
    pub rows: Vec<KeyRow>,
    pub csrf: String,
    pub writable: bool,
    pub notice: String,
}
/// Read metadata from this deployment's environment and encrypted requests.
#[server]
pub async fn keys_view() -> Result<KeysView, ServerFnError> {
    crate::admin_listing::require_owner().await?;
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let csrf = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::csrf::CsrfToken>,
        _,
    >()
    .await
    .map_or_else(|_| String::new(), |token| token.0 .0);
    let production =
        store::deployment_secret_changes::accepts_changes_from(|key| std::env::var(key).ok());
    let rows = if production {
        store::deployment_secret_changes::latest(
            &surreal,
            store::persons::Role::Owner,
            store::DeploymentEnvironment::Production,
        )
        .await
        .map_err(|_| ServerFnError::new("Could not read encrypted change metadata"))?
    } else {
        Vec::new()
    };
    let query = dioxus_fullstack_core::FullstackContext::extract::<
        axum::extract::Query<std::collections::BTreeMap<String, String>>,
        _,
    >()
    .await
    .map_or_else(|_| std::collections::BTreeMap::new(), |query| query.0);
    let notice = match query.get("result").map(String::as_str) {
        Some("saved") => "Encrypted change saved. Export it for the deployment operator; this does not deploy or verify credentials.",
        Some("unchanged") => "Blank fields left existing values unchanged.",
        Some("test") => "Configuration checked. Configured means this process has a nonblank value, not provider approval or Brand authorization.",
        Some("blocked") => "Saving is unavailable. Check production scope and runtime encryption with the deployment operator.",
        Some("invalid") => "Unsupported field or invalid value. No change was saved.",
        _ => "Secrets stay blank. Enter only replacements; blank fields preserve existing values.",
    };
    Ok(KeysView {
        csrf,
        writable: production
            && std::env::var("NAVIGATOR_RUNTIME_KMS_KEY").is_ok_and(|key| !key.trim().is_empty()),
        notice: notice.into(),
        rows: store::deployment_secret_changes::PRESETS
            .iter()
            .map(|preset| KeyRow {
                provider: preset.provider.into(),
                key: preset.key.into(),
                secret: preset.secret,
                identifier: if preset.secret {
                    String::new()
                } else {
                    std::env::var(preset.key).unwrap_or_default()
                },
                configured: std::env::var(preset.key).is_ok_and(|value| !value.trim().is_empty()),
                change: rows.iter().find(|row| row.key == preset.key).map_or(
                    ChangeState::None,
                    |row| {
                        if store::deployment_secret_changes::exportable_at(row, chrono::Utc::now())
                        {
                            ChangeState::Pending
                        } else {
                            ChangeState::Expired
                        }
                    },
                ),
            })
            .collect(),
    })
}
/// Owner route entry.
#[component]
pub fn DeploymentKeys() -> Element {
    let resource = use_server_future(keys_view)?;
    let view = resource.read().clone();
    match view {
        Some(Ok(view)) => keys_body(&view),
        _ => {
            rsx! { main { h1 { "Social app keys" } p { "Unable to load credential configuration." } } }
        }
    }
}
/// One row's control: a secret is a password field that never echoes its saved
/// value; an identifier is plain text, prefilled.
fn key_field(row: &KeyRow, writable: bool) -> Field {
    let field = if row.secret {
        Field::password(row.key.clone(), row.key.clone()).placeholder("Enter replacement secret")
    } else {
        Field::text(row.key.clone(), row.key.clone(), row.identifier.clone())
            .placeholder("Enter app or client identifier")
    }
    .autocomplete("off")
    .maxlength(8192);
    if writable {
        field
    } else {
        field.disabled()
    }
}
/// Render the editor without returning saved values to the browser.
pub fn keys_body(view: &KeysView) -> Element {
    rsx! {
        document::Title { "Navigator | Social app keys" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        main { class: "nav-theme", id: "deployment-keys",
            header { class: "page-header",
                a { href: "/app/owner", "Back to Owner" }
                h1 { "Social app keys" }
                p { "Shared production app credentials. Each Brand connects its own account through OAuth." }
            }
            p { role: "status", "{view.notice}" }
            if !view.writable {
                p { "Saving requires production scope and a dedicated runtime encryption key configured by the deployment operator." }
            }
            form { method: "post", action: PATH, autocomplete: "off",
                input { r#type: "hidden", name: "_csrf", value: "{view.csrf}" }
                for (index, row) in view.rows.iter().enumerate() {
                    section { class: "nav-form-card deployment-key-row", key: "{row.key}",
                        if index == 0 || view.rows[index - 1].provider != row.provider { h2 { "{row.provider}" } }
                        {key_field(row, view.writable).render()}
                        p {
                            if row.configured { "Configured in this deployment" } else { "Not set in this deployment" }
                            if row.change == ChangeState::Pending { " · Saved change awaiting operator reconciliation" }
                            if row.change == ChangeState::Expired { " · Saved change expired; enter a fresh replacement" }
                        }
                    }
                }
                button { class: "nav-btn nav-btn--primary", r#type: "submit", disabled: !view.writable, "Save encrypted changes" }
            }
            form { method: "post", action: "/app/owner/keys/test",
                input { r#type: "hidden", name: "_csrf", value: "{view.csrf}" }
                button { class: "nav-btn", r#type: "submit", "Check configuration" }
            }
            p { a { href: "/app/admin/brands", "Manage Brand social accounts" } }
            if view.rows.iter().any(|row| row.change == ChangeState::Pending) {
                p { a { href: "/app/owner/keys/export", "Download encrypted changes for SOPS" } }
            }
            p { "Only unexpired changes are exported. Imports expire after 24 hours; saved metadata does not prove deployment." }
            p { "Testing checks loaded configuration only. Verify provider permissions and each Brand's account on its Social Accounts page. No test post is sent." }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presets_are_blank_and_pending_is_not_deployed() {
        let html = dioxus_ssr::render_element(keys_body(&KeysView {
            writable: true,
            csrf: "synthetic-csrf".into(),
            notice: "Saved change".into(),
            rows: vec![KeyRow {
                provider: "Meta".into(),
                key: "NAVIGATOR_META_APP_SECRET".into(),
                secret: true,
                identifier: "must-not-render-secret".into(),
                configured: false,
                change: ChangeState::Pending,
            }],
        }));
        assert!(html.contains("type=\"password\""), "{html}");
        assert!(
            !html.contains("value=\"\""),
            "a secret echoes no value: {html}"
        );
        assert!(html.contains("Not set in this deployment"));
        assert!(html.contains("awaiting operator reconciliation"));
        assert!(html.contains("No test post is sent"));
        assert!(!html.contains("must-not-render-secret"));
    }
}
