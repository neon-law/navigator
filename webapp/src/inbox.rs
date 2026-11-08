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
/// The firm reading pane's server-function endpoint, mounted only on the
/// [`INBOX_PATH`] router, under `/app/inbox`'s firm-role admission.
pub const INBOX_PANE_ENDPOINT: &str = "/app/inbox/pane";
/// The Project reading pane's server-function endpoint, mounted only on the
/// [`PROJECT_INBOX_PATH`] router; the store re-checks participation.
pub const PROJECT_INBOX_PANE_ENDPOINT: &str = "/app/projects/inbox-pane";
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
    /// The direction of the conversation's most recent message — what the
    /// `?direction=` filter matches rows on (LAW-238 D3).
    #[serde(default)]
    pub direction: String,
    /// Whether any message on the conversation carries a filed attachment —
    /// what the `?attachments=true` filter matches rows on.
    #[serde(default)]
    pub has_attachments: bool,
    /// Whether a firm-side Person sent a message on the conversation — what
    /// the Sent folder lists.
    #[serde(default)]
    pub sent: bool,
    /// Opening snippet of the conversation's most recent message, whitespace
    /// collapsed and ellipsized — None when it has no messages.
    #[serde(default)]
    pub preview: Option<String>,
    /// The Person the inbound sender resolved to at ingest, when one was
    /// persisted.
    #[serde(default)]
    pub person: Option<InboxPersonView>,
}

/// The persisted Person an inbound conversation's sender resolved to at
/// ingest — display only.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxPersonView {
    pub name: String,
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
    /// `occurred_at` short and human ("Nov 6, 2026, 22:29 UTC"), else the
    /// raw value when it does not parse.
    #[serde(default)]
    pub occurred_label: String,
    /// An outbound message no Person wrote — an automatic acknowledgement.
    #[serde(default)]
    pub automated: bool,
    #[serde(default)]
    pub provider_message_id: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    /// Attachments in provider order, with primary and filed document slots
    /// filling older records' gaps. Empty outside a Project-scoped thread.
    #[serde(default)]
    pub attachments: Vec<InboxAttachmentView>,
}

/// One transcript attachment: its provider filename, optional document kind,
/// and authorized document or Message download link.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxAttachmentView {
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    /// The stored MIME type; an `image/*` attachment renders inline.
    #[serde(default)]
    pub content_type: Option<String>,
    pub href: String,
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
    pub direction_filter: Option<String>,
    /// Whether the `?attachments=true` filter is on — rows with a filed
    /// attachment only (LAW-238 D3).
    #[serde(default)]
    pub attachments_only: bool,
    /// Whether the Sent folder (`?sent=true`) is selected.
    #[serde(default)]
    pub sent_only: bool,
    #[serde(default)]
    pub brands: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    /// The Project code when this workspace lists one Project's inbox — the
    /// reading pane fetches through the Project-scoped server function.
    #[serde(default)]
    pub project_code: Option<String>,
    pub error: Option<String>,
    /// Where the compose dialog posts a new text; absent unless this viewer
    /// may text a new number and the deployment's number is registered.
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
pub(crate) async fn chrome() -> (Option<crate::components::AppLogo>, String, String) {
    (
        crate::app_chrome::app_logo_from_context().await,
        crate::app_chrome::app_tokens_href_from_context().await,
        crate::app_chrome::firm_name_from_context().await,
    )
}

#[cfg(feature = "server")]
pub(crate) async fn person_id() -> Option<uuid::Uuid> {
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
/// The Surreal handle: a request extension on the reading pane's directly
/// mounted endpoints, which run outside the page's Dioxus runtime, else the
/// render's context provider.
#[cfg(feature = "server")]
async fn surreal() -> store::surreal::SurrealDb {
    match dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<store::surreal::SurrealDb>,
        _,
    >()
    .await
    {
        Ok(axum::Extension(surreal)) => surreal,
        Err(_) => consume_context::<store::surreal::SurrealDb>(),
    }
}

#[cfg(feature = "server")]
pub(crate) async fn csrf_token() -> String {
    dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<crate::csrf::CsrfToken>, _>()
        .await
        .map(|axum::Extension(token)| token.0)
        .unwrap_or_default()
}

#[cfg(feature = "server")]
pub(crate) async fn query_error() -> Option<String> {
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
    filter
        .channel
        .filter(|channel| inbox_channels().contains(channel))
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

#[cfg(feature = "server")]
async fn selected_direction() -> Option<String> {
    #[derive(serde::Deserialize, Default)]
    struct Filter {
        direction: Option<String>,
    }
    let filter =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<Filter>, _>()
            .await
            .ok()
            .map(|axum::extract::Query(filter)| filter)?;
    filter.direction.filter(|direction| {
        matches!(
            direction.as_str(),
            store::inbox::direction::INBOUND
                | store::inbox::direction::OUTBOUND
                | store::inbox::direction::INTERNAL
        )
    })
}

#[cfg(feature = "server")]
async fn attachments_only() -> bool {
    #[derive(serde::Deserialize, Default)]
    struct Filter {
        attachments: Option<bool>,
    }
    dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<Filter>, _>()
        .await
        .ok()
        .and_then(|axum::extract::Query(filter)| filter.attachments)
        .unwrap_or(false)
}

#[cfg(feature = "server")]
async fn sent_only() -> bool {
    #[derive(serde::Deserialize, Default)]
    struct Filter {
        sent: Option<bool>,
    }
    dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<Filter>, _>()
        .await
        .ok()
        .and_then(|axum::extract::Query(filter)| filter.sent)
        .unwrap_or(false)
}

/// The filters in effect on one inbox list — what every rail link carries
/// forward except the one it changes.
#[derive(Clone, Copy, Default)]
struct Filters<'a> {
    channel: Option<&'a str>,
    brand: Option<&'a str>,
    unread: bool,
    direction: Option<&'a str>,
    attachments: bool,
    sent: bool,
}

impl<'a> Filters<'a> {
    fn of(page: &'a InboxPage) -> Self {
        Self {
            channel: page.channel_filter.as_deref(),
            brand: page.brand_filter.as_deref(),
            unread: page.unread_only,
            direction: page.direction_filter.as_deref(),
            attachments: page.attachments_only,
            sent: page.sent_only,
        }
    }

    fn href(self, base: &str) -> String {
        filter_href(
            base,
            self.channel,
            self.brand,
            self.unread,
            self.direction,
            self.attachments,
            self.sent,
        )
    }
}

fn filter_href(
    base: &str,
    channel: Option<&str>,
    brand: Option<&str>,
    unread: bool,
    direction: Option<&str>,
    attachments: bool,
    sent: bool,
) -> String {
    let mut parameters = Vec::new();
    for (key, value) in [
        ("channel", channel),
        ("brand", brand),
        ("direction", direction),
    ] {
        if let Some(value) = value {
            let encoded =
                percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC);
            parameters.push(format!("{key}={encoded}"));
        }
    }
    if unread {
        parameters.push("unread=true".into());
    }
    if attachments {
        parameters.push("attachments=true".into());
    }
    if sent {
        parameters.push("sent=true".into());
    }
    if parameters.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", parameters.join("&"))
    }
}

/// One `(label, href, active)` triple for [`inbox_rail_group`].
type RailOption = (String, String, bool);

fn rail_option(label: &str, href: String, active: bool) -> RailOption {
    (label.to_string(), href, active)
}

