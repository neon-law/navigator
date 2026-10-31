//! Firm inbox and Project inbox.
//!
//! Firm participants triage conversations after intake, then communicate
//! through the shared message transcript with project-scoped authorization.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Choice, Field, FormCard, Heading};
use crate::people::ViewerRole;

/// Owner firm-wide list.
pub const OWNER_INBOX_PATH: &str = "/app/owner/inbox";
/// One Owner triage conversation.
pub const OWNER_INBOX_THREAD_PATH: &str = "/app/owner/inbox/{conversation_id}";
/// Firm inbox for firm-side participants.
pub const INBOX_PATH: &str = "/app/inbox";
/// One Firm inbox conversation.
pub const INBOX_THREAD_PATH: &str = "/app/inbox/{conversation_id}";
/// Conversations assigned to one Project.
pub const PROJECT_INBOX_PATH: &str = "/app/projects/{project_code}/inbox";
/// One assigned conversation.
pub const PROJECT_INBOX_THREAD_PATH: &str = "/app/projects/{project_code}/inbox/{conversation_id}";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxRowView {
    pub id: String,
    pub href: String,
    pub channel: String,
    #[serde(default)]
    pub brand_key: Option<String>,
    pub assignee: String,
    pub unread: bool,
    pub delivery: String,
    pub next_action: String,
    pub subject: String,
    /// Who the conversation is with: the sender's name, else their address.
    #[serde(default)]
    pub sender: String,
    /// Last activity, short and human ("Oct 11, 04:06 UTC").
    #[serde(default)]
    pub updated: String,
    /// Last activity as RFC 3339, for the `<time>` element.
    #[serde(default)]
    pub updated_iso: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxMessageView {
    pub id: String,
    pub direction: String,
    pub body: String,
    pub delivery: String,
    pub internal: bool,
    #[serde(default)]
    pub occurred_at: String,
    #[serde(default)]
    pub provider_message_id: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectOption {
    pub id: String,
    pub label: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxPage {
    pub role: ViewerRole,
    pub title: String,
    pub rows: Vec<InboxRowView>,
    pub base_href: String,
    pub channel_filter: Option<String>,
    #[serde(default)]
    pub unread_only: bool,
    #[serde(default)]
    pub brand_filter: Option<String>,
    #[serde(default)]
    pub brands: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    pub error: Option<String>,
    /// Where the "New text" composer posts; absent unless this viewer may text a
    /// new number and the deployment has an SMS number.
    #[serde(default)]
    pub compose_action: Option<String>,
    #[serde(default)]
    pub csrf_token: String,
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    #[serde(default)]
    pub tokens_href: String,
    #[serde(default)]
    pub firm_name: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxThreadPage {
    pub role: ViewerRole,
    pub title: String,
    #[serde(default)]
    pub brand_key: Option<String>,
    #[serde(default)]
    pub provider_account: String,
    #[serde(default)]
    pub source_page: Option<String>,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub interaction_type: String,
    #[serde(default)]
    pub provider_post_id: Option<String>,
    #[serde(default)]
    pub provider_thread_ref: Option<String>,
    pub channel: String,
    pub assignee: String,
    pub unread: bool,
    pub delivery: String,
    pub next_action: String,
    pub bodies_visible: bool,
    pub messages: Vec<InboxMessageView>,
    pub projects: Vec<ProjectOption>,
    pub assign_action: Option<String>,
    pub can_attribute: bool,
    pub note_action: Option<String>,
    pub reply_action: Option<String>,
    /// Where an Owner or Admin posts the email of the Person who owns this
    /// text's number; absent once a Person is linked or for other channels.
    #[serde(default)]
    pub link_phone_action: Option<String>,
    pub csrf_token: String,
    pub error: Option<String>,
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    #[serde(default)]
    pub tokens_href: String,
    #[serde(default)]
    pub firm_name: String,
}

#[cfg(feature = "server")]
async fn chrome() -> (Option<crate::components::AppLogo>, String, String) {
    (
        crate::app_chrome::app_logo_from_context().await,
        crate::app_chrome::app_tokens_href_from_context().await,
        crate::app_chrome::firm_name_from_context().await,
    )
}

#[cfg(feature = "server")]
async fn person_id() -> Option<uuid::Uuid> {
    dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::portal_project_list::PersonId>,
        _,
    >()
    .await
    .ok()
    .and_then(|axum::Extension(pid)| pid.0)
    .and_then(|raw| raw.parse().ok())
}

#[cfg(feature = "server")]
async fn csrf_token() -> String {
    dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<crate::csrf::CsrfToken>, _>()
        .await
        .map(|axum::Extension(token)| token.0)
        .unwrap_or_default()
}

#[cfg(feature = "server")]
async fn query_error() -> Option<String> {
    #[derive(serde::Deserialize, Default)]
    struct Flash {
        error: Option<String>,
    }
    dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<Flash>, _>()
        .await
        .ok()
        .and_then(|axum::extract::Query(flash)| flash.error)
}

#[cfg(feature = "server")]
async fn selected_channel() -> Option<String> {
    #[derive(serde::Deserialize, Default)]
    struct InboxFilter {
        channel: Option<String>,
    }
    let filter =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<InboxFilter>, _>()
            .await
            .ok()
            .map(|axum::extract::Query(filter)| filter)?;
    filter.channel.filter(|channel| {
        matches!(
            channel.as_str(),
            store::inbox::channel::EMAIL
                | store::inbox::channel::SMS
                | store::inbox::channel::VOICE
                | store::inbox::channel::FACEBOOK
                | store::inbox::channel::INSTAGRAM
                | store::inbox::channel::THREADS
                | store::inbox::channel::YOUTUBE
                | store::inbox::channel::TIKTOK
        )
    })
}

#[cfg(feature = "server")]
async fn selected_brand() -> Option<String> {
    #[derive(serde::Deserialize, Default)]
    struct InboxFilter {
        brand: Option<String>,
    }
    dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<InboxFilter>, _>()
        .await
        .ok()
        .and_then(|axum::extract::Query(filter)| filter.brand)
        .filter(|brand| !brand.trim().is_empty())
}

#[cfg(feature = "server")]
async fn unread_only() -> bool {
    #[derive(serde::Deserialize, Default)]
    struct Filter {
        unread: Option<bool>,
    }
    dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<Filter>, _>()
        .await
        .ok()
        .and_then(|axum::extract::Query(filter)| filter.unread)
        .unwrap_or(false)
}

fn filter_href(base: &str, channel: Option<&str>, brand: Option<&str>, unread: bool) -> String {
    let mut parameters = Vec::new();
    for (key, value) in [("channel", channel), ("brand", brand)] {
        if let Some(value) = value {
            let encoded =
                percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC);
            parameters.push(format!("{key}={encoded}"));
        }
    }
    if unread {
        parameters.push("unread=true".into());
    }
    if parameters.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", parameters.join("&"))
    }
}

#[cfg(any(feature = "server", test))]
fn channel_matches(row_channel: &str, selected: Option<&str>) -> bool {
    selected.is_none_or(|channel| channel == row_channel)
}

#[cfg(any(feature = "server", test))]
fn brand_matches(row_brand: Option<&str>, selected: Option<&str>) -> bool {
    selected.is_none_or(|brand| row_brand == Some(brand))
}

#[cfg(feature = "server")]
fn inbox_brands(rows: &[store::inbox::IntakeRow]) -> Vec<String> {
    let mut brands = rows
        .iter()
        .filter_map(|row| row.brand_key.clone())
        .collect::<Vec<_>>();
    brands.sort();
    brands.dedup();
    brands
}

#[cfg(feature = "server")]
fn inbox_channels() -> Vec<String> {
    vec![
        store::inbox::channel::EMAIL.into(),
        store::inbox::channel::SMS.into(),
        store::inbox::channel::FACEBOOK.into(),
        store::inbox::channel::INSTAGRAM.into(),
        store::inbox::channel::THREADS.into(),
        store::inbox::channel::YOUTUBE.into(),
        store::inbox::channel::TIKTOK.into(),
    ]
}

#[cfg(feature = "server")]
fn row_view(row: &store::inbox::IntakeRow, href: String) -> InboxRowView {
    InboxRowView {
        id: row.id.to_string(),
        href,
        channel: row.channel.clone(),
        brand_key: row.brand_key.clone(),
        assignee: row.assignee.clone(),
        unread: row.unread,
        delivery: row.delivery.clone(),
        next_action: row.next_action.clone(),
        subject: row
            .subject
            .clone()
            .filter(|subject| !subject.trim().is_empty())
            .unwrap_or_else(|| "(no subject)".into()),
        sender: row.contact.as_ref().map_or_else(
            || {
                row.external_name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| row.external_address.clone())
            },
            contact_label,
        ),
        updated: row.updated_at.format("%b %-d, %H:%M UTC").to_string(),
        updated_iso: row.updated_at.to_rfc3339(),
    }
}

/// `Pat Client (client)`: whose number this is and what they are to the firm.
#[cfg(feature = "server")]
fn contact_label(contact: &store::inbox::ContactLink) -> String {
    format!("{} ({})", contact.name, contact.role)
}

#[cfg(feature = "server")]
fn message_view(message: &store::inbox::Message) -> InboxMessageView {
    InboxMessageView {
        id: message.id.to_string(),
        direction: message.direction.clone(),
        body: message.body.clone(),
        delivery: message.delivery_state.clone(),
        internal: message.direction == store::inbox::direction::INTERNAL,
        visibility: message.visibility.clone(),
        occurred_at: message.occurred_at.clone(),
        provider_message_id: message
            .provider_message_id
            .clone()
            .or_else(|| message.provider_ref.clone()),
    }
}

/// Starting a text needs an SMS number on the deployment and a viewer whose
/// role may send one; the server re-checks authority on submit.
#[cfg(feature = "server")]
fn compose_action(role: ViewerRole) -> Option<String> {
    let has_number =
        std::env::var("NAVIGATOR_SMS_NUMBER").is_ok_and(|number| !number.trim().is_empty());
    (has_number && matches!(role, ViewerRole::Owner | ViewerRole::Admin))
        .then(|| format!("{INBOX_PATH}/texts"))
}

/// Owner list.
#[server]
pub async fn owner_inbox_page() -> Result<InboxPage, ServerFnError> {
    let role = crate::admin_listing::require_firm_person().await?;
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let actor = person_id()
        .await
        .ok_or_else(|| ServerFnError::new("owner access required"))?;
    let channel_filter = selected_channel().await;
    let unread_only = unread_only().await;
    let brand_filter = selected_brand().await;
    let all_rows = store::inbox::list_intake(&surreal, actor)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let brands = inbox_brands(&all_rows);
    let brand_filter = brand_filter.filter(|brand| brands.contains(brand));
    let rows = all_rows
        .into_iter()
        .filter(|row| {
            channel_matches(&row.channel, channel_filter.as_deref())
                && brand_matches(row.brand_key.as_deref(), brand_filter.as_deref())
                && (!unread_only || row.unread)
        })
        .collect::<Vec<_>>();
    let (logo, tokens_href, firm_name) = chrome().await;
    Ok(InboxPage {
        role,
        title: "Inbox".into(),
        base_href: INBOX_PATH.into(),
        channel_filter,
        unread_only,
        brand_filter,
        brands,
        channels: inbox_channels(),
        rows: rows
            .iter()
            .map(|row| row_view(row, format!("/app/inbox/{}", row.id)))
            .collect(),
        error: query_error().await,
        compose_action: compose_action(role),
        csrf_token: csrf_token().await,
        logo,
        tokens_href,
        firm_name,
    })
}

/// Owner conversation, with an assign control.
#[server]
pub async fn owner_inbox_thread() -> Result<InboxThreadPage, ServerFnError> {
    let role = crate::admin_listing::require_firm_person().await?;
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let actor = person_id()
        .await
        .ok_or_else(|| ServerFnError::new("owner access required"))?;
    let axum::extract::Path(conversation_id) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<uuid::Uuid>, _>()
            .await
            .map_err(|_| ServerFnError::new("missing conversation"))?;
    let detail = store::inbox::intake_detail(&surreal, actor, conversation_id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let (logo, tokens_href, firm_name) = chrome().await;
    Ok(InboxThreadPage {
        role,
        title: detail
            .conversation
            .subject
            .clone()
            .unwrap_or_else(|| detail.conversation.external_address.clone()),
        brand_key: detail.conversation.brand_key.clone(),
        provider_account: detail.conversation.provider_account.clone(),
        source_page: detail.conversation.source_page.clone(),
        sender: detail.contact.as_ref().map_or_else(
            || {
                detail
                    .conversation
                    .external_name
                    .clone()
                    .unwrap_or_else(|| detail.conversation.external_address.clone())
            },
            contact_label,
        ),
        interaction_type: detail.conversation.interaction_type.clone(),
        provider_post_id: detail.conversation.provider_post_id.clone(),
        provider_thread_ref: detail.conversation.provider_thread_ref.clone(),
        channel: detail.conversation.channel.clone(),
        assignee: match detail.conversation.project_id {
            Some(project_id) => detail
                .projects
                .iter()
                .find(|project| project.id == project_id)
                .map_or_else(|| "Assigned".into(), |project| project.name.clone()),
            None => "Unassigned".into(),
        },
        unread: detail.conversation.unread,
        delivery: detail
            .messages
            .iter()
            .rev()
            .find(|message| message.direction == store::inbox::direction::OUTBOUND)
            .map_or_else(
                || store::inbox::delivery::RECEIVED.into(),
                |message| message.delivery_state.clone(),
            ),
        next_action: detail.next_action,
        bodies_visible: detail.bodies_visible,
        messages: detail.messages.iter().map(message_view).collect(),
        projects: detail
            .projects
            .iter()
            .map(|project| ProjectOption {
                id: project.id.to_string(),
                label: format!("{} ({})", project.name, project.code),
            })
            .collect(),
        assign_action: Some(format!("/app/inbox/{conversation_id}/assign")),
        can_attribute: matches!(role, ViewerRole::Lawyer | ViewerRole::Clerk),
        note_action: None,
        reply_action: (detail.can_social_reply || detail.can_sms_reply)
            .then(|| format!("/app/inbox/{conversation_id}/replies")),
        link_phone_action: (detail.can_sms_reply && detail.contact.is_none())
            .then(|| format!("/app/inbox/{conversation_id}/contact")),
        csrf_token: csrf_token().await,
        error: query_error().await,
        logo,
        tokens_href,
        firm_name,
    })
}

#[cfg(feature = "server")]
async fn project_for_code(code: &str) -> Result<store::projects::Project, ServerFnError> {
    let surreal = consume_context::<store::surreal::SurrealDb>();
    store::projects::find_by_code(&surreal, code)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
        .ok_or_else(|| ServerFnError::new("project was not found"))
}

/// Project inbox list for a participant who is not a Client.
#[server]
pub async fn project_inbox_page() -> Result<InboxPage, ServerFnError> {
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let actor = person_id()
        .await
        .ok_or_else(|| ServerFnError::new("not allowed"))?;
    let axum::extract::Path(project_code) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<String>, _>()
            .await
            .map_err(|_| ServerFnError::new("missing project"))?;
    let project = project_for_code(&project_code).await?;
    let channel_filter = selected_channel().await;
    let unread_only = unread_only().await;
    let brand_filter = selected_brand().await;
    let all_rows = store::inbox::list_project(&surreal, actor, project.id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let brands = inbox_brands(&all_rows);
    let brand_filter = brand_filter.filter(|brand| brands.contains(brand));
    let rows = all_rows
        .into_iter()
        .filter(|row| {
            channel_matches(&row.channel, channel_filter.as_deref())
                && brand_matches(row.brand_key.as_deref(), brand_filter.as_deref())
                && (!unread_only || row.unread)
        })
        .collect::<Vec<_>>();
    let (logo, tokens_href, firm_name) = chrome().await;
    Ok(InboxPage {
        role,
        title: format!("{} inbox", project.name),
        base_href: format!("/app/projects/{project_code}/inbox"),
        channel_filter,
        unread_only,
        brand_filter,
        brands,
        channels: inbox_channels(),
        rows: rows
            .iter()
            .map(|row| {
                row_view(
                    row,
                    format!("/app/projects/{project_code}/inbox/{}", row.id),
                )
            })
            .collect(),
        error: query_error().await,
        compose_action: None,
        csrf_token: String::new(),
        logo,
        tokens_href,
        firm_name,
    })
}

/// One Project conversation.
#[server]
pub async fn project_inbox_thread() -> Result<InboxThreadPage, ServerFnError> {
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let actor = person_id()
        .await
        .ok_or_else(|| ServerFnError::new("not allowed"))?;
    let axum::extract::Path((project_code, conversation_id)) =
        dioxus_fullstack_core::FullstackContext::extract::<
            axum::extract::Path<(String, uuid::Uuid)>,
            _,
        >()
        .await
        .map_err(|_| ServerFnError::new("missing conversation"))?;
    let project = project_for_code(&project_code).await?;
    let thread = store::inbox::project_thread(&surreal, actor, project.id, conversation_id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let (logo, tokens_href, firm_name) = chrome().await;
    Ok(InboxThreadPage {
        role,
        title: thread
            .conversation
            .subject
            .clone()
            .unwrap_or_else(|| thread.conversation.external_address.clone()),
        brand_key: thread.conversation.brand_key.clone(),
        provider_account: thread.conversation.provider_account.clone(),
        source_page: thread.conversation.source_page.clone(),
        sender: thread
            .conversation
            .external_name
            .clone()
            .unwrap_or_else(|| thread.conversation.external_address.clone()),
        interaction_type: thread.conversation.interaction_type.clone(),
        provider_post_id: thread.conversation.provider_post_id.clone(),
        provider_thread_ref: thread.conversation.provider_thread_ref.clone(),
        channel: thread.conversation.channel,
        assignee: thread.assignee,
        unread: thread.conversation.unread,
        delivery: thread.delivery,
        next_action: thread.next_action,
        bodies_visible: true,
        messages: thread.messages.iter().map(message_view).collect(),
        projects: Vec::new(),
        assign_action: thread
            .can_attribute
            .then(|| format!("/app/inbox/{conversation_id}/assign")),
        can_attribute: thread.can_attribute,
        note_action: thread
            .can_note
            .then(|| format!("/app/projects/{project_code}/inbox/{conversation_id}/notes")),
        reply_action: thread
            .can_send
            .then(|| format!("/app/projects/{project_code}/inbox/{conversation_id}/replies")),
        link_phone_action: None,
        csrf_token: csrf_token().await,
        error: query_error().await,
        logo,
        tokens_href,
        firm_name,
    })
}

#[component]
pub fn OwnerInbox() -> Element {
    let resource = use_server_future(owner_inbox_page)?;
    let page = match &*resource.read() {
        Some(Ok(page)) => page.clone(),
        Some(Err(_)) => return rsx! { main { id: "inbox", p { "Failed to load the inbox." } } },
        None => return rsx! { main { id: "inbox", p { "Loading…" } } },
    };
    inbox_list_body(&page)
}

#[component]
pub fn OwnerInboxThread() -> Element {
    let resource = use_server_future(owner_inbox_thread)?;
    let page = match &*resource.read() {
        Some(Ok(page)) => page.clone(),
        Some(Err(_)) => {
            return rsx! { main { id: "inbox-thread", p { "Failed to load the conversation." } } };
        }
        None => return rsx! { main { id: "inbox-thread", p { "Loading…" } } },
    };
    inbox_thread_body(&page)
}

#[component]
pub fn ProjectInbox() -> Element {
    let resource = use_server_future(project_inbox_page)?;
    let page = match &*resource.read() {
        Some(Ok(page)) => page.clone(),
        Some(Err(_)) => {
            return rsx! { main { id: "inbox", p { "Failed to load the project inbox." } } };
        }
        None => return rsx! { main { id: "inbox", p { "Loading…" } } },
    };
    inbox_list_body(&page)
}

#[component]
pub fn ProjectInboxThread() -> Element {
    let resource = use_server_future(project_inbox_thread)?;
    let page = match &*resource.read() {
        Some(Ok(page)) => page.clone(),
        Some(Err(_)) => {
            return rsx! { main { id: "inbox-thread", p { "Failed to load the conversation." } } };
        }
        None => return rsx! { main { id: "inbox-thread", p { "Loading…" } } },
    };
    inbox_thread_body(&page)
}

#[allow(clippy::too_many_lines)]
pub fn inbox_list_body(page: &InboxPage) -> Element {
    let role = page.role;
    let firm_name = page.firm_name.clone();
    let title = page.title.clone();
    let rows = page.rows.iter().map(|row| {
        let unread = if row.unread { "Unread" } else { "Read" };
        let brand = row.brand_key.as_deref().unwrap_or("unbranded");
        let row_class = if row.unread {
            "inbox-table__row inbox-table__row--unread"
        } else {
            "inbox-table__row"
        };
        rsx! {
            tr {
                key: "{row.id}",
                id: "inbox-row-{row.id}",
                class: "{row_class}",
                td { class: "inbox-table__state",
                    span { class: "inbox-table__dot", aria_hidden: "true" }
                    span { class: "nav-visually-hidden", "{unread}" }
                }
                td { class: "inbox-table__sender", "{row.sender}" }
                td { class: "inbox-table__subject",
                    a { href: "{row.href}", "{row.subject}" }
                }
                td { class: "inbox-table__tag", "{brand}" }
                td { class: "inbox-table__tag", "{row.channel}" }
                td { class: "inbox-table__assignee", "{row.assignee}" }
                td { class: "inbox-table__next",
                    span { "{row.next_action}" }
                    span { class: "inbox-table__delivery", " · {row.delivery}" }
                }
                td { class: "inbox-table__date",
                    time { datetime: "{row.updated_iso}", "{row.updated}" }
                }
            }
        }
    });
    let compose = page.compose_action.clone().map(|action| {
        rsx! {
            FormCard {
                title: "New text".to_string(),
                action,
                submit_label: "Send text".to_string(),
                heading: Heading::H2,
                csrf_token: Some(page.csrf_token.clone()),
                fields: vec![
                    Field::text("Phone number (E.164, e.g. +15555550123)", "to", "").required(),
                    Field::textarea("Message", "body", "", 4).required(),
                    Field::checkbox(
                        "This person agreed to receive texts from the firm",
                        "consent",
                        "yes",
                        false,
                    )
                    .required(),
                ],
            }
        }
    });
    rsx! {
        document::Title { "{firm_name} | {title}" }
        document::Meta { name: "description", content: "Firm and project conversations." }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{page.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(role),
            logo: page.logo.clone(),
        }
        main { id: "inbox", class: "nav-theme",
            header { class: "page-header",
                h1 { "{title}" }
                p { class: "page-subtitle",
                    "Every email, text, and message to the firm, newest first."
                }
                nav { aria_label: "Filter inbox by channel",
                    a { href: filter_href(&page.base_href, None, page.brand_filter.as_deref(), page.unread_only), "All channels" }
                    for channel in page.channels.iter() {
                        " · "
                        a { href: filter_href(&page.base_href, Some(channel), page.brand_filter.as_deref(), page.unread_only), "{channel}" }
                    }
                    if let Some(selected) = page.channel_filter.as_ref() {
                        " · Showing {selected}"
                    }
                }
                nav { aria_label: "Filter inbox by Brand",
                    a { href: filter_href(&page.base_href, page.channel_filter.as_deref(), None, page.unread_only), "All Brands" }
                    for brand in page.brands.iter() {
                        " · " a { href: filter_href(&page.base_href, page.channel_filter.as_deref(), Some(brand), page.unread_only), "{brand}" }
                    }
                    if let Some(selected) = page.brand_filter.as_ref() {
                        " · Showing Brand {selected}"
                    }
                }
            }
            nav { aria_label: "Inbox unread filter",
                a { href: filter_href(&page.base_href, page.channel_filter.as_deref(), page.brand_filter.as_deref(), false), "All messages" }
                " · "
                a { href: filter_href(&page.base_href, page.channel_filter.as_deref(), page.brand_filter.as_deref(), true), "Unread only" }
            }
            if let Some(error) = page.error.clone() {
                p { class: "page-subtitle", "{error}" }
            }
            {compose}
            if page.rows.is_empty() {
                p { class: "page-subtitle", "No conversations yet." }
            } else {
                div { class: "inbox-table__scroll",
                    table { class: "inbox-table", "aria-label": "Conversations",
                        thead {
                            tr {
                                th { scope: "col", class: "inbox-table__state",
                                    span { class: "nav-visually-hidden", "Unread" }
                                }
                                th { scope: "col", "From" }
                                th { scope: "col", "Subject" }
                                th { scope: "col", "Brand" }
                                th { scope: "col", "Channel" }
                                th { scope: "col", "Assignee" }
                                th { scope: "col", "Next · Delivery" }
                                th { scope: "col", "Date" }
                            }
                        }
                        tbody { {rows} }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
pub fn inbox_thread_body(page: &InboxThreadPage) -> Element {
    let role = page.role;
    let firm_name = page.firm_name.clone();
    let title = page.title.clone();
    let unread = if page.unread { "Unread" } else { "Read" };
    let messages = page.messages.iter().map(|message| {
        let label = if message.internal {
            "Internal note"
        } else {
            message.direction.as_str()
        };
        let audience = message.visibility.as_deref().map_or("", |visibility| {
            if visibility == "public" {
                " · Public"
            } else if visibility == "private" {
                " · Private"
            } else {
                ""
            }
        });
        rsx! {
            article {
                key: "{message.id}",
                id: "inbox-message-{message.id}",
                p { "{label}{audience}. Delivery: {message.delivery}." }
                if message.delivery == "unknown" {
                    p { class: "nav-muted", "Delivery has not been confirmed. Check the provider before sending another reply." }
                } else if message.delivery == "failed" {
                    p { class: "nav-muted", "The provider rejected this reply. Check the receiving account and channel configuration." }
                }
                time { datetime: "{message.occurred_at}", "{message.occurred_at}" }
                p { "{message.body}" }
                if let Some(reference) = message.provider_message_id.as_ref() {
                    p { class: "nav-muted", "Provider message: {reference}" }
                }
            }
        }
    });
    let assign = page.assign_action.clone().map(|action| {
        let choices = page
            .projects
            .iter()
            .map(|project| Choice::new(project.id.clone(), project.label.clone()))
            .collect();
        rsx! {
            FormCard {
                title: "Attribute message to a project".to_string(),
                action,
                submit_label: "Attribute".to_string(),
                heading: Heading::H2,
                csrf_token: Some(page.csrf_token.clone()),
                fields: vec![Field::select("Project", "project_id", choices, None).required()],
            }
        }
    });
    let note = page.note_action.clone().map(|action| {
        rsx! {
            FormCard {
                title: "Internal note".to_string(),
                action,
                submit_label: "Save note".to_string(),
                heading: Heading::H2,
                csrf_token: Some(page.csrf_token.clone()),
                fields: vec![Field::textarea("Note", "body", "", 4).required()],
            }
        }
    });
    let private_social = page.interaction_type == "direct_message"
        || page.messages.iter().any(|message| {
            message.direction == "inbound" && message.visibility.as_deref() == Some("private")
        });
    let unsupported_private =
        private_social && matches!(page.channel.as_str(), "threads" | "youtube" | "tiktok");
    let reply = page
        .reply_action
        .clone()
        .filter(|_| !unsupported_private)
        .map(|action| {
            let mut fields = vec![Field::textarea("Reply", "body", "", 4).required()];
            if matches!(
                page.channel.as_str(),
                "facebook" | "instagram" | "threads" | "youtube" | "tiktok"
            ) {
                let private = private_social;
                let mut audiences = Vec::new();
                if !private {
                    audiences.push(Choice::new("public", "Public reply"));
                }
                if matches!(page.channel.as_str(), "facebook" | "instagram") {
                    audiences.push(Choice::new("private", "Private reply"));
                }
                fields.push(
                    Field::select(
                        "Reply audience",
                        "visibility",
                        audiences,
                        Some(if private { "private" } else { "public" }.into()),
                    )
                    .required(),
                );
            }
            rsx! {
                FormCard {
                    title: "Send a reply".to_string(),
                    action,
                    submit_label: "Send reply".to_string(),
                    heading: Heading::H2,
                    csrf_token: Some(page.csrf_token.clone()),
                    fields,
                }
            }
        });
    let link_phone = page.link_phone_action.clone().map(|action| {
        rsx! {
            FormCard {
                title: "Whose number is this?".to_string(),
                action,
                submit_label: "Link number".to_string(),
                heading: Heading::H2,
                csrf_token: Some(page.csrf_token.clone()),
                fields: vec![Field::email("Their email on file", "email", "").required()],
            }
        }
    });
    rsx! {
        document::Title { "{firm_name} | {title}" }
        document::Meta { name: "description", content: "One inbox conversation." }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{page.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(role),
            logo: page.logo.clone(),
        }
        main { id: "inbox-thread", class: "nav-theme",
            a { href: INBOX_PATH, "Back to Inbox" }
            header { class: "page-header",
                h1 { "{title}" }
                p { class: "page-subtitle",
                    "Channel: {page.channel}. Assignee: {page.assignee}. {unread}. Delivery: {page.delivery}."
                }
                p { class: "page-subtitle", "Brand: " {page.brand_key.as_deref().unwrap_or("unbranded")} }
                p { class: "page-subtitle", "From: {page.sender}. Source account: {page.provider_account}. Interaction: {page.interaction_type}." }
                if let Some(source) = page.source_page.as_ref() {
                    p { class: "nav-muted", "Received via: {source}" }
                }
                if let Some(reference) = page.provider_post_id.as_ref() { p { class: "nav-muted", "Provider post: {reference}" } }
                if let Some(reference) = page.provider_thread_ref.as_ref() { p { class: "nav-muted", "Provider thread: {reference}" } }
                p { class: "page-subtitle", "Next: {page.next_action}" }
                if unsupported_private {
                    p { "This channel supports public comment replies only; private replies are unavailable." }
                }
                if page.reply_action.is_none() {
                    p { class: "nav-muted", "Replies require an assigned Project and active Lawyer or Clerk participation. Owner and the Firm Admin DRI may reply to texts and supported social interactions." }
                }
            }
            if let Some(error) = page.error.clone() {
                p { class: "page-subtitle", "{error}" }
            }
            if page.bodies_visible {
                div { id: "inbox-messages", {messages} }
            } else {
                p { id: "inbox-bodies-hidden",
                    "Message bodies stay on the project. Assign or reassign from here."
                }
            }
            {assign}
            {note}
            {reply}
            {link_phone}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_filter_links_keep_the_selected_brand() {
        let mut view = page(ViewerRole::Lawyer, Vec::new());
        view.brand_filter = Some("vesta".into());
        view.brands = vec!["vesta".into()];
        let html = dioxus_ssr::render_element(inbox_list_body(&view));
        assert!(html.contains("channel=sms&#38;brand=vesta"), "{html}");
    }

    #[test]
    fn unread_links_keep_channel_and_brand_and_thread_shows_the_source() {
        let mut view = page(ViewerRole::Lawyer, Vec::new());
        view.brand_filter = Some("neon".into());
        view.channel_filter = Some("email".into());
        let html = dioxus_ssr::render_element(inbox_list_body(&view));
        assert!(
            html.contains("channel=email&#38;brand=neon&#38;unread=true"),
            "{html}"
        );
        let mut view = base_thread();
        view.brand_key = Some("neon".into());
        view.provider_account = "receiving-account".into();
        view.sender = "sender@example.com".into();
        let html = thread(&view);
        for context in [
            "receiving-account",
            "sender@example.com",
            "source-message-1",
            "2026-10",
        ] {
            assert!(html.contains(context), "missing {context}: {html}");
        }
    }

    #[test]
    fn unsupported_private_replies_have_no_empty_audience_composer() {
        let mut view = base_thread();
        view.channel = "youtube".into();
        view.interaction_type = "direct_message".into();
        view.reply_action = Some("/app/inbox/c1/replies".into());
        let html = thread(&view);
        assert!(!html.contains("Send a reply"));
        assert!(html.contains("private replies are unavailable"));
    }

    #[test]
    fn private_message_composer_never_offers_a_public_reply() {
        let mut view = base_thread();
        view.channel = "instagram".into();
        view.messages[0].visibility = Some("private".into());
        view.reply_action = Some("/app/inbox/c1/replies".into());
        let html = thread(&view);
        assert!(html.contains("Private reply"), "{html}");
        assert!(!html.contains("Public reply"), "{html}");
    }

    #[test]
    fn channel_filter_keeps_only_matching_conversations() {
        assert!(channel_matches("instagram", Some("instagram")));
        assert!(!channel_matches("email", Some("instagram")));
        assert!(channel_matches("email", None));
        assert!(brand_matches(Some("neon"), Some("neon")));
        assert!(!brand_matches(Some("other"), Some("neon")));
        assert!(!brand_matches(None, Some("neon")));
        assert!(brand_matches(None, None));
    }

    fn page(role: ViewerRole, rows: Vec<InboxRowView>) -> InboxPage {
        let mut brands = rows
            .iter()
            .filter_map(|row| row.brand_key.clone())
            .collect::<Vec<_>>();
        brands.sort();
        brands.dedup();
        InboxPage {
            role,
            title: "Inbox".into(),
            rows,
            base_href: "/app/inbox".into(),
            channel_filter: None,
            unread_only: false,
            brand_filter: None,
            brands,
            compose_action: None,
            csrf_token: String::new(),
            channels: [
                "email",
                "sms",
                "facebook",
                "instagram",
                "threads",
                "youtube",
                "tiktok",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            error: None,
            logo: None,
            tokens_href: String::new(),
            firm_name: "Practice".into(),
        }
    }

    fn thread(page: &InboxThreadPage) -> String {
        dioxus_ssr::render_element(inbox_thread_body(page))
    }

    fn base_thread() -> InboxThreadPage {
        InboxThreadPage {
            role: ViewerRole::Owner,
            title: "Question".into(),
            brand_key: Some("neon".into()),
            provider_account: "support@example.com".into(),
            source_page: Some("support@example.com".into()),
            sender: "sender@example.com".into(),
            interaction_type: "message".into(),
            provider_post_id: None,
            provider_thread_ref: None,
            channel: "email".into(),
            assignee: "Unassigned".into(),
            unread: true,
            delivery: "received".into(),
            next_action: "Assign to a project".into(),
            bodies_visible: true,
            messages: vec![InboxMessageView {
                id: "m1".into(),
                direction: "inbound".into(),
                body: "Hello from outside.".into(),
                delivery: "received".into(),
                internal: false,
                occurred_at: "2026-10-10T12:00:00Z".into(),
                provider_message_id: Some("source-message-1".into()),
                visibility: None,
            }],
            projects: vec![ProjectOption {
                id: "project-1".into(),
                label: "Alpha (alpha)".into(),
            }],
            assign_action: Some("/app/owner/inbox/c1/assign".into()),
            can_attribute: true,
            note_action: None,
            reply_action: None,
            link_phone_action: None,
            csrf_token: "token".into(),
            error: None,
            logo: None,
            tokens_href: String::new(),
            firm_name: "Practice".into(),
        }
    }

    #[test]
    fn list_offers_a_new_text_composer_only_when_the_server_allows_it() {
        let plain = dioxus_ssr::render_element(inbox_list_body(&page(ViewerRole::Owner, vec![])));
        assert!(!plain.contains("New text"), "{plain}");

        let mut composing = page(ViewerRole::Owner, vec![]);
        composing.compose_action = Some("/app/inbox/texts".into());
        composing.csrf_token = "token".into();
        let html = dioxus_ssr::render_element(inbox_list_body(&composing));
        assert!(html.contains("New text"), "{html}");
        assert!(html.contains("action=\"/app/inbox/texts\""), "{html}");
        for name in ["name=\"to\"", "name=\"body\"", "name=\"consent\""] {
            assert!(html.contains(name), "{name}: {html}");
        }
    }

    #[test]
    fn a_text_thread_offers_to_link_the_number_until_a_person_is_linked() {
        let mut page = base_thread();
        page.channel = "sms".into();
        page.link_phone_action = Some("/app/inbox/c1/contact".into());
        let html = thread(&page);
        assert!(html.contains("Whose number is this?"), "{html}");
        assert!(html.contains("/app/inbox/c1/contact"), "{html}");
        assert!(html.contains("name=\"email\""), "{html}");

        page.link_phone_action = None;
        assert!(!thread(&page).contains("Whose number is this?"));
    }

    #[test]
    fn a_linked_text_sender_shows_the_person_and_role() {
        let row = store::inbox::IntakeRow {
            contact: Some(store::inbox::ContactLink {
                person_id: uuid::Uuid::nil(),
                name: "Pat Client".into(),
                role: "client".into(),
            }),
            id: uuid::Uuid::nil(),
            firm_id: uuid::Uuid::nil(),
            brand_key: None,
            project_id: None,
            channel: "sms".into(),
            assignee: "Unassigned".into(),
            unread: true,
            delivery: "received".into(),
            next_action: "Assign".into(),
            external_address: "+15555550123".into(),
            external_name: None,
            subject: None,
            updated_at: chrono::Utc::now(),
        };
        assert_eq!(row_view(&row, "/x".into()).sender, "Pat Client (client)");
    }

    #[test]
    fn a_text_thread_reply_has_no_social_audience_picker() {
        let mut page = base_thread();
        page.channel = "sms".into();
        page.reply_action = Some("/app/inbox/c1/replies".into());
        let html = thread(&page);
        assert!(html.contains("/app/inbox/c1/replies"), "{html}");
        assert!(!html.contains("name=\"visibility\""), "{html}");
    }

    #[test]
    fn owner_list_shows_channel_assignee_unread_delivery_and_next_action() {
        let html = dioxus_ssr::render_element(inbox_list_body(&page(
            ViewerRole::Owner,
            vec![InboxRowView {
                id: "c1".into(),
                href: "/app/owner/inbox/c1".into(),
                channel: "sms".into(),
                brand_key: Some("neon".into()),
                assignee: "Unassigned".into(),
                unread: true,
                delivery: "received".into(),
                next_action: "Assign to a project".into(),
                subject: "Question".into(),
                sender: "Pat Client".into(),
                updated: "Oct 11, 04:06 UTC".into(),
                updated_iso: "2026-10-11T04:06:15+00:00".into(),
            }],
        )));
        assert!(html.contains("<table"), "{html}");
        assert!(html.contains("inbox-table__row--unread"), "{html}");
        for cell in [
            "Pat Client",
            "Question",
            "sms",
            "neon",
            "Unassigned",
            "Assign to a project",
            "received",
            "Oct 11, 04:06 UTC",
            "Unread",
        ] {
            assert!(html.contains(cell), "{cell}: {html}");
        }
        assert!(html.contains(r#"href="/app/owner/inbox/c1""#), "{html}");
        assert!(
            html.contains(r#"href="/app/inbox?channel=email""#),
            "{html}"
        );
        assert!(html.contains(r#"href="/app/inbox?channel=sms""#), "{html}");
        assert!(html.contains(r#"href="/app/inbox?brand=neon""#), "{html}");
    }

    #[test]
    fn owner_thread_assigns_and_hides_bodies_after_assignment_without_participation() {
        let mut hidden = base_thread();
        hidden.bodies_visible = false;
        hidden.messages.clear();
        hidden.next_action = "Reassign".into();
        hidden.assignee = "Alpha".into();
        let html = thread(&hidden);
        assert!(
            html.contains(r#"action="/app/owner/inbox/c1/assign""#),
            "{html}"
        );
        assert!(
            html.contains("Reassign") || html.contains("Assign to a project"),
            "{html}"
        );
        assert!(html.contains("inbox-bodies-hidden"), "{html}");
        assert!(!html.contains("Hello from outside."), "{html}");
        assert!(!html.contains("Send a reply"), "{html}");
    }

    #[test]
    fn lawyer_thread_offers_a_private_note_and_not_a_send() {
        let mut page = base_thread();
        page.role = ViewerRole::Lawyer;
        page.assign_action = None;
        page.note_action = Some("/app/projects/alpha/inbox/c1/notes".into());
        page.reply_action = None;
        page.next_action = "Leave an internal note".into();
        page.messages.push(InboxMessageView {
            id: "m2".into(),
            direction: "internal".into(),
            body: "Private strategy.".into(),
            delivery: "received".into(),
            internal: true,
            occurred_at: "2026-10-10T12:00:00Z".into(),
            provider_message_id: None,
            visibility: None,
        });
        let html = thread(&page);
        assert!(html.contains("Internal note"), "{html}");
        assert!(html.contains("Private strategy."), "{html}");
        assert!(
            html.contains(r#"action="/app/projects/alpha/inbox/c1/notes""#),
            "{html}"
        );
        assert!(!html.contains("Send a reply"), "{html}");
        assert!(!html.contains("/replies"), "{html}");
    }

    #[test]
    fn clerk_thread_can_send_and_shows_unknown_delivery() {
        let mut page = base_thread();
        page.role = ViewerRole::Clerk;
        page.assign_action = None;
        page.note_action = Some("/app/projects/alpha/inbox/c1/notes".into());
        page.reply_action = Some("/app/projects/alpha/inbox/c1/replies".into());
        page.delivery = "unknown".into();
        page.next_action = "Reconcile delivery".into();
        page.messages.push(InboxMessageView {
            id: "m2".into(),
            direction: "outbound".into(),
            body: "We received your message.".into(),
            delivery: "unknown".into(),
            internal: false,
            occurred_at: "2026-10-10T12:00:00Z".into(),
            provider_message_id: None,
            visibility: Some("private".into()),
        });
        let html = thread(&page);
        assert!(html.contains("Reconcile delivery"), "{html}");
        assert!(html.contains("Delivery: unknown"), "{html}");
        assert!(
            html.contains(r#"action="/app/projects/alpha/inbox/c1/replies""#),
            "{html}"
        );
        assert!(html.contains("We received your message."), "{html}");
        assert!(html.contains("outbound · Private"), "{html}");
    }

    #[test]
    fn social_thread_discloses_public_reply_audience() {
        let mut page = base_thread();
        page.channel = "instagram".into();
        page.messages[0].visibility = Some("public".into());
        let html = thread(&page);
        assert!(html.contains("inbound · Public"), "{html}");
    }

    #[test]
    fn social_reply_form_offers_public_and_private_audiences() {
        let mut page = base_thread();
        page.channel = "instagram".into();
        page.reply_action = Some("/app/inbox/c1/replies".into());
        let html = thread(&page);
        assert!(html.contains("Reply audience"), "{html}");
        assert!(html.contains("name=\"visibility\""), "{html}");
        assert!(html.contains("Public reply"), "{html}");
        assert!(html.contains("Private reply"), "{html}");
        assert!(html.contains("/app/inbox/c1/replies"), "{html}");
    }

    #[test]
    fn threads_and_youtube_replies_are_public_only() {
        for channel in ["threads", "youtube", "tiktok", "facebook"] {
            let mut page = base_thread();
            page.channel = channel.into();
            page.reply_action = Some("/app/inbox/c1/replies".into());
            page.messages[0].visibility = Some(
                if channel == "facebook" {
                    "private"
                } else {
                    "public"
                }
                .into(),
            );
            let html = thread(&page);
            assert!(html.contains("Reply audience"), "{html}");
            if channel == "facebook" {
                assert!(!html.contains("Public reply"), "{html}");
                assert!(html.contains("Private reply"), "{html}");
                assert!(html.contains("inbound · Private"), "{html}");
            } else {
                assert!(html.contains("Public reply"), "{html}");
                assert!(!html.contains("Private reply"), "{html}");
            }
        }
    }

    #[test]
    fn channel_and_brand_filters_accept_an_exact_match() {
        assert!(channel_matches("instagram", None));
        assert!(channel_matches("instagram", Some("instagram")));
        assert!(!channel_matches("instagram", Some("facebook")));
        assert!(brand_matches(Some("neon"), None));
        assert!(brand_matches(Some("neon"), Some("neon")));
        assert!(!brand_matches(Some("neon"), Some("vesta")));
        assert!(!brand_matches(None, Some("neon")));
    }

    #[test]
    fn inbox_list_shows_an_empty_state_and_active_filters() {
        let mut page = page(ViewerRole::Owner, Vec::new());
        page.error = Some("Choose a project first.".into());
        page.channel_filter = Some("instagram".into());
        page.brand_filter = Some("neon".into());
        page.brands = vec!["neon".into()];
        let html = dioxus_ssr::render_element(inbox_list_body(&page));
        assert!(html.contains("Choose a project first."), "{html}");
        assert!(html.contains("Showing instagram"), "{html}");
        assert!(html.contains("Showing Brand neon"), "{html}");
        assert!(html.contains("No conversations yet."), "{html}");
    }
}