/// One folder-rail group: a titled nav of stacked filter links, each carrying
/// `aria-current` when it is the filter in effect. The URL-query hrefs are
/// unchanged from the inline filter rows this rail replaced, so links stay
/// shareable and the browser e2e selectors keep working.
fn inbox_rail_group(aria_label: &str, title: &str, options: &[RailOption]) -> Element {
    rsx! {
        nav { class: "inbox-rail__group", "aria-label": "{aria_label}",
            h2 { class: "inbox-rail__title", "{title}" }
            ul {
                for (label, href, active) in options.iter() {
                    li {
                        a {
                            class: if *active { "inbox-rail__link inbox-rail__link--active" } else { "inbox-rail__link" },
                            href: "{href}",
                            "aria-current": active.then_some("true"),
                            "{label}"
                        }
                    }
                }
            }
        }
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

#[cfg(any(feature = "server", test))]
fn direction_matches(row_direction: &str, selected: Option<&str>) -> bool {
    selected.is_none_or(|direction| direction == row_direction)
}

#[cfg(feature = "server")]
fn row_matches(
    row: &store::inbox::IntakeRow,
    channel_filter: Option<&str>,
    brand_filter: Option<&str>,
    unread_only: bool,
    direction_filter: Option<&str>,
    attachments_only: bool,
    sent_only: bool,
) -> bool {
    channel_matches(&row.channel, channel_filter)
        && brand_matches(row.brand_key.as_deref(), brand_filter)
        && (!unread_only || row.unread)
        && direction_matches(&row.direction, direction_filter)
        && (!attachments_only || row.has_attachments)
        && (!sent_only || row.sent)
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
        store::inbox::channel::VOICE.into(),
        store::inbox::channel::FACEBOOK.into(),
        store::inbox::channel::INSTAGRAM.into(),
        store::inbox::channel::THREADS.into(),
        store::inbox::channel::YOUTUBE.into(),
        store::inbox::channel::TIKTOK.into(),
        store::inbox::channel::X.into(),
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
        direction: row.direction.clone(),
        has_attachments: row.has_attachments,
        sent: row.sent,
        preview: row.preview.clone(),
        person: row.person.as_ref().map(|person| InboxPersonView {
            name: person.name.clone(),
        }),
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
        occurred_label: occurred_label(&message.occurred_at),
        automated: message.direction == store::inbox::direction::OUTBOUND
            && message.author_person_id.is_none(),
        occurred_at: message.occurred_at.clone(),
        provider_message_id: message
            .provider_message_id
            .clone()
            .or_else(|| message.provider_ref.clone()),
        attachments: Vec::new(),
    }
}

/// A stored RFC 3339 instant as the transcript shows it, in UTC.
#[cfg(any(feature = "server", test))]
fn occurred_label(occurred_at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(occurred_at).map_or_else(
        |_| occurred_at.to_string(),
        |at| {
            at.with_timezone(&chrono::Utc)
                .format("%b %-d, %Y, %H:%M UTC")
                .to_string()
        },
    )
}

/// The attachments one transcript Message renders, as the thread page
/// receives them: provider links retain their names and order; older primary
/// and provenance slots fill any gaps. Document assets link to their revision;
/// bare Inbox assets use the Project/Conversation/Message-authorized download.
#[cfg(feature = "server")]
#[must_use]
fn thread_attachment_views(
    code: &str,
    project_id: uuid::Uuid,
    conversation_id: uuid::Uuid,
    message_id: uuid::Uuid,
    primary: Option<store::assets::Asset>,
    filed: Vec<store::assets::Asset>,
    linked: Vec<store::inbox::MessageAttachment>,
) -> Vec<InboxAttachmentView> {
    attachment_views(primary, filed, linked, |id, asset| {
        if asset.is_some_and(|asset| asset.project_id == Some(project_id)) {
            format!("/app/projects/{code}/documents/{id}")
        } else {
            format!("/app/api/projects/{project_id}/inbox/{conversation_id}/messages/{message_id}/attachments/{id}")
        }
    })
}

/// Linked attachments first, in provider order, then any primary or filed
/// slot not already listed — each href built by `href`.
#[cfg(feature = "server")]
fn attachment_views(
    primary: Option<store::assets::Asset>,
    filed: Vec<store::assets::Asset>,
    linked: Vec<store::inbox::MessageAttachment>,
    href: impl Fn(uuid::Uuid, Option<&store::assets::Asset>) -> String,
) -> Vec<InboxAttachmentView> {
    let mut attachments = Vec::new();
    let mut seen = Vec::new();
    let assets = primary.into_iter().chain(filed).collect::<Vec<_>>();
    for link in linked {
        if seen.contains(&link.asset_id) {
            continue;
        }
        seen.push(link.asset_id);
        let asset = assets.iter().find(|asset| asset.id == link.asset_id);
        attachments.push(InboxAttachmentView {
            filename: Some(link.filename),
            kind: asset.and_then(|asset| asset.kind.clone()),
            content_type: asset.map(|asset| asset.content_type.clone()),
            href: href(link.asset_id, asset),
        });
    }
    for asset in &assets {
        if seen.contains(&asset.id) {
            continue;
        }
        seen.push(asset.id);
        attachments.push(InboxAttachmentView {
            filename: asset.filename.clone(),
            kind: asset.kind.clone(),
            content_type: Some(asset.content_type.clone()),
            href: href(asset.id, Some(asset)),
        });
    }
    attachments
}

/// A firm-inbox thread's attachments, through the firm-authorized download
/// (`/app/inbox/{conversation}/messages/{message}/attachments/{asset}`). A
/// failed lookup keeps the readable transcript.
#[cfg(feature = "server")]
async fn firm_messages_with_attachments(
    surreal: &store::surreal::SurrealDb,
    firm_id: uuid::Uuid,
    conversation_id: uuid::Uuid,
    messages: &[store::inbox::Message],
) -> Vec<InboxMessageView> {
    let mut views = Vec::with_capacity(messages.len());
    for message in messages {
        let mut view = message_view(message);
        let primary = match message.attachment_asset_id {
            Some(asset_id) => store::assets::find_by_id(surreal, asset_id)
                .await
                .unwrap_or_default(),
            None => None,
        };
        let linked = store::inbox::message_assets(surreal, firm_id, message.id)
            .await
            .unwrap_or_default();
        let message_id = message.id;
        view.attachments = attachment_views(primary, Vec::new(), linked, |id, _| {
            format!("/app/inbox/{conversation_id}/messages/{message_id}/attachments/{id}")
        });
        views.push(view);
    }
    views
}

/// Resolve a Project thread's ordered Message attachments and older primary
/// or provenance slots. A failed lookup keeps the readable transcript and
/// whatever attachment sources remain available.
#[cfg(feature = "server")]
async fn thread_messages_with_attachments(
    surreal: &store::surreal::SurrealDb,
    code: &str,
    project_id: uuid::Uuid,
    firm_id: uuid::Uuid,
    conversation_id: uuid::Uuid,
    messages: &[store::inbox::Message],
) -> Vec<InboxMessageView> {
    let mut views = Vec::with_capacity(messages.len());
    for message in messages {
        let mut view = message_view(message);
        let primary = match message.attachment_asset_id {
            Some(asset_id) => store::assets::find_by_id(surreal, asset_id)
                .await
                .unwrap_or_default(),
            None => None,
        };
        let filed =
            store::assets::find_by_source_message_id(surreal, project_id, &message.id.to_string())
                .await
                .unwrap_or_default();
        let linked = store::inbox::message_assets(surreal, firm_id, message.id)
            .await
            .unwrap_or_default();
        view.attachments = thread_attachment_views(
            code,
            project_id,
            conversation_id,
            message.id,
            primary,
            filed,
            linked,
        );
        views.push(view);
    }
    views
}

/// The link text for one inline attachment: its filename, falling back to
/// "Untitled attachment", with the kind in parentheses when the asset
/// carries one.
#[must_use]
fn attachment_label(attachment: &InboxAttachmentView) -> String {
    let name = attachment
        .filename
        .as_deref()
        .unwrap_or("Untitled attachment");
    match attachment.kind.as_deref() {
        Some(kind) if !kind.is_empty() => format!("{name} ({kind})"),
        _ => name.to_string(),
    }
}

/// Whether an attachment renders inline as an image: an `image/*` MIME type,
/// else a common browser image extension on its filename. HEIC and TIFF stay
/// links — most browsers cannot draw them.
#[must_use]
fn is_inline_image(attachment: &InboxAttachmentView) -> bool {
    const RENDERABLE: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "avif"];
    if let Some(content_type) = attachment.content_type.as_deref() {
        let content_type = content_type.to_ascii_lowercase();
        return content_type.starts_with("image/")
            && RENDERABLE
                .iter()
                .any(|kind| content_type.ends_with(kind));
    }
    attachment
        .filename
        .as_deref()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, extension)| {
            RENDERABLE.contains(&extension.to_ascii_lowercase().as_str())
        })
}

/// Starting a text needs a viewer whose role may send one and a deployment
/// whose SMS number is in the Brand phone-number registry — the same three
/// facts `POST /app/inbox/texts` checks, so the dialog is offered only when a
/// send could succeed. The server re-checks authority on submit.
#[cfg(feature = "server")]
async fn compose_action(role: ViewerRole) -> Option<String> {
    if !matches!(role, ViewerRole::Owner | ViewerRole::Admin) {
        return None;
    }
    let number = std::env::var("NAVIGATOR_SMS_NUMBER")
        .ok()
        .map(|number| number.trim().to_string())
        .filter(|number| !number.is_empty())?;
    let registry = store::postgres::PostgresDb::from_env().await.ok()?;
    store::brand_phone_numbers::find_by_number(&registry, &number)
        .await
        .ok()
        .flatten()
        .map(|_| format!("{INBOX_PATH}/texts"))
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
    let direction_filter = selected_direction().await;
    let attachments_only = attachments_only().await;
    let sent_only = sent_only().await;
    let brands = store::inbox::intake_brands(&surreal, actor)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let brand_filter = brand_filter.filter(|brand| brands.contains(brand));
    let scoped_rows = match brand_filter.as_deref() {
        Some(brand) => store::inbox::list_intake_in_brand(&surreal, actor, brand).await,
        None => store::inbox::list_intake(&surreal, actor).await,
    }
    .map_err(|error| ServerFnError::new(error.to_string()))?;
    let rows = scoped_rows
        .into_iter()
        .filter(|row| {
            row_matches(
                row,
                channel_filter.as_deref(),
                brand_filter.as_deref(),
                unread_only,
                direction_filter.as_deref(),
                attachments_only,
                sent_only,
            )
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
        direction_filter,
        attachments_only,
        sent_only,
        brands,
        channels: inbox_channels(),
        project_code: None,
        rows: rows
            .iter()
            .map(|row| row_view(row, format!("/app/inbox/{}", row.id)))
            .collect(),
        error: query_error().await,
        compose_action: compose_action(role).await,
        csrf_token: csrf_token().await,
        logo,
        tokens_href,
        firm_name,
    })
}

/// Owner conversation, with an assign control.
#[server]
pub async fn owner_inbox_thread() -> Result<InboxThreadPage, ServerFnError> {
    let axum::extract::Path(conversation_id) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<uuid::Uuid>, _>()
            .await
            .map_err(|_| ServerFnError::new("missing conversation"))?;
    firm_thread_page(conversation_id).await
}

/// The reading pane's fetch: one firm-inbox conversation by id, the same
/// firm-person authorization and shape as the thread route.
#[server(prefix = "", endpoint = "app/inbox/pane")]
pub async fn inbox_conversation(conversation_id: String) -> Result<InboxThreadPage, ServerFnError> {
    firm_thread_page(
        conversation_id
            .parse()
            .map_err(|_| ServerFnError::new("missing conversation"))?,
    )
    .await
}

#[cfg(feature = "server")]
async fn firm_thread_page(conversation_id: uuid::Uuid) -> Result<InboxThreadPage, ServerFnError> {
    let role = crate::admin_listing::require_firm_person().await?;
    let surreal = surreal().await;
    let actor = person_id()
        .await
        .ok_or_else(|| ServerFnError::new("owner access required"))?;
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
        messages: firm_messages_with_attachments(
            &surreal,
            detail.conversation.firm_id,
            conversation_id,
            &detail.messages,
        )
        .await,
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
    let surreal = surreal().await;
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
    let direction_filter = selected_direction().await;
    let attachments_only = attachments_only().await;
    let sent_only = sent_only().await;
    let all_rows = store::inbox::list_project(&surreal, actor, project.id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let brands = inbox_brands(&all_rows);
    let brand_filter = brand_filter.filter(|brand| brands.contains(brand));
    let rows = all_rows
        .into_iter()
        .filter(|row| {
            row_matches(
                row,
                channel_filter.as_deref(),
                brand_filter.as_deref(),
                unread_only,
                direction_filter.as_deref(),
                attachments_only,
                sent_only,
            )
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
        direction_filter,
        attachments_only,
        sent_only,
        brands,
        channels: inbox_channels(),
        project_code: Some(project_code.clone()),
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
    let axum::extract::Path((project_code, conversation_id)) =
        dioxus_fullstack_core::FullstackContext::extract::<
            axum::extract::Path<(String, uuid::Uuid)>,
            _,
        >()
        .await
        .map_err(|_| ServerFnError::new("missing conversation"))?;
    project_thread_page(project_code, conversation_id).await
}

/// The Project reading pane's fetch: one conversation by id under its
/// Project code, the same participant authorization and shape as the
/// Project thread route.
#[server(prefix = "", endpoint = "app/projects/inbox-pane")]
pub async fn project_inbox_conversation(
    project_code: String,
    conversation_id: String,
) -> Result<InboxThreadPage, ServerFnError> {
    project_thread_page(
        project_code,
        conversation_id
            .parse()
            .map_err(|_| ServerFnError::new("missing conversation"))?,
    )
    .await
}

#[cfg(feature = "server")]
async fn project_thread_page(
    project_code: String,
    conversation_id: uuid::Uuid,
) -> Result<InboxThreadPage, ServerFnError> {
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    let surreal = surreal().await;
    let actor = person_id()
        .await
        .ok_or_else(|| ServerFnError::new("not allowed"))?;
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
        messages: thread_messages_with_attachments(
            &surreal,
            &project_code,
            project.id,
            thread.conversation.firm_id,
            thread.conversation.id,
            &thread.messages,
        )
        .await,
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
    rsx! { InboxWorkspace { page } }
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
    rsx! { InboxWorkspace { page } }
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

/// The inbox workspace, laid out like a mail client: the folder rail on the
/// left, and to its right the conversation list with the reading pane
/// beneath it at the list's full width. Selection is a client signal — the
/// deep-link thread route stays for mobile and for clients without
/// hydration.
#[component]
fn InboxWorkspace(page: InboxPage) -> Element {
    let selected = use_signal(|| None::<String>);
    rsx! {
        document::Title { "{page.firm_name} | {page.title}" }
        document::Meta { name: "description", content: "Firm and project conversations." }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{page.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(page.role),
            logo: page.logo.clone(),
        }
        main { id: "inbox", class: "nav-theme",
            { inbox_rail(&page) }
            div { class: "inbox-main",
                { inbox_list(&page, selected) }
                InboxPane {
                    selected,
                    project_code: page.project_code.clone(),
                    csrf_token: page.csrf_token.clone(),
                }
            }
            { compose_dialog(&page) }
        }
    }
}

/// The compose dialog's element id; the rail's Compose button opens it.
const COMPOSE_DIALOG_ID: &str = "inbox-compose";

/// The new-text form as a modal `<dialog>`, rendered only when the server
/// offers a compose action. Opening it is `showModal()`, so the browser
/// traps focus and closes it on Escape.
fn compose_dialog(page: &InboxPage) -> Element {
    let Some(action) = page.compose_action.clone() else {
        return rsx! {};
    };
    rsx! {
        dialog {
            id: COMPOSE_DIALOG_ID,
            class: "inbox-compose nav-card",
            "aria-labelledby": "{COMPOSE_DIALOG_ID}-title",
            div { class: "inbox-compose__header",
                h2 { id: "{COMPOSE_DIALOG_ID}-title", class: "inbox-compose__title", "New text" }
                button {
                    class: "inbox-compose__close",
                    r#type: "button",
                    "aria-label": "Close",
                    onclick: move |_| {
                        document::eval(&format!(
                            "document.getElementById('{COMPOSE_DIALOG_ID}')?.close();"
                        ));
                    },
                    span { "aria-hidden": "true", "×" }
                }
            }
            FormCard {
                title: "New text".to_string(),
                action,
                submit_label: "Send text".to_string(),
                heading: Heading::Hidden,
                csrf_token: Some(page.csrf_token.clone()),
                fields: vec![
                    Field::text("To (E.164, e.g. +15555550123)", "to", "").required(),
                    Field::textarea("Message", "body", "", 6).required(),
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
    }
}

#[allow(clippy::too_many_lines)]
fn inbox_list(page: &InboxPage, selected: Signal<Option<String>>) -> Element {
    let title = if page.sent_only {
        "Sent".to_string()
    } else {
        page.title.clone()
    };
    let subtitle = if page.sent_only {
        "Conversations someone at the firm wrote in, newest first."
    } else {
        "Every email, text, and message to the firm, newest first."
    };
    let rows = page.rows.iter().map(|row| {
        let unread = if row.unread { "Unread" } else { "Read" };
        let brand = row.brand_key.as_deref().unwrap_or("unbranded");
        let selected_now = selected.read().as_ref() == Some(&row.id);
        let row_class = if selected_now {
            "inbox-table__row inbox-table__row--selected"
        } else if row.unread {
            "inbox-table__row inbox-table__row--unread"
        } else {
            "inbox-table__row"
        };
        let mut selected = selected;
        let row_id = row.id.clone();
        rsx! {
            tr {
                key: "{row.id}",
                id: "inbox-row-{row.id}",
                class: "{row_class}",
                aria_current: selected_now.then_some("true"),
                td { class: "inbox-table__state",
                    span { class: "inbox-table__dot", aria_hidden: "true" }
                    span { class: "nav-visually-hidden", "{unread}" }
                }
                td { class: "inbox-table__sender",
                    "{row.sender}"
                    if let Some(person) = row.person.as_ref() {
                        span { class: "inbox-person-badge", "{person.name}" }
                    }
                }
                td { class: "inbox-table__subject",
                    a {
                        href: "{row.href}",
                        onclick: move |event: MouseEvent| {
                            event.prevent_default();
                            selected.set(Some(row_id.clone()));
                            document::eval(
                                "const t = document.getElementById('inbox-pane-title'); \
                                 t?.focus({preventScroll: true}); \
                                 t?.scrollIntoView({behavior: 'smooth', block: 'start'});",
                            );
                        },
                        "{row.subject}"
                    }
                    if let Some(preview) = row.preview.as_ref() {
                        span { class: "inbox-table__preview", "{preview}" }
                    }
                }
                td { class: "inbox-table__tag", "{brand}" }
                td { class: "inbox-table__tag", "{row.channel}" }
                td { class: "inbox-table__assignee", "{row.assignee}" }
                td { class: "inbox-table__date",
                    time { datetime: "{row.updated_iso}", "{row.updated}" }
                }
            }
        }
    });
    rsx! {
        div { class: "inbox-list",
            header { class: "page-header inbox-list__header",
                h1 { "{title}" }
                a { href: crate::communication_dashboard::PARKED_CALLS_PATH, "Parked calls" }
                p { class: "page-subtitle", "{subtitle}" }
            }
            if let Some(error) = page.error.clone() {
                p { class: "page-subtitle", role: "alert", "{error}" }
            }
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
                                th { scope: "col", "Project" }
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

/// Folder-rail link sets: the Inbox and Sent mailboxes, then one option per
/// value of each filter, with the filter in effect marked active. Hrefs keep
/// the URL-query semantics, so every view is a shareable link.
fn inbox_rail(page: &InboxPage) -> Element {
    let base = page.base_href.as_str();
    let current = Filters::of(page);
    let mailbox_options = vec![
        rail_option(
            "Inbox",
            Filters { sent: false, ..current }.href(base),
            !page.sent_only,
        ),
        rail_option(
            "Sent",
            Filters { sent: true, ..current }.href(base),
            page.sent_only,
        ),
    ];
    let mut channel_options = vec![rail_option(
        "All channels",
        Filters { channel: None, ..current }.href(base),
        page.channel_filter.is_none(),
    )];
    for channel in &page.channels {
        channel_options.push(rail_option(
            channel,
            Filters { channel: Some(channel.as_str()), ..current }.href(base),
            page.channel_filter.as_deref() == Some(channel.as_str()),
        ));
    }
    let mut brand_options = vec![rail_option(
        "All Brands",
        Filters { brand: None, ..current }.href(base),
        page.brand_filter.is_none(),
    )];
    for brand in &page.brands {
        brand_options.push(rail_option(
            brand,
            Filters { brand: Some(brand.as_str()), ..current }.href(base),
            page.brand_filter.as_deref() == Some(brand.as_str()),
        ));
    }
    let unread_options = vec![
        rail_option(
            "All messages",
            Filters { unread: false, ..current }.href(base),
            !page.unread_only,
        ),
        rail_option(
            "Unread only",
            Filters { unread: true, ..current }.href(base),
            page.unread_only,
        ),
    ];
    let mut direction_options = vec![rail_option(
        "Any direction",
        Filters { direction: None, ..current }.href(base),
        page.direction_filter.is_none(),
    )];
    for direction in ["inbound", "outbound", "internal"] {
        direction_options.push(rail_option(
            direction,
            Filters { direction: Some(direction), ..current }.href(base),
            page.direction_filter.as_deref() == Some(direction),
        ));
    }
    let attachment_options = vec![
        rail_option(
            "Any attachment state",
            Filters { attachments: false, ..current }.href(base),
            !page.attachments_only,
        ),
        rail_option(
            "With attachments",
            Filters { attachments: true, ..current }.href(base),
            page.attachments_only,
        ),
    ];
    rsx! {
        aside { class: "inbox-rail", "aria-label": "Inbox folders",
            if page.compose_action.is_some() {
                button {
                    class: "nav-btn nav-btn--primary inbox-rail__compose",
                    r#type: "button",
                    "aria-haspopup": "dialog",
                    "aria-controls": COMPOSE_DIALOG_ID,
                    onclick: move |_| {
                        document::eval(&format!(
                            "document.getElementById('{COMPOSE_DIALOG_ID}')?.showModal();"
                        ));
                    },
                    "Compose"
                }
            }
            { inbox_rail_group("Mailboxes", "Mail", &mailbox_options) }
            { inbox_rail_group("Filter inbox by channel", "Channel", &channel_options) }
            { inbox_rail_group("Filter inbox by Brand", "Brand", &brand_options) }
            { inbox_rail_group("Inbox unread filter", "Unread", &unread_options) }
            { inbox_rail_group("Inbox direction filter", "Direction", &direction_options) }
            { inbox_rail_group("Inbox attachments filter", "Attachments", &attachment_options) }
        }
    }
}

/// The reading pane: always present beneath the list, so focus has a stable
/// target. Its content fetches per selection.
#[component]
fn InboxPane(
    selected: ReadSignal<Option<String>>,
    project_code: Option<String>,
    csrf_token: String,
) -> Element {
    rsx! {
        section { id: "inbox-pane", class: "inbox-pane",
            "aria-label": "Conversation reading pane",
            h2 { id: "inbox-pane-title", class: "inbox-pane__title", tabindex: "-1",
                "Reading pane"
            }
            if let Some(conversation_id) = selected().clone() {
                InboxPaneThread {
                    key: "{conversation_id}",
                    conversation_id,
                    project_code,
                }
            } else {
                p { class: "inbox-pane__empty", "Select a conversation to read it here." }
            }
        }
    }
}

/// One conversation's transcript in the reading pane — the same fetch the
/// thread route answers, remounted per selection through the `key`.
#[component]
fn InboxPaneThread(conversation_id: String, project_code: Option<String>) -> Element {
    let fetch_conversation_id = conversation_id.clone();
    let fetch_project_code = project_code.clone();
    let resource = use_server_future(move || {
        let conversation_id = fetch_conversation_id.clone();
        let project_code = fetch_project_code.clone();
        async move { pane_conversation(project_code, conversation_id).await }
    })?;
    let view = match &*resource.read() {
        Some(Ok(page)) => {
            let href = project_code.as_ref().map_or_else(
                || format!("/app/inbox/{conversation_id}"),
                |code| format!("/app/projects/{code}/inbox/{conversation_id}"),
            );
            inbox_pane_body(page, &href)
        }
        Some(Err(_)) => {
            rsx! { p { class: "inbox-pane__empty", "Could not load the conversation." } }
        }
        None => rsx! { p { class: "inbox-pane__empty", "Loading…" } },
    };
    view
}

/// The reading pane's fetch: the Project-scoped server function when the
/// workspace lists one Project's inbox, else the firm-wide one.
async fn pane_conversation(
    project_code: Option<String>,
    conversation_id: String,
) -> Result<InboxThreadPage, ServerFnError> {
    if let Some(project_code) = project_code {
        project_inbox_conversation(project_code, conversation_id).await
    } else {
        inbox_conversation(conversation_id).await
    }
}

/// The reading pane's transcript: the subject, the conversation's facts as
/// chips, an assign-to-project control, and the messages. Replies, notes,
/// and number linking stay on the full thread the pane links to.
pub fn inbox_pane_body(page: &InboxThreadPage, href: &str) -> Element {
    let brand = page.brand_key.as_deref().unwrap_or("unbranded");
    let messages = page
        .messages
        .iter()
        .map(|message| inbox_message_article(message, page));
    rsx! {
        div { class: "inbox-pane__thread",
            div { class: "inbox-pane__heading",
                h3 { "{page.title}" }
                a { class: "nav-link", href: "{href}", "Open full thread" }
            }
            ul { class: "inbox-chips", "aria-label": "Conversation facts",
                li { class: "inbox-chip", "{page.channel}" }
                li { class: "inbox-chip", "{brand}" }
                li { class: "inbox-chip", "Project: {page.assignee}" }
            }
            { pane_assign(page) }
            if page.bodies_visible {
                { messages }
            } else {
                p { class: "inbox-pane__empty",
                    "Message bodies stay on the project. Assign or reassign from here."
                }
            }
        }
    }
}

/// The pane's one-line assign control: a Project picker posting to the
/// conversation's assign action and returning to the inbox. Absent without an
/// action or a Project to choose.
fn pane_assign(page: &InboxThreadPage) -> Element {
    let Some(action) = page.assign_action.clone() else {
        return rsx! {};
    };
    if page.projects.is_empty() {
        return rsx! {};
    }
    let label = if page.assignee == "Unassigned" {
        "Assign to project"
    } else {
        "Move to project"
    };
    rsx! {
        form { class: "inbox-assign", method: "post", action: "{action}",
            if !page.csrf_token.is_empty() {
                input { r#type: "hidden", name: "_csrf", value: "{page.csrf_token}" }
            }
            input { r#type: "hidden", name: "next", value: "inbox" }
            label { r#for: "inbox-assign-project", class: "inbox-assign__label", "{label}" }
            select { id: "inbox-assign-project", name: "project_id", class: "nav-input", required: true,
                option { value: "", "Choose a project…" }
                for project in page.projects.iter() {
                    option { key: "{project.id}", value: "{project.id}", "{project.label}" }
                }
            }
            button { class: "nav-btn nav-btn--primary", r#type: "submit", "Assign" }
        }
    }
}

/// Who a message is from and to, as a mail header shows it: an inbound
/// message comes from the counterparty to the firm's receiving account; a
/// firm message goes the other way; an internal note stays inside.
fn message_parties(message: &InboxMessageView, page: &InboxThreadPage) -> (String, String) {
    let account = if page.provider_account.is_empty() {
        page.source_page.clone().unwrap_or_default()
    } else {
        page.provider_account.clone()
    };
    let firm = if page.firm_name.is_empty() {
        "The firm".to_string()
    } else {
        page.firm_name.clone()
    };
    if message.internal {
        ("Internal note".into(), "Visible to the firm only".into())
    } else if message.direction == "outbound" {
        let from = if message.automated {
            format!("{firm} (automatic)")
        } else {
            firm
        };
        (from, page.sender.clone())
    } else {
        (page.sender.clone(), account)
    }
}

/// One transcript message as a mail card: a From / To header with the time,
/// the body, then attachments — images drawn at the card's full width, every
/// other file a download link. Shared by the reading pane and the thread page.
fn inbox_message_article(message: &InboxMessageView, page: &InboxThreadPage) -> Element {
    let (from, to) = message_parties(message, page);
    let audience = message.visibility.as_deref().map_or("", |visibility| {
        if visibility == "public" {
            "Public"
        } else if visibility == "private" {
            "Private"
        } else {
            ""
        }
    });
    let when = if message.occurred_label.is_empty() {
        message.occurred_at.clone()
    } else {
        message.occurred_label.clone()
    };
    let card = if message.internal {
        "inbox-message inbox-message--internal"
    } else if message.direction == "outbound" {
        "inbox-message inbox-message--outbound"
    } else {
        "inbox-message"
    };
    let (images, files): (Vec<_>, Vec<_>) =
        message.attachments.iter().partition(|attachment| is_inline_image(attachment));
    rsx! {
        article {
            key: "{message.id}",
            id: "inbox-message-{message.id}",
            class: "{card}",
            header { class: "inbox-message__header",
                div { class: "inbox-message__parties",
                    p { class: "inbox-message__from", "{from}" }
                    if !to.is_empty() {
                        p { class: "inbox-message__to", "to {to}" }
                    }
                }
                div { class: "inbox-message__meta",
                    time { datetime: "{message.occurred_at}", "{when}" }
                    span { class: "inbox-message__delivery",
                        "{page.channel} · {message.delivery}"
                        if !audience.is_empty() { " · {audience}" }
                    }
                }
            }
            if message.delivery == "unknown" {
                p { class: "nav-muted", "Delivery has not been confirmed. Check the provider before sending another reply." }
            } else if message.delivery == "failed" {
                p { class: "nav-muted", "The provider rejected this reply. Check the receiving account and channel configuration." }
            }
            div { class: "inbox-message__body", "{message.body}" }
            for image in images.iter() {
                figure { class: "inbox-message__image",
                    a { href: "{image.href}",
                        img {
                            src: "{image.href}",
                            alt: "{attachment_label(image)}",
                            loading: "lazy",
                        }
                    }
                    figcaption { class: "nav-muted", "{attachment_label(image)}" }
                }
            }
            if !files.is_empty() {
                ul { class: "inbox-message-attachments",
                    for attachment in files.iter() {
                        li {
                            a { class: "nav-link", href: "{attachment.href}",
                                "{attachment_label(attachment)}"
                            }
                        }
                    }
                }
            }
            if let Some(reference) = message.provider_message_id.as_ref() {
                p { class: "inbox-message__reference nav-muted", "Provider message: {reference}" }
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
    let fi_metadata = page.provider_account.starts_with("google_fi:");
    let next_action = if page.next_action == "Reassign"
        && page.assign_action.is_none()
        && (page.channel == "voice" || fi_metadata)
    {
        "Review"
    } else {
        page.next_action.as_str()
    };
    let messages = page
        .messages
        .iter()
        .map(|message| inbox_message_article(message, page));
    let assign = page.assign_action.clone().map(|action| {
        let choices = page
            .projects
            .iter()
            .map(|project| Choice::new(project.id.clone(), project.label.clone()))
            .collect();
        rsx! {
            FormCard {
                title: "Assign to a project".to_string(),
                action,
                submit_label: "Assign".to_string(),
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
                p { class: "page-subtitle", "Next: {next_action}" }
                if unsupported_private {
                    p { "This channel supports public comment replies only; private replies are unavailable." }
                }
                if fi_metadata {
                    p { class: "nav-muted", "Imported Fi history cannot send external replies." }
                } else if page.reply_action.is_none() {
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
        let html = list_html(&view);
        assert!(html.contains("channel=sms&#38;brand=vesta"), "{html}");
    }

    #[test]
    fn direction_and_attachment_filters_compose_with_the_existing_ones() {
        let mut view = page(
            ViewerRole::Lawyer,
            vec![
                InboxRowView {
                    id: "c1".into(),
                    href: "/app/inbox/c1".into(),
                    channel: "email".into(),
                    brand_key: Some("neon".into()),
                    assignee: "Unassigned".into(),
                    unread: true,
                    delivery: "received".into(),
                    next_action: String::new(),
                    subject: "Notice".into(),
                    sender: "court@example.com".into(),
                    updated: "Oct 11, 04:06 UTC".into(),
                    updated_iso: "2026-10-11T04:06:15+00:00".into(),
                    direction: store::inbox::direction::INBOUND.into(),
                    has_attachments: true,
                    sent: false,
                    preview: None,
                    person: None,
                },
                InboxRowView {
                    id: "c2".into(),
                    href: "/app/inbox/c2".into(),
                    channel: "email".into(),
                    brand_key: Some("neon".into()),
                    assignee: "Unassigned".into(),
                    unread: false,
                    delivery: "sent".into(),
                    next_action: String::new(),
                    subject: "Reply".into(),
                    sender: "firm@example.com".into(),
                    updated: "Oct 12, 04:06 UTC".into(),
                    updated_iso: "2026-10-12T04:06:15+00:00".into(),
                    direction: store::inbox::direction::OUTBOUND.into(),
                    has_attachments: false,
                    sent: false,
                    preview: None,
                    person: None,
                },
            ],
        );
        // All five filters at once: the channel/brand/unread trio from
        // before, plus direction and has-attachments (LAW-238 D3).
        view.channel_filter = Some("email".into());
        view.brand_filter = Some("neon".into());
        view.unread_only = true;
        view.direction_filter = Some(store::inbox::direction::INBOUND.into());
        view.attachments_only = true;
        let html = list_html(&view);
        // Every filter link preserves the other four selections.
        assert!(
            html.contains("channel=email&#38;brand=neon&#38;direction=inbound&#38;unread=true&#38;attachments=true"),
            "{html}"
        );
        // Dropping a filter drops it from every link — the all-channels
        // link carries only the surviving filters.
        view.attachments_only = false;
        view.direction_filter = None;
        let html = list_html(&view);
        assert!(
            html.contains(r#"href="/app/inbox?brand=neon&#38;unread=true""#),
            "{html}"
        );
    }

    /// The conversation's latest message direction and attachment flag
    /// decide the new filters, composed with the existing three.
    #[test]
    #[cfg(feature = "server")]
    fn row_matches_requires_every_selected_filter() {
        let row = store::inbox::IntakeRow {
            contact: None,
            id: uuid::Uuid::nil(),
            firm_id: uuid::Uuid::nil(),
            brand_key: Some("neon".into()),
            project_id: None,
            channel: "email".into(),
            assignee: "Unassigned".into(),
            unread: true,
            delivery: "received".into(),
            next_action: "Assign".into(),
            external_address: "court@example.com".into(),
            external_name: None,
            subject: Some("Notice".into()),
            direction: store::inbox::direction::INBOUND.into(),
            has_attachments: true,
            sent: false,
            preview: None,
            person: None,
            updated_at: chrono::Utc::now(),
        };
        let selected = (
            Some("email"),
            Some("neon"),
            true,
            Some(store::inbox::direction::INBOUND),
            true,
        );
        assert!(row_matches(
            &row, selected.0, selected.1, selected.2, selected.3, selected.4, false
        ));
        // Each filter is load-bearing: drop the match on any one of the
        // five and the row is excluded.
        for (index, miss) in [
            Some("call"),  // channel
            Some("vesta"), // brand
        ]
        .into_iter()
        .enumerate()
        {
            let (channel, brand) = if index == 0 {
                (miss, selected.1)
            } else {
                (selected.0, miss)
            };
            assert!(
                !row_matches(&row, channel, brand, selected.2, selected.3, selected.4, false),
                "filter {index} is load-bearing"
            );
        }
        assert!(row_matches(
            &row, selected.0, selected.1,
            false, // unread_only off: the unread row still matches
            selected.3, selected.4, false
        ));
        // unread_only on excludes a read row.
        let mut read_row = row.clone();
        read_row.unread = false;
        assert!(!row_matches(
            &read_row, selected.0, selected.1, true, selected.3, selected.4, false
        ));
        assert!(!row_matches(
            &row,
            selected.0,
            selected.1,
            true,
            Some(store::inbox::direction::OUTBOUND),
            selected.4, false
        ));
        // attachments_only on excludes a row with nothing filed.
        let mut bare_row = row.clone();
        bare_row.has_attachments = false;
        assert!(row_matches(
            &bare_row, selected.0, selected.1, true, selected.3, false, false
        ));
        assert!(!row_matches(
            &bare_row, selected.0, selected.1, true, selected.3, true, false
        ));
    }

    #[test]
    fn unread_links_keep_channel_and_brand_and_thread_shows_the_source() {
        let mut view = page(ViewerRole::Lawyer, Vec::new());
        view.brand_filter = Some("neon".into());
        view.channel_filter = Some("email".into());
        let html = list_html(&view);
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

    #[cfg(feature = "server")]
    #[test]
    fn voice_filter_preserves_the_selected_brand() {
        let mut view = page(ViewerRole::Owner, Vec::new());
        view.channels = super::inbox_channels();
        view.brand_filter = Some("neon".into());
        let html = list_html(&view);
        assert!(
            html.contains(r#"href="/app/inbox?channel=voice&#38;brand=neon""#),
            "the Inbox must offer voice calls without dropping the Brand filter: {html}"
        );
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
            direction_filter: None,
            attachments_only: false,
            sent_only: false,
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
                "x",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            error: None,
            logo: None,
            tokens_href: String::new(),
            firm_name: "Practice".into(),
            project_code: None,
        }
    }

    /// The workspace as a browser would receive it, selection unset.
    fn list_html(page: &InboxPage) -> String {
        dioxus_ssr::render_element(rsx! { InboxWorkspace { page: page.clone() } })
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
                occurred_label: String::new(),
                automated: false,
                provider_message_id: Some("source-message-1".into()),
                visibility: None,
                attachments: Vec::new(),
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
    fn compose_is_a_modal_dialog_offered_only_when_the_server_allows_it() {
        let plain = list_html(&page(ViewerRole::Owner, vec![]));
        assert!(!plain.contains("id=\"inbox-compose\""), "{plain}");
        assert!(!plain.contains(">Compose<"), "{plain}");

        let mut composing = page(ViewerRole::Owner, vec![]);
        composing.compose_action = Some("/app/inbox/texts".into());
        composing.csrf_token = "token".into();
        let html = list_html(&composing);
        assert!(html.contains("aria-controls=\"inbox-compose\""), "{html}");
        assert!(html.contains(">Compose<"), "{html}");
        // The form lives inside the closed dialog, never as a box on the page.
        let dialog = &html[html
            .find("<dialog id=\"inbox-compose\"")
            .expect("a compose dialog")..];
        assert!(!dialog[..dialog.find('>').unwrap()].contains(" open"), "{dialog}");
        assert!(dialog.contains("action=\"/app/inbox/texts\""), "{dialog}");
        for name in ["name=\"to\"", "name=\"body\"", "name=\"consent\""] {
            assert!(dialog.contains(name), "{name}: {dialog}");
        }
        assert_eq!(html.matches("action=\"/app/inbox/texts\"").count(), 1, "{html}");
    }

    #[test]
    fn the_sent_folder_is_a_mailbox_that_keeps_the_other_filters() {
        let mut view = page(ViewerRole::Owner, Vec::new());
        view.channel_filter = Some("sms".into());
        let html = list_html(&view);
        assert!(html.contains(r#"href="/app/inbox?channel=sms&#38;sent=true""#), "{html}");
        assert!(html.contains("<h1>Inbox</h1>"), "{html}");

        view.sent_only = true;
        let html = list_html(&view);
        assert!(html.contains("<h1>Sent</h1>"), "{html}");
        assert!(
            html.contains(
                r#"inbox-rail__link--active" href="/app/inbox?channel=sms&#38;sent=true" aria-current="true">Sent</a>"#
            ),
            "{html}"
        );
        // Inbox drops only the Sent filter.
        assert!(html.contains(r#"href="/app/inbox?channel=sms">Inbox</a>"#), "{html}");
    }

    #[test]
    fn the_reading_pane_assigns_to_a_project_and_returns_to_the_inbox() {
        let page = base_thread();
        let html = dioxus_ssr::render_element(inbox_pane_body(&page, "/app/inbox/c1"));
        assert!(html.contains(r#"action="/app/owner/inbox/c1/assign""#), "{html}");
        assert!(html.contains(r#"name="next" value="inbox""#), "{html}");
        assert!(html.contains(r#"<label for="inbox-assign-project""#), "{html}");
        assert!(html.contains(r#"<option value="project-1">Alpha (alpha)</option>"#), "{html}");
        assert!(html.contains("Assign to project"), "{html}");

        let mut assigned = base_thread();
        assigned.assignee = "Alpha".into();
        let html = dioxus_ssr::render_element(inbox_pane_body(&assigned, "/app/inbox/c1"));
        assert!(html.contains("Move to project"), "{html}");

        let mut no_projects = base_thread();
        no_projects.projects.clear();
        let html = dioxus_ssr::render_element(inbox_pane_body(&no_projects, "/app/inbox/c1"));
        assert!(!html.contains("inbox-assign"), "{html}");
    }

    #[test]
    fn a_message_card_names_its_parties_and_draws_images_full_width() {
        let mut page = base_thread();
        page.messages[0].occurred_label = occurred_label("2026-11-06T22:29:11.5+00:00");
        page.messages[0].attachments = vec![
            InboxAttachmentView {
                filename: Some("photo.jpg".into()),
                kind: None,
                content_type: Some("image/jpeg".into()),
                href: "/app/inbox/c1/messages/m1/attachments/a1".into(),
            },
            InboxAttachmentView {
                filename: Some("retainer.pdf".into()),
                kind: None,
                content_type: Some("application/pdf".into()),
                href: "/app/inbox/c1/messages/m1/attachments/a2".into(),
            },
        ];
        page.messages.push(InboxMessageView {
            id: "m2".into(),
            direction: "outbound".into(),
            body: "Thanks, received.".into(),
            delivery: "submitted".into(),
            internal: false,
            occurred_at: String::new(),
            occurred_label: String::new(),
            automated: true,
            provider_message_id: None,
            visibility: None,
            attachments: Vec::new(),
        });
        let html = thread(&page);
        assert!(html.contains("Nov 6, 2026, 22:29 UTC"), "{html}");
        // Inbound: from the counterparty, to the receiving account.
        assert!(html.contains(r#"<p class="inbox-message__from">sender@example.com</p>"#), "{html}");
        assert!(html.contains("to support@example.com"), "{html}");
        // An automatic acknowledgement is labelled as one.
        assert!(html.contains("Practice (automatic)"), "{html}");
        assert!(html.contains(r#"<img src="/app/inbox/c1/messages/m1/attachments/a1""#), "{html}");
        assert!(!html.contains(r#"<img src="/app/inbox/c1/messages/m1/attachments/a2""#), "{html}");
        assert!(html.contains("retainer.pdf"), "{html}");
    }

    #[test]
    fn only_browser_drawable_images_render_inline() {
        let attachment = |filename: &str, content_type: Option<&str>| InboxAttachmentView {
            filename: Some(filename.into()),
            kind: None,
            content_type: content_type.map(str::to_string),
            href: String::new(),
        };
        assert!(is_inline_image(&attachment("a.bin", Some("image/png"))));
        assert!(is_inline_image(&attachment("IMG.JPG", None)));
        assert!(!is_inline_image(&attachment("a.svg", Some("image/svg+xml"))));
        assert!(!is_inline_image(&attachment("a.heic", Some("image/heic"))));
        assert!(!is_inline_image(&attachment("a.png", Some("application/pdf"))));
        assert_eq!(occurred_label("not a time"), "not a time");
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
    fn the_transcript_renders_attachments_under_their_message() {
        let mut page = base_thread();
        page.messages[0].attachments = vec![
            InboxAttachmentView {
                filename: Some("exhibit-a.pdf".into()),
                kind: Some("pdf".into()),
                content_type: None,
                href: "/app/projects/24-1043/documents/9f2c".into(),
            },
            InboxAttachmentView {
                filename: None,
                kind: None,
                content_type: None,
                href: "/app/projects/24-1043/documents/aaa1".into(),
            },
        ];
        let html = thread(&page);
        assert!(
            html.contains("href=\"/app/projects/24-1043/documents/9f2c\""),
            "{html}"
        );
        assert!(html.contains("exhibit-a.pdf (pdf)"), "{html}");
        assert!(html.contains("Untitled attachment"), "{html}");

        // A message with nothing filed renders no attachment list.
        page.messages[0].attachments = Vec::new();
        let html = thread(&page);
        assert!(!html.contains("inbox-message-attachments"), "{html}");
    }

    #[test]
    fn thread_attachments_dedupe_the_first_slot_and_keep_list_order() {
        let project_id = uuid::Uuid::from_u128(10);
        let primary_id = uuid::Uuid::nil();
        let filed_id = uuid::Uuid::from_u128(2);
        let asset = |id, filename: &str| store::assets::Asset {
            id,
            filename: Some(filename.into()),
            kind: Some("pdf".into()),
            project_id: Some(project_id),
            ..asset_fixture()
        };
        let views = thread_attachment_views(
            "sample-litigation",
            project_id,
            uuid::Uuid::from_u128(11),
            uuid::Uuid::from_u128(12),
            Some(asset(primary_id, "exhibit-a.pdf")),
            vec![
                asset(primary_id, "exhibit-a.pdf"),
                asset(filed_id, "exhibit-b.pdf"),
            ],
            Vec::new(),
        );
        assert_eq!(views.len(), 2, "the first attachment occupies both slots");
        assert_eq!(views[0].filename.as_deref(), Some("exhibit-a.pdf"));
        assert_eq!(views[1].filename.as_deref(), Some("exhibit-b.pdf"));
        assert_eq!(
            views[1].href,
            format!("/app/projects/sample-litigation/documents/{filed_id}")
        );
        assert_eq!(
            attachment_label(&views[0]),
            "exhibit-a.pdf (pdf)".to_string()
        );
    }

    #[test]
    fn bare_inbox_attachments_keep_provider_names_order_and_download_scope() {
        let project = uuid::Uuid::from_u128(10);
        let conversation = uuid::Uuid::from_u128(11);
        let message = uuid::Uuid::from_u128(12);
        let links = [(1_u128, 0_i64, "first.txt"), (2, 1, "second.txt")]
            .into_iter()
            .map(|(id, position, name)| store::inbox::MessageAttachment {
                asset_id: uuid::Uuid::from_u128(id),
                filename: name.into(),
                position,
                sha256: String::new(),
            })
            .collect();
        let views = thread_attachment_views(
            "sample-litigation",
            project,
            conversation,
            message,
            None,
            Vec::new(),
            links,
        );
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].filename.as_deref(), Some("first.txt"));
        assert_eq!(views[1].filename.as_deref(), Some("second.txt"));
        assert_eq!(views[1].href, format!("/app/api/projects/{project}/inbox/{conversation}/messages/{message}/attachments/{}", uuid::Uuid::from_u128(2)));
    }

    fn asset_fixture() -> store::assets::Asset {
        // Only the fields thread_attachment_views reads are load-bearing;
        // the struct is a plain record, so the rest holds neutral values.
        serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::nil(),
            "storage_key": "assets/test",
            "content_type": "application/pdf",
            "byte_size": 1,
            "sha256_hex": "abc",
            "visibility": "internal",
            "notarization": [],
            "inserted_at": "2026-10-30T00:00:00Z",
            "updated_at": "2026-10-30T00:00:00Z"
        }))
        .expect("fixture asset")
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
            direction: store::inbox::direction::INBOUND.into(),
            has_attachments: false,
            sent: false,
            preview: None,
            person: None,
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
        let html = list_html(&page(
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
                direction: store::inbox::direction::INBOUND.into(),
                has_attachments: false,
                sent: false,
                preview: None,
                person: None,
            }],
        ));
        assert!(html.contains("<table"), "{html}");
        assert!(html.contains("inbox-table__row--unread"), "{html}");
        for cell in [
            "Pat Client",
            "Question",
            "sms",
            "neon",
            "Unassigned",
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
            occurred_label: String::new(),
            automated: false,
            provider_message_id: None,
            visibility: None,
            attachments: Vec::new(),
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
            occurred_label: String::new(),
            automated: false,
            provider_message_id: None,
            visibility: Some("private".into()),
            attachments: Vec::new(),
        });
        let html = thread(&page);
        assert!(html.contains("Reconcile delivery"), "{html}");
        assert!(html.contains("email · unknown"), "{html}");
        assert!(
            html.contains(r#"action="/app/projects/alpha/inbox/c1/replies""#),
            "{html}"
        );
        assert!(html.contains("We received your message."), "{html}");
        assert!(html.contains("inbox-message--outbound"), "{html}");
        assert!(html.contains("unknown · Private"), "{html}");
    }

    #[test]
    fn social_thread_discloses_public_reply_audience() {
        let mut page = base_thread();
        page.channel = "instagram".into();
        page.messages[0].visibility = Some("public".into());
        let html = thread(&page);
        assert!(html.contains("instagram · received · Public"), "{html}");
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
                assert!(html.contains("facebook · received · Private"), "{html}");
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
        let html = list_html(&page);
        assert!(html.contains("Choose a project first."), "{html}");
        // The filters live in the folder rail; the active filter is marked on
        // its own link with aria-current rather than narrated beside the row.
        assert!(
            html.contains("<aside class=\"inbox-rail\" aria-label=\"Inbox folders\">"),
            "{html}"
        );
        // Five filter groups plus the Inbox mailbox.
        assert_eq!(html.matches("aria-current=\"true\"").count(), 6, "{html}");
        assert!(
            html.contains(
                "inbox-rail__link--active\" href=\"/app/inbox?channel=instagram&#38;brand=neon\" aria-current=\"true\">instagram</a>"
            ),
            "{html}"
        );
        assert!(
            html.contains(
                "inbox-rail__link--active\" href=\"/app/inbox?channel=instagram&#38;brand=neon\" aria-current=\"true\">neon</a>"
            ),
            "{html}"
        );
        assert!(html.contains("No conversations yet."), "{html}");
    }

    #[cfg(feature = "server")]
    mod loaders {
        use super::super::*;
        use crate::inbox::server_fn_test_support::{call, request_parts, runtime, Viewer};
        use store::persons::{NewPerson, Role};

        struct World {
            surreal: store::surreal::SurrealDb,
            owner: uuid::Uuid,
            lawyer: uuid::Uuid,
            project_code: String,
            assigned: uuid::Uuid,
            intake: uuid::Uuid,
        }

        async fn person(db: &store::surreal::SurrealDb, label: &str, role: Role) -> uuid::Uuid {
            store::persons::create(
                db,
                &NewPerson::with_role(
                    format!("{label} Person"),
                    format!("{label}-{}@example.com", uuid::Uuid::now_v7()),
                    role,
                ),
            )
            .await
            .unwrap()
            .id
        }

        fn inbound<'a>(
            firm: uuid::Uuid,
            channel: &'a str,
            provider_message_id: &'a str,
            address: &'a str,
            subject: Option<&'a str>,
        ) -> store::inbox::IngestInbound<'a> {
            store::inbox::IngestInbound {
                project_id: None,
                firm_id: firm,
                brand_key: Some("neon"),
                interaction_type: None,
                provider_post_id: None,
                parent_provider_message_id: None,
                visibility: None,
                channel,
                provider_account: "support@example.com",
                provider_message_id,
                provider_thread_ref: None,
                external_address: address,
                external_name: Some("Sender Name"),
                subject,
                body: "Hello from outside.",
                source_page: Some("/contact"),
                source_channel: Some(channel),
                matter_consent: true,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachments: Vec::new(),
                occurred_at: "2026-09-27T00:00:00Z",
                raw_storage_key: None,
            }
        }

        /// A Firm with an Owner and a participating Lawyer, one Project, an
        /// email conversation assigned to that Project (carrying two linked
        /// attachments), and a text conversation still in intake.
        async fn world() -> World {
            let surreal = store::test_support::mem_surreal().await;
            let owner = person(&surreal, "owner", Role::Owner).await;
            let admin = person(&surreal, "admin", Role::Admin).await;
            let lawyer = person(&surreal, "lawyer", Role::Lawyer).await;
            let firm = store::firms::create(
                &surreal,
                &store::firms::NewFirm {
                    name: "Inbox Loader Practice".into(),
                    status: "active".into(),
                    entity_id: store::test_support::seed_entity(&surreal).await,
                    admin_dri_person_id: admin,
                },
            )
            .await
            .unwrap()
            .id;
            store::firms::ensure_membership(
                &surreal,
                &store::firms::NewPersonFirmRole {
                    person_id: lawyer,
                    firm_id: firm,
                    membership: store::firms::FirmMembership::Lawyer,
                    is_dri: false,
                },
            )
            .await
            .unwrap();
            let project_code = "inbox-loader-matter".to_string();
            let project = store::projects::create(
                &surreal,
                &store::projects::NewProject {
                    code: project_code.clone(),
                    name: "Inbox Loader Matter".into(),
                    status: "open".into(),
                    entity_id: store::test_support::seed_entity(&surreal).await,
                    firm_id: Some(firm),
                    ..store::projects::NewProject::default()
                },
            )
            .await
            .unwrap()
            .id;
            store::projects::designate_dri_in_surreal(
                &surreal,
                project,
                lawyer,
                store::projects::DriSide::Lawyer,
            )
            .await
            .unwrap();

            let mut email = inbound(
                firm,
                store::inbox::channel::EMAIL,
                "loader-email-1",
                "client@example.com",
                Some("Notice of hearing"),
            );
            email.attachments = vec![
                store::inbox::IngestAttachment {
                    asset_id: uuid::Uuid::now_v7(),
                    filename: "notice.pdf".into(),
                    sha256: "sha-notice".into(),
                },
                store::inbox::IngestAttachment {
                    asset_id: uuid::Uuid::now_v7(),
                    filename: "exhibit.pdf".into(),
                    sha256: "sha-exhibit".into(),
                },
            ];
            let assigned = store::inbox::ingest_inbound(&surreal, &email)
                .await
                .unwrap()
                .conversation_id;
            store::inbox::assign(&surreal, owner, assigned, project)
                .await
                .unwrap();
            let intake = store::inbox::ingest_inbound(
                &surreal,
                &inbound(
                    firm,
                    store::inbox::channel::SMS,
                    "loader-sms-1",
                    "+15555550123",
                    None,
                ),
            )
            .await
            .unwrap()
            .conversation_id;
            World {
                surreal,
                owner,
                lawyer,
                project_code,
                assigned,
                intake,
            }
        }

        fn load<T, F: std::future::Future<Output = Result<T, ServerFnError>>>(
            rt: &tokio::runtime::Runtime,
            world: &World,
            route: &str,
            uri: &str,
            viewer: &Viewer,
            loader: impl FnOnce() -> F,
        ) -> Result<T, ServerFnError> {
            let parts = request_parts(rt, route, uri, viewer);
            call(
                rt,
                parts,
                |dom| dom.provide_root_context(world.surreal.clone()),
                loader,
            )
            .0
        }

        #[test]
        fn the_firm_list_reads_every_conversation_and_honors_its_filters() {
            let rt = runtime();
            let world = rt.block_on(world());
            let owner = Viewer::new(ViewerRole::Owner, Some(world.owner));

            let page = load(
                &rt,
                &world,
                INBOX_PATH,
                INBOX_PATH,
                &owner,
                owner_inbox_page,
            )
            .unwrap();
            assert_eq!(page.title, "Inbox");
            assert_eq!(page.base_href, INBOX_PATH);
            assert_eq!(
                page.rows.len(),
                2,
                "both conversations are in the Firm list"
            );
            assert_eq!(page.brands, vec!["neon".to_string()]);
            assert_eq!(page.channels, inbox_channels());
            assert_eq!(page.csrf_token, "CSRF");
            let text = page
                .rows
                .iter()
                .find(|row| row.id == world.intake.to_string())
                .unwrap();
            assert_eq!(text.subject, "(no subject)");
            assert_eq!(text.sender, "Sender Name");
            assert_eq!(text.href, format!("/app/inbox/{}", world.intake));
            assert!(!text.updated_iso.is_empty());

            let uri = format!(
                "{INBOX_PATH}?channel=email&brand=neon&unread=true&direction=inbound&attachments=true&error=Choose%20a%20project"
            );
            let page = load(&rt, &world, INBOX_PATH, &uri, &owner, owner_inbox_page).unwrap();
            assert_eq!(page.channel_filter.as_deref(), Some("email"));
            assert_eq!(page.brand_filter.as_deref(), Some("neon"));
            assert_eq!(page.direction_filter.as_deref(), Some("inbound"));
            assert!(page.unread_only && page.attachments_only);
            assert_eq!(page.error.as_deref(), Some("Choose a project"));
            assert_eq!(page.rows.len(), 1);
            assert_eq!(page.rows[0].id, world.assigned.to_string());
            assert!(page.rows[0].has_attachments);

            // Values the filter strip never offers are dropped, not applied.
            let uri = format!("{INBOX_PATH}?channel=fax&brand=vesta&direction=sideways&brand=%20");
            let page = load(&rt, &world, INBOX_PATH, &uri, &owner, owner_inbox_page).unwrap();
            assert_eq!(page.channel_filter, None);
            assert_eq!(page.brand_filter, None);
            assert_eq!(page.direction_filter, None);
            assert_eq!(page.rows.len(), 2);
        }

        #[test]
        fn the_firm_list_refuses_a_client_and_an_unlinked_session() {
            let rt = runtime();
            let world = rt.block_on(world());
            for viewer in [
                Viewer::new(ViewerRole::Client, Some(world.owner)),
                Viewer::new(ViewerRole::Owner, None),
            ] {
                assert!(load(
                    &rt,
                    &world,
                    INBOX_PATH,
                    INBOX_PATH,
                    &viewer,
                    owner_inbox_page
                )
                .is_err());
            }
        }

        #[test]
        fn the_firm_thread_shows_the_transcript_and_the_assign_control() {
            let rt = runtime();
            let world = rt.block_on(world());
            let owner = Viewer::new(ViewerRole::Owner, Some(world.owner));

            let page = load(
                &rt,
                &world,
                INBOX_THREAD_PATH,
                &format!("/app/inbox/{}", world.assigned),
                &owner,
                owner_inbox_thread,
            )
            .unwrap();
            assert_eq!(page.title, "Notice of hearing");
            assert_eq!(page.assignee, "Inbox Loader Matter");
            assert_eq!(page.brand_key.as_deref(), Some("neon"));
            assert_eq!(page.provider_account, "support@example.com");
            // A1 resolves the thread's sender through the association
            // persisted at ingest — an unknown email sender auto-created a
            // client-tier Person — so the label carries the role suffix,
            // matching every other conversation surface.
            assert_eq!(page.sender, "Sender Name (client)");
            assert_eq!(page.channel, "email");
            assert_eq!(page.delivery, store::inbox::delivery::RECEIVED);
            assert_eq!(
                page.assign_action,
                Some(format!("/app/inbox/{}/assign", world.assigned))
            );
            assert!(
                !page.can_attribute,
                "an Owner assigns rather than attributes"
            );
            assert!(page
                .projects
                .iter()
                .any(|project| project.label == "Inbox Loader Matter (inbox-loader-matter)"));
            if page.bodies_visible {
                assert_eq!(page.messages.len(), 1);
                assert_eq!(page.messages[0].body, "Hello from outside.");
                assert_eq!(
                    page.messages[0].provider_message_id.as_deref(),
                    Some("loader-email-1")
                );
                assert!(!page.messages[0].internal);
            }

            let page = load(
                &rt,
                &world,
                INBOX_THREAD_PATH,
                &format!("/app/inbox/{}", world.intake),
                &owner,
                owner_inbox_thread,
            )
            .unwrap();
            assert_eq!(
                page.title, "+15555550123",
                "no subject falls back to the address"
            );
            assert_eq!(page.assignee, "Unassigned");

            assert!(load(
                &rt,
                &world,
                INBOX_THREAD_PATH,
                "/app/inbox/not-a-conversation",
                &owner,
                owner_inbox_thread,
            )
            .is_err());
        }

        #[test]
        fn the_project_list_and_thread_read_the_assigned_conversation() {
            let rt = runtime();
            let world = rt.block_on(world());
            let lawyer = Viewer::new(ViewerRole::Lawyer, Some(world.lawyer));
            let code = &world.project_code;

            let page = load(
                &rt,
                &world,
                PROJECT_INBOX_PATH,
                &format!("/app/projects/{code}/inbox?brand=neon"),
                &lawyer,
                project_inbox_page,
            )
            .unwrap();
            assert_eq!(page.title, "Inbox Loader Matter inbox");
            assert_eq!(page.base_href, format!("/app/projects/{code}/inbox"));
            assert_eq!(page.brands, vec!["neon".to_string()]);
            assert_eq!(page.brand_filter.as_deref(), Some("neon"));
            assert_eq!(page.compose_action, None);
            assert_eq!(page.rows.len(), 1, "only the assigned conversation");
            assert_eq!(
                page.rows[0].href,
                format!("/app/projects/{code}/inbox/{}", world.assigned)
            );

            let page = load(
                &rt,
                &world,
                PROJECT_INBOX_THREAD_PATH,
                &format!("/app/projects/{code}/inbox/{}", world.assigned),
                &lawyer,
                project_inbox_thread,
            )
            .unwrap();
            assert_eq!(page.title, "Notice of hearing");
            assert!(page.bodies_visible);
            assert_eq!(page.sender, "Sender Name");
            assert_eq!(page.messages.len(), 1);
            let attachments = &page.messages[0].attachments;
            assert_eq!(attachments.len(), 2, "{:?}", attachments.len());
            assert_eq!(attachments[0].filename.as_deref(), Some("notice.pdf"));
            assert_eq!(attachments[1].filename.as_deref(), Some("exhibit.pdf"));
            assert!(
                attachments[0].href.starts_with("/app/api/projects/"),
                "{}",
                attachments[0].href
            );
            if let Some(note) = page.note_action.as_deref() {
                assert_eq!(
                    note,
                    format!("/app/projects/{code}/inbox/{}/notes", world.assigned)
                );
            }
        }

        #[test]
        fn the_project_pages_refuse_an_unknown_matter_and_a_bad_path() {
            let rt = runtime();
            let world = rt.block_on(world());
            let lawyer = Viewer::new(ViewerRole::Lawyer, Some(world.lawyer));
            assert!(load(
                &rt,
                &world,
                PROJECT_INBOX_PATH,
                "/app/projects/no-such-matter/inbox",
                &lawyer,
                project_inbox_page,
            )
            .is_err());
            assert!(load(
                &rt,
                &world,
                PROJECT_INBOX_THREAD_PATH,
                &format!("/app/projects/{}/inbox/not-a-uuid", world.project_code),
                &lawyer,
                project_inbox_thread,
            )
            .is_err());
            assert!(load(
                &rt,
                &world,
                PROJECT_INBOX_THREAD_PATH,
                &format!("/app/projects/no-such-matter/inbox/{}", world.assigned),
                &lawyer,
                project_inbox_thread,
            )
            .is_err());
            assert!(load(
                &rt,
                &world,
                PROJECT_INBOX_PATH,
                &format!("/app/projects/{}/inbox", world.project_code),
                &Viewer::new(ViewerRole::Lawyer, None),
                project_inbox_page,
            )
            .is_err());
        }

        #[test]
        fn intake_brands_are_sorted_and_deduplicated() {
            let row = |brand: Option<&str>| store::inbox::IntakeRow {
                contact: None,
                id: uuid::Uuid::now_v7(),
                firm_id: uuid::Uuid::nil(),
                brand_key: brand.map(str::to_string),
                project_id: None,
                channel: "email".into(),
                assignee: "Unassigned".into(),
                unread: false,
                delivery: "received".into(),
                next_action: "Assign".into(),
                external_address: "a@example.com".into(),
                external_name: Some(" ".into()),
                subject: Some("  ".into()),
                direction: store::inbox::direction::INBOUND.into(),
                has_attachments: false,
                sent: false,
                preview: None,
                person: None,
                updated_at: chrono::Utc::now(),
            };
            let rows = [
                row(Some("vesta")),
                row(None),
                row(Some("neon")),
                row(Some("vesta")),
            ];
            assert_eq!(
                inbox_brands(&rows),
                vec!["neon".to_string(), "vesta".to_string()]
            );
            // A blank name and subject fall back to the address and a placeholder.
            let view = row_view(&rows[1], "/x".into());
            assert_eq!(view.sender, "a@example.com");
            assert_eq!(view.subject, "(no subject)");
        }
    }
}

/// Drives a `#[server]` loader the way the portal router does, without a
/// router-mounted render: the request parts carry the extensions the auth
/// layer injects (viewer role, person id, CSRF token) and the path parameters
/// a matched route records, and the loader runs inside a Dioxus scope that
/// provides the store handles the portal's `ServeConfig` would.
#[cfg(all(test, feature = "server"))]
pub(crate) mod server_fn_test_support {
    use std::future::Future;
    use std::sync::{Arc, Mutex};

    use dioxus::prelude::*;
    use dioxus_fullstack_core::FullstackContext;

    use crate::people::ViewerRole;

    /// Who the request is from, as the auth layer would inject it.
    #[derive(Clone, Default)]
    pub(crate) struct Viewer {
        pub role: Option<ViewerRole>,
        pub person_id: Option<uuid::Uuid>,
        pub csrf_token: Option<String>,
    }

    impl Viewer {
        pub(crate) fn new(role: ViewerRole, person_id: Option<uuid::Uuid>) -> Self {
            Self {
                role: Some(role),
                person_id,
                csrf_token: Some("CSRF".to_string()),
            }
        }
    }

    /// A multi-threaded runtime, so the embedded store's own tasks keep
    /// running while a loader is polled on the test thread.
    pub(crate) fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("test runtime")
    }

    /// The request parts a router hands its handler for `uri` matched against
    /// `route`, so `axum::extract::Path` resolves inside the loader.
    pub(crate) fn request_parts(
        rt: &tokio::runtime::Runtime,
        route: &str,
        uri: &str,
        viewer: &Viewer,
    ) -> axum::http::request::Parts {
        let slot: Arc<Mutex<Option<axum::http::request::Parts>>> = Arc::default();
        let captured = slot.clone();
        let router = axum::Router::new().route(
            route,
            axum::routing::get(move |request: axum::extract::Request| {
                let captured = captured.clone();
                async move {
                    *captured.lock().expect("slot") = Some(request.into_parts().0);
                    axum::http::StatusCode::OK
                }
            }),
        );
        let mut request = axum::http::Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .expect("request");
        let extensions = request.extensions_mut();
        if let Some(role) = viewer.role {
            extensions.insert(role);
        }
        extensions.insert(crate::portal_project_list::PersonId(
            viewer.person_id.map(|id| id.to_string()),
        ));
        if let Some(token) = viewer.csrf_token.clone() {
            extensions.insert(crate::csrf::CsrfToken(token));
        }
        let response = rt.block_on(axum::handler::Handler::call(
            axum::routing::any_service(router),
            request,
            (),
        ));
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "`{uri}` does not match `{route}`"
        );
        let parts = slot.lock().expect("slot").take();
        parts.expect("the route captured its request")
    }

    /// Run `loader` inside a Dioxus scope whose root context `provide` fills,
    /// under a request context built from `parts`. Returns the loader's
    /// result and the HTTP status the loader committed.
    pub(crate) fn call<T, F>(
        rt: &tokio::runtime::Runtime,
        parts: axum::http::request::Parts,
        provide: impl FnOnce(&VirtualDom),
        loader: impl FnOnce() -> F,
    ) -> (T, axum::http::StatusCode)
    where
        F: Future<Output = T>,
    {
        let dom = VirtualDom::new(|| rsx! {});
        provide(&dom);
        let context = FullstackContext::new(parts);
        let result = dom.in_scope(ScopeId::ROOT, || {
            rt.block_on(context.clone().scope(loader()))
        });
        (result, context.current_http_status().status)
    }
}
