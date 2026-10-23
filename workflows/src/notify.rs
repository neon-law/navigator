//! Ops-notification seam — the chat sibling of [`crate::email::EmailService`].
//!
//! Neon Law Navigator's durable workflows prove their liveness by notifying firm ops
//! (the six-hourly `Heartbeat`, the nightly `Archives` digest, the
//! `BillingCanary`, …). That signal goes to an incoming **Slack** webhook on
//! the engineering channel — where engineers already watch — and **no longer
//! also goes out as email**: a recurring liveness signal is trivial to lose in
//! an inbox (the very failure mode that hid a real heartbeat gap once), so once
//! Slack delivery proved reliable the firm dropped the duplicate ops email (the
//! follow-up to the dual-send introduced in PR #13).
//!
//! Two pieces:
//!
//! - [`Notifier`] — the trait, with a real [`SlackNotifier`] (POSTs
//!   `{"text": …}` to an incoming webhook) and a [`CapturingNotifier`] that
//!   keeps messages in memory for KIND/tests so nothing leaves the binary.
//! - [`SlackOpsDelivery`] — an [`EmailService`] adapter that delivers an ops
//!   notice to a [`Notifier`] (Slack) *instead of* sending email. The ops
//!   workflows render their notice as an [`OutboundEmail`] and hand it to their
//!   `EmailService`; wiring them with this adapter routes the notice to Slack
//!   and sends no mail at all.
//!
//! **The load-bearing boundary:** [`SlackOpsDelivery`] must back **only**
//! internal/operations services — `BillingCanary` and `BillingDigest`. (`Archives` and `Heartbeat` are also Slack-only but post
//! to the [`Notifier`] directly — their messages are mrkdwn, so they need the
//! links and glyphs a fenced code block would flatten, not the plain-text
//! framing this adapter renders.) Those carry no client, matter, or PII data
//! (their recipients are env-pinned to firm ops).
//! It must **never** back
//! a client-facing email service (such as `Notation`):
//! pushing client content into a chat channel would cross the firm's trust
//! boundary, violating the standing no-content rule (see the `observability`
//! skill). The boundary is enforced at the wiring point in `workflows-service`'s
//! `main.rs`, not by per-message inspection here.
//!
//! The persistent staging deployment (`NAVIGATOR_SIMULATED_MATTERS=true` on the
//! production profile) appends a final `from Staging` line at the real Slack
//! clients. In-memory capturing backends used by KIND and tests do not.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use thiserror::Error;

use crate::email::service::{EmailError, EmailService, OutboundEmail, SendReceipt};

/// Why a notification failed to deliver. Distinct from [`EmailError`] because a
/// notifier outage must never be conflated with — nor fail — an email send.
#[derive(Debug, Error)]
pub enum NotifyError {
    /// The HTTP request to the webhook never completed (DNS, TLS, timeout).
    #[error("transport error: {0}")]
    Transport(String),
    /// The webhook returned a non-2xx status (e.g. 404 for a revoked webhook).
    #[error("notification endpoint rejected the message: status {0}")]
    Rejected(u16),
}

/// Why a Slack Web API request failed. The bot client is separate from the
/// legacy incoming-webhook notifier because Web API calls choose a channel at
/// request time and therefore can serve one private channel per Project.
#[derive(Debug, Error)]
pub enum SlackBotError {
    /// The request did not complete.
    #[error("Slack API transport error: {0}")]
    Transport(String),
    /// Slack or its edge rejected the HTTP request.
    #[error("Slack API HTTP status {0}")]
    HttpStatus(u16),
    /// Slack or its edge returned a transient server failure.
    #[error("Slack API transient HTTP status {0}")]
    RetryableHttpStatus(u16),
    /// Slack rate-limited the request and supplied an optional retry delay.
    #[error("Slack API rate limited the request")]
    RateLimited { retry_after_seconds: Option<u64> },
    /// Slack returned a structured API error.
    #[error("Slack API rejected the request: {0}")]
    Api(String),
    /// The bot token lacks an OAuth scope the method requires. `needed` is
    /// the scope Slack named and `provided` the scopes the token already
    /// carries, each `unspecified` when Slack named none. Slack's `needed`
    /// for `conversations.list` lists every conversation type's scope, not
    /// only the types requested, so `provided` is what shows the real gap.
    #[error("Slack API rejected {method}: missing_scope (needed: {needed}; provided: {provided})")]
    MissingScope {
        method: String,
        needed: String,
        provided: String,
    },
    /// Slack returned success without the object the caller needs.
    #[error("Slack API returned an incomplete response")]
    IncompleteResponse,
}

/// The subset of a Slack channel response Navigator needs to address later
/// messages. The ID, not the channel name, is the stable posting coordinate.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SlackChannel {
    pub id: String,
    pub name: String,
}

/// Slack's channel-name limit.
const SLACK_CHANNEL_NAME_LIMIT: usize = 80;

/// The private channel name for a Project: `project-<code>` in production and
/// `staging-<code>` on a simulated-matters deployment, so the two workspaces'
/// channels never collide. The code is lowercased and anything Slack rejects
/// becomes a hyphen; the result stays within Slack's 80-character limit.
#[must_use]
pub fn project_channel_name(simulated: bool, code: &str) -> String {
    let prefix = if simulated { "staging" } else { "project" };
    let slug: String = code
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut name = format!("{prefix}-{slug}");
    name.truncate(SLACK_CHANNEL_NAME_LIMIT);
    name
}

/// The provider receipt for a successful `chat.postMessage` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackMessageReceipt {
    pub channel_id: String,
    pub timestamp: String,
    pub correlation_id: Option<String>,
}

/// Target-aware Slack delivery for internal Project channels.
#[async_trait]
pub trait SlackBot: Send + Sync {
    /// Create one private channel named after the Project code.
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError>;
    /// Find a non-archived private channel the bot can see by exact name.
    /// Implementors that cannot list channels report none.
    async fn find_private_channel_by_name(
        &self,
        name: &str,
    ) -> Result<Option<SlackChannel>, SlackBotError> {
        let _ = name;
        Ok(None)
    }
    /// Post a short internal notice to a channel ID.
    async fn post_message(&self, channel_id: &str, text: &str) -> Result<(), SlackBotError>;
    /// Resolve a workspace member by the Person directory's email address.
    /// `None` means Slack has no matching account.
    async fn user_id_by_email(&self, email: &str) -> Result<Option<String>, SlackBotError>;
    /// List every member of a public or private channel.
    async fn channel_member_ids(&self, channel_id: &str) -> Result<Vec<String>, SlackBotError>;
    /// Invite users to a public or private channel. Callers omit current members.
    async fn invite_to_channel(
        &self,
        channel_id: &str,
        user_ids: &[String],
    ) -> Result<(), SlackBotError>;
    /// Post a message and retain Slack's delivery coordinates. Implementors
    /// that only support the legacy operation receive an explicit empty
    /// receipt, so existing operational callers remain unchanged.
    async fn post_message_with_receipt(
        &self,
        channel_id: &str,
        text: &str,
        correlation_id: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        self.post_message(channel_id, text).await?;
        Ok(SlackMessageReceipt {
            channel_id: channel_id.to_string(),
            timestamp: String::new(),
            correlation_id: correlation_id.map(str::to_string),
        })
    }
    /// Look up a correlated notice without sending it again. Absence is not
    /// proof of non-delivery; callers retain an unknown result.
    async fn message_receipt_by_correlation(
        &self,
        channel_id: &str,
        correlation_id: &str,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        let _ = (channel_id, correlation_id);
        Ok(None)
    }
    /// Post `text` as a reply in the thread whose parent message has
    /// timestamp `thread_ts`. Implementors without threads post it flat.
    async fn post_thread_reply(
        &self,
        channel_id: &str,
        thread_ts: &str,
        text: &str,
    ) -> Result<(), SlackBotError> {
        let _ = thread_ts;
        self.post_message(channel_id, text).await
    }
    /// The id of the canvas attached to a channel, or `None` when it has none.
    /// Implementors without canvases report the capability as missing.
    async fn channel_canvas_id(&self, channel_id: &str) -> Result<Option<String>, SlackBotError> {
        let _ = channel_id;
        Err(SlackBotError::Api(CANVAS_UNSUPPORTED.to_string()))
    }
    /// Create a channel's canvas from markdown and return its id. A channel
    /// holds at most one; Slack rejects a second with
    /// [`CHANNEL_CANVAS_EXISTS`].
    async fn create_channel_canvas(
        &self,
        channel_id: &str,
        markdown: &str,
    ) -> Result<String, SlackBotError> {
        let _ = (channel_id, markdown);
        Err(SlackBotError::Api(CANVAS_UNSUPPORTED.to_string()))
    }
    /// Replace the whole body of an existing canvas with markdown.
    async fn replace_canvas(&self, canvas_id: &str, markdown: &str) -> Result<(), SlackBotError> {
        let _ = (canvas_id, markdown);
        Err(SlackBotError::Api(CANVAS_UNSUPPORTED.to_string()))
    }
    /// Archive a channel the bot is a member of. `Ok(true)` when this call
    /// archived it; `Ok(false)` when it was already archived or Slack no
    /// longer knows it, so a nightly re-run is a no-op.
    async fn archive_channel(&self, channel_id: &str) -> Result<bool, SlackBotError> {
        let _ = channel_id;
        Err(SlackBotError::Api(ARCHIVE_UNSUPPORTED.to_string()))
    }
}

/// The error code an implementor that cannot archive channels returns.
pub const ARCHIVE_UNSUPPORTED: &str = "archive_unsupported";
/// The error code an implementor without canvases returns.
pub const CANVAS_UNSUPPORTED: &str = "canvas_unsupported";
/// Slack's error code for creating a second canvas on one channel.
pub const CHANNEL_CANVAS_EXISTS: &str = "channel_canvas_already_exists";

/// What [`upsert_channel_canvas`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanvasWrite {
    Created(String),
    Edited(String),
}

/// Write `markdown` as the channel's one canvas: edit it in place when the
/// channel has one, create it otherwise. A second call never stacks a second
/// canvas, including when another writer creates one between the lookup and
/// the create.
pub async fn upsert_channel_canvas(
    bot: &dyn SlackBot,
    channel_id: &str,
    markdown: &str,
) -> Result<CanvasWrite, SlackBotError> {
    if let Some(canvas_id) = bot.channel_canvas_id(channel_id).await? {
        bot.replace_canvas(&canvas_id, markdown).await?;
        return Ok(CanvasWrite::Edited(canvas_id));
    }
    match bot.create_channel_canvas(channel_id, markdown).await {
        Ok(canvas_id) => Ok(CanvasWrite::Created(canvas_id)),
        Err(SlackBotError::Api(code)) if code == CHANNEL_CANVAS_EXISTS => {
            let canvas_id = bot
                .channel_canvas_id(channel_id)
                .await?
                .ok_or(SlackBotError::IncompleteResponse)?;
            bot.replace_canvas(&canvas_id, markdown).await?;
            Ok(CanvasWrite::Edited(canvas_id))
        }
        Err(error) => Err(error),
    }
}

/// Slack Web API client authenticated with a bot token. The token is held only
/// in the HTTP client's request path and is never included in errors or logs.
#[derive(Clone)]
pub struct SlackBotClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
    from_staging: bool,
}

impl SlackBotClient {
    /// The production Slack Web API origin.
    pub const DEFAULT_BASE_URL: &'static str = "https://slack.com/api";

    /// Build a client for Slack's Web API. Staging labeling follows
    /// [`cloud::labels_slack_from_staging_env`].
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self::from_parts(
            token,
            Self::DEFAULT_BASE_URL,
            cloud::labels_slack_from_staging_env(),
        )
    }

    /// Build a client against an explicit origin for tests. Process staging
    /// labels are not applied, so a local simulated-matters shell cannot
    /// rewrite the asserted request body.
    #[must_use]
    pub fn with_base_url(token: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::from_parts(token, base_url, false)
    }

    #[cfg(test)]
    fn with_staging_origin(
        token: impl Into<String>,
        base_url: impl Into<String>,
        from_staging: bool,
    ) -> Self {
        Self::from_parts(token, base_url, from_staging)
    }

    fn from_parts(
        token: impl Into<String>,
        base_url: impl Into<String>,
        from_staging: bool,
    ) -> Self {
        let http = match reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
        {
            Ok(client) => client,
            Err(_) => reqwest::Client::new(),
        };
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            from_staging,
        }
    }

    async fn post<T: serde::Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        body: &T,
    ) -> Result<R, SlackBotError> {
        let response = self
            .http
            .post(format!("{}/{method}", self.base_url))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|error| SlackBotError::Transport(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            if status.as_u16() == 429 {
                let retry_after_seconds = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok());
                return Err(SlackBotError::RateLimited {
                    retry_after_seconds,
                });
            }
            if status.is_server_error() {
                return Err(SlackBotError::RetryableHttpStatus(status.as_u16()));
            }
            return Err(SlackBotError::HttpStatus(status.as_u16()));
        }
        decode(method, response).await
    }

    async fn get<T: serde::Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        query: &T,
    ) -> Result<R, SlackBotError> {
        let response = self
            .http
            .get(format!("{}/{method}", self.base_url))
            .bearer_auth(&self.token)
            .query(query)
            .send()
            .await
            .map_err(|error| SlackBotError::Transport(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            if status.as_u16() == 429 {
                let retry_after_seconds = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok());
                return Err(SlackBotError::RateLimited {
                    retry_after_seconds,
                });
            }
            if status.is_server_error() {
                return Err(SlackBotError::RetryableHttpStatus(status.as_u16()));
            }
            return Err(SlackBotError::HttpStatus(status.as_u16()));
        }
        decode(method, response).await
    }
}

#[derive(Debug, Deserialize)]
struct CreateChannelResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    channel: Option<SlackChannel>,
}

#[derive(Debug, Deserialize)]
struct PostMessageResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LookupUserResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    user: Option<SlackUser>,
}

#[derive(Debug, Deserialize)]
struct SlackUser {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ChannelMembersResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    response_metadata: SlackResponseMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct SlackResponseMetadata {
    #[serde(default)]
    next_cursor: String,
}

#[derive(Debug, Deserialize)]
struct ListChannelsResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    channels: Vec<SlackChannel>,
    #[serde(default)]
    response_metadata: SlackResponseMetadata,
}

#[derive(Debug, Deserialize)]
struct ChannelInfoResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    channel: Option<ChannelInfo>,
}

#[derive(Debug, Deserialize)]
struct ChannelInfo {
    #[serde(default)]
    properties: Option<ChannelProperties>,
}

#[derive(Debug, Deserialize)]
struct ChannelProperties {
    #[serde(default)]
    canvas: Option<ChannelCanvas>,
}

#[derive(Debug, Deserialize)]
struct ChannelCanvas {
    #[serde(default)]
    file_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CanvasCreateResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    canvas_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InviteResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

#[async_trait]
impl SlackBot for SlackBotClient {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        let response: CreateChannelResponse = self
            .post(
                "conversations.create",
                &json!({ "name": name, "is_private": true }),
            )
            .await?;
        if !response.ok {
            return Err(SlackBotError::Api(
                response
                    .error
                    .unwrap_or_else(|| "unknown_error".to_string()),
            ));
        }
        response.channel.ok_or(SlackBotError::IncompleteResponse)
    }

    async fn find_private_channel_by_name(
        &self,
        name: &str,
    ) -> Result<Option<SlackChannel>, SlackBotError> {
        let mut cursor = String::new();
        loop {
            let mut query = json!({
                "types": "private_channel",
                "exclude_archived": true,
                "limit": 200,
            });
            if !cursor.is_empty() {
                query["cursor"] = json!(cursor);
            }
            let response: ListChannelsResponse = self.get("conversations.list", &query).await?;
            if !response.ok {
                return Err(SlackBotError::Api(
                    response
                        .error
                        .unwrap_or_else(|| "unknown_error".to_string()),
                ));
            }
            if let Some(found) = response
                .channels
                .into_iter()
                .find(|channel| channel.name == name)
            {
                return Ok(Some(found));
            }
            cursor = response.response_metadata.next_cursor;
            if cursor.is_empty() {
                return Ok(None);
            }
        }
    }

    async fn post_message(&self, channel_id: &str, text: &str) -> Result<(), SlackBotError> {
        let text = cloud::outbound_slack_text(text, self.from_staging);
        let response: PostMessageResponse = self
            .post(
                "chat.postMessage",
                &json!({
                    "channel": channel_id,
                    "text": text,
                    "unfurl_links": false,
                    "unfurl_media": false,
                }),
            )
            .await?;
        if response.ok {
            Ok(())
        } else {
            Err(SlackBotError::Api(
                response
                    .error
                    .unwrap_or_else(|| "unknown_error".to_string()),
            ))
        }
    }

    async fn user_id_by_email(&self, email: &str) -> Result<Option<String>, SlackBotError> {
        let response: LookupUserResponse = self
            .get("users.lookupByEmail", &json!({ "email": email }))
            .await?;
        if response.ok {
            return response
                .user
                .map(|user| Some(user.id))
                .ok_or(SlackBotError::IncompleteResponse);
        }
        if response.error.as_deref() == Some("users_not_found") {
            return Ok(None);
        }
        Err(SlackBotError::Api(
            response
                .error
                .unwrap_or_else(|| "unknown_error".to_string()),
        ))
    }

    async fn channel_member_ids(&self, channel_id: &str) -> Result<Vec<String>, SlackBotError> {
        let mut cursor = String::new();
        let mut members = Vec::new();
        loop {
            let mut body = json!({ "channel": channel_id, "limit": 200 });
            if !cursor.is_empty() {
                body["cursor"] = json!(cursor);
            }
            let response: ChannelMembersResponse = self.get("conversations.members", &body).await?;
            if !response.ok {
                return Err(SlackBotError::Api(
                    response
                        .error
                        .unwrap_or_else(|| "unknown_error".to_string()),
                ));
            }
            members.extend(response.members);
            cursor = response.response_metadata.next_cursor;
            if cursor.is_empty() {
                return Ok(members);
            }
        }
    }

    async fn invite_to_channel(
        &self,
        channel_id: &str,
        user_ids: &[String],
    ) -> Result<(), SlackBotError> {
        for batch in user_ids.chunks(100) {
            let response: InviteResponse = self
                .post(
                    "conversations.invite",
                    &json!({ "channel": channel_id, "users": batch.join(",") }),
                )
                .await?;
            if !response.ok {
                return Err(SlackBotError::Api(
                    response
                        .error
                        .unwrap_or_else(|| "unknown_error".to_string()),
                ));
            }
        }
        Ok(())
    }

    async fn archive_channel(&self, channel_id: &str) -> Result<bool, SlackBotError> {
        let response: InviteResponse = self
            .post("conversations.archive", &json!({ "channel": channel_id }))
            .await?;
        match (response.ok, response.error.as_deref()) {
            (true, _) => Ok(true),
            (false, Some("already_archived" | "channel_not_found")) => Ok(false),
            (false, error) => Err(SlackBotError::Api(
                error.unwrap_or("unknown_error").to_string(),
            )),
        }
    }

    async fn post_message_with_receipt(
        &self,
        channel_id: &str,
        text: &str,
        correlation_id: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        self.chat_post(channel_id, text, correlation_id, None).await
    }

    async fn message_receipt_by_correlation(
        &self,
        channel_id: &str,
        correlation_id: &str,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        #[derive(Deserialize)]
        struct History {
            ok: bool,
            error: Option<String>,
            #[serde(default)]
            messages: Vec<serde_json::Value>,
        }
        // One bounded read per reconciliation pass respects restrictive history
        // limits. A missing message stays unknown and is never retransmitted.
        let history: History = self
            .get(
                "conversations.history",
                &json!({
                    "channel": channel_id, "limit": 15,
                }),
            )
            .await?;
        if !history.ok {
            return Err(api_error(history.error));
        }
        Ok(history.messages.iter().find_map(|message| {
            if message.get("client_msg_id")?.as_str()? != correlation_id {
                return None;
            }
            let timestamp = message.get("ts")?.as_str()?.to_string();
            if timestamp.is_empty() {
                return None;
            }
            Some(SlackMessageReceipt {
                channel_id: channel_id.to_string(),
                timestamp,
                correlation_id: Some(correlation_id.to_string()),
            })
        }))
    }

    async fn post_thread_reply(
        &self,
        channel_id: &str,
        thread_ts: &str,
        text: &str,
    ) -> Result<(), SlackBotError> {
        self.chat_post(channel_id, text, None, Some(thread_ts))
            .await
            .map(|_| ())
    }

    async fn channel_canvas_id(&self, channel_id: &str) -> Result<Option<String>, SlackBotError> {
        let response: ChannelInfoResponse = self
            .get("conversations.info", &json!({ "channel": channel_id }))
            .await?;
        if !response.ok {
            return Err(api_error(response.error));
        }
        Ok(response
            .channel
            .and_then(|channel| channel.properties)
            .and_then(|properties| properties.canvas)
            .and_then(|canvas| canvas.file_id))
    }

    async fn create_channel_canvas(
        &self,
        channel_id: &str,
        markdown: &str,
    ) -> Result<String, SlackBotError> {
        let markdown = cloud::outbound_slack_text(markdown, self.from_staging);
        let response: CanvasCreateResponse = self
            .post(
                "conversations.canvases.create",
                &json!({
                    "channel_id": channel_id,
                    "document_content": { "type": "markdown", "markdown": markdown },
                }),
            )
            .await?;
        if !response.ok {
            return Err(api_error(response.error));
        }
        response.canvas_id.ok_or(SlackBotError::IncompleteResponse)
    }

    async fn replace_canvas(&self, canvas_id: &str, markdown: &str) -> Result<(), SlackBotError> {
        let markdown = cloud::outbound_slack_text(markdown, self.from_staging);
        let response: InviteResponse = self
            .post(
                "canvases.edit",
                &json!({
                    "canvas_id": canvas_id,
                    "changes": [{
                        "operation": "replace",
                        "document_content": { "type": "markdown", "markdown": markdown },
                    }],
                }),
            )
            .await?;
        if response.ok {
            Ok(())
        } else {
            Err(api_error(response.error))
        }
    }
}

impl SlackBotClient {
    async fn chat_post(
        &self,
        channel_id: &str,
        text: &str,
        correlation_id: Option<&str>,
        thread_ts: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        let text = cloud::outbound_slack_text(text, self.from_staging);
        let mut body = json!({
            "channel": channel_id,
            "text": text,
            "unfurl_links": false,
            "unfurl_media": false,
        });
        if let Some(correlation_id) = correlation_id {
            body["client_msg_id"] = json!(correlation_id);
        }
        if let Some(thread_ts) = thread_ts {
            body["thread_ts"] = json!(thread_ts);
        }
        let response: PostMessageResponse = self.post("chat.postMessage", &body).await?;
        if response.ok {
            Ok(SlackMessageReceipt {
                channel_id: response.channel.ok_or(SlackBotError::IncompleteResponse)?,
                timestamp: response.ts.ok_or(SlackBotError::IncompleteResponse)?,
                correlation_id: correlation_id.map(str::to_string),
            })
        } else {
            Err(SlackBotError::Api(
                response
                    .error
                    .unwrap_or_else(|| "unknown_error".to_string()),
            ))
        }
    }
}

/// Decode a Slack Web API body, lifting `missing_scope` into its own error so
/// the method and the scope Slack named survive into logs.
async fn decode<R: for<'de> Deserialize<'de>>(
    method: &str,
    response: reqwest::Response,
) -> Result<R, SlackBotError> {
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| SlackBotError::Transport(error.to_string()))?;
    if body["ok"] == false && body["error"] == "missing_scope" {
        return Err(SlackBotError::MissingScope {
            method: method.to_string(),
            needed: body["needed"].as_str().unwrap_or("unspecified").to_string(),
            provided: body["provided"]
                .as_str()
                .unwrap_or("unspecified")
                .to_string(),
        });
    }
    serde_json::from_value(body).map_err(|error| SlackBotError::Transport(error.to_string()))
}

fn api_error(error: Option<String>) -> SlackBotError {
    SlackBotError::Api(error.unwrap_or_else(|| "unknown_error".to_string()))
}

/// In-memory Slack bot used by tests and local development. It proves the
/// target-aware request path without sending anything outside the process.
#[derive(Clone, Default)]
pub struct CapturingSlackBot {
    created: Arc<Mutex<Vec<String>>>,
    posted: Arc<Mutex<Vec<(String, String)>>>,
    receipts: Arc<Mutex<Vec<SlackMessageReceipt>>>,
    replies: Arc<Mutex<Vec<(String, String)>>>,
    canvases: Arc<Mutex<Vec<CapturedCanvas>>>,
    canvas_writes: Arc<Mutex<Vec<CanvasWrite>>>,
    canvas_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedCanvas {
    channel_id: String,
    canvas_id: String,
    markdown: String,
}

impl CapturingSlackBot {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A bot whose canvas calls all fail with Slack error `code`, such as a
    /// missing scope.
    #[must_use]
    pub fn failing_canvases(mut self, code: &str) -> Self {
        self.canvas_error = Some(code.to_string());
        self
    }

    /// Every canvas create or edit, in order.
    #[must_use]
    pub fn canvas_writes(&self) -> Vec<CanvasWrite> {
        self.canvas_writes
            .lock()
            .expect("Slack bot lock poisoned")
            .clone()
    }

    /// The current markdown of a channel's canvas, if it has one.
    #[must_use]
    pub fn canvas_markdown(&self, channel_id: &str) -> Option<String> {
        self.canvases
            .lock()
            .expect("Slack bot lock poisoned")
            .iter()
            .find(|canvas| canvas.channel_id == channel_id)
            .map(|canvas| canvas.markdown.clone())
    }

    /// Every channel id a canvas call named, in order.
    #[must_use]
    pub fn canvas_channels(&self) -> Vec<String> {
        self.canvases
            .lock()
            .expect("Slack bot lock poisoned")
            .iter()
            .map(|canvas| canvas.channel_id.clone())
            .collect()
    }

    #[must_use]
    pub fn created_channels(&self) -> Vec<String> {
        self.created
            .lock()
            .expect("Slack bot lock poisoned")
            .clone()
    }

    #[must_use]
    pub fn posted_messages(&self) -> Vec<(String, String)> {
        self.posted.lock().expect("Slack bot lock poisoned").clone()
    }

    /// Thread replies as `(thread_ts, text)`.
    #[must_use]
    pub fn thread_replies(&self) -> Vec<(String, String)> {
        self.replies
            .lock()
            .expect("Slack bot lock poisoned")
            .clone()
    }
}

#[async_trait]
impl SlackBot for CapturingSlackBot {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        let count = {
            let mut created = self.created.lock().expect("Slack bot lock poisoned");
            created.push(name.to_string());
            created.len()
        };
        Ok(SlackChannel {
            id: format!("C{count:016X}"),
            name: name.to_string(),
        })
    }

    async fn post_message(&self, channel_id: &str, text: &str) -> Result<(), SlackBotError> {
        self.posted
            .lock()
            .expect("Slack bot lock poisoned")
            .push((channel_id.to_string(), text.to_string()));
        Ok(())
    }

    async fn user_id_by_email(&self, _email: &str) -> Result<Option<String>, SlackBotError> {
        Ok(None)
    }

    async fn channel_member_ids(&self, _channel_id: &str) -> Result<Vec<String>, SlackBotError> {
        Ok(Vec::new())
    }

    async fn invite_to_channel(
        &self,
        _channel_id: &str,
        _user_ids: &[String],
    ) -> Result<(), SlackBotError> {
        Ok(())
    }

    async fn post_message_with_receipt(
        &self,
        channel_id: &str,
        text: &str,
        correlation_id: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        self.post_message(channel_id, text).await?;
        let receipt = SlackMessageReceipt {
            channel_id: channel_id.to_string(),
            timestamp: format!("{}.000000", self.posted_messages().len()),
            correlation_id: correlation_id.map(str::to_string),
        };
        self.receipts
            .lock()
            .map_err(|_| SlackBotError::Transport("capturing receipt lock poisoned".into()))?
            .push(receipt.clone());
        Ok(receipt)
    }

    async fn message_receipt_by_correlation(
        &self,
        channel_id: &str,
        correlation_id: &str,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        Ok(self
            .receipts
            .lock()
            .map_err(|_| SlackBotError::Transport("capturing receipt lock poisoned".into()))?
            .iter()
            .find(|receipt| {
                receipt.channel_id == channel_id
                    && receipt.correlation_id.as_deref() == Some(correlation_id)
            })
            .cloned())
    }

    async fn post_thread_reply(
        &self,
        _channel_id: &str,
        thread_ts: &str,
        text: &str,
    ) -> Result<(), SlackBotError> {
        self.replies
            .lock()
            .expect("Slack bot lock poisoned")
            .push((thread_ts.to_string(), text.to_string()));
        Ok(())
    }

    async fn channel_canvas_id(&self, channel_id: &str) -> Result<Option<String>, SlackBotError> {
        if let Some(code) = &self.canvas_error {
            return Err(SlackBotError::Api(code.clone()));
        }
        Ok(self
            .canvases
            .lock()
            .expect("Slack bot lock poisoned")
            .iter()
            .find(|canvas| canvas.channel_id == channel_id)
            .map(|canvas| canvas.canvas_id.clone()))
    }

    async fn create_channel_canvas(
        &self,
        channel_id: &str,
        markdown: &str,
    ) -> Result<String, SlackBotError> {
        if let Some(code) = &self.canvas_error {
            return Err(SlackBotError::Api(code.clone()));
        }
        let mut canvases = self.canvases.lock().expect("Slack bot lock poisoned");
        if canvases
            .iter()
            .any(|canvas| canvas.channel_id == channel_id)
        {
            return Err(SlackBotError::Api(CHANNEL_CANVAS_EXISTS.to_string()));
        }
        let canvas_id = format!("F{:016X}", canvases.len() + 1);
        canvases.push(CapturedCanvas {
            channel_id: channel_id.to_string(),
            canvas_id: canvas_id.clone(),
            markdown: markdown.to_string(),
        });
        self.canvas_writes
            .lock()
            .expect("Slack bot lock poisoned")
            .push(CanvasWrite::Created(canvas_id.clone()));
        Ok(canvas_id)
    }

    async fn replace_canvas(&self, canvas_id: &str, markdown: &str) -> Result<(), SlackBotError> {
        if let Some(code) = &self.canvas_error {
            return Err(SlackBotError::Api(code.clone()));
        }
        let mut canvases = self.canvases.lock().expect("Slack bot lock poisoned");
        let canvas = canvases
            .iter_mut()
            .find(|canvas| canvas.canvas_id == canvas_id)
            .ok_or_else(|| SlackBotError::Api("canvas_not_found".to_string()))?;
        canvas.markdown = markdown.to_string();
        self.canvas_writes
            .lock()
            .expect("Slack bot lock poisoned")
            .push(CanvasWrite::Edited(canvas_id.to_string()));
        Ok(())
    }
}

/// A one-way channel for internal operations notifications. Implementors post a
/// short plain-text message somewhere firm engineers watch (Slack today).
#[async_trait]
pub trait Notifier: Send + Sync {
    /// Deliver `text`. Returns `Ok(())` on a successful post.
    async fn notify(&self, text: String) -> Result<(), NotifyError>;
}

/// Captures every message in memory instead of sending it. Used by tests and
/// KIND/dev, where posting to the real engineering channel would be noise.
#[derive(Clone, Default)]
pub struct CapturingNotifier {
    sent: Arc<Mutex<Vec<String>>>,
}

impl CapturingNotifier {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every message handed to [`Notifier::notify`] so far.
    #[must_use]
    pub fn captured(&self) -> Vec<String> {
        self.sent.lock().expect("notifier lock poisoned").clone()
    }
}

#[async_trait]
impl Notifier for CapturingNotifier {
    async fn notify(&self, text: String) -> Result<(), NotifyError> {
        self.sent.lock().expect("notifier lock poisoned").push(text);
        Ok(())
    }
}

/// The ops [`Notifier`] a process builds from its environment:
/// `SLACK_WEBHOOK_URL` set and non-empty selects [`SlackNotifier`]; unset or
/// empty selects [`CapturingNotifier`], so KIND and tests never post to a real
/// channel. A missing webhook is a valid configuration, never a boot failure.
pub fn notifier_from_lookup<F: Fn(&str) -> Option<String>>(get: F) -> Arc<dyn Notifier> {
    match get("SLACK_WEBHOOK_URL").filter(|s| !s.is_empty()) {
        Some(url) => Arc::new(SlackNotifier::new(url)),
        None => Arc::new(CapturingNotifier::new()),
    }
}

/// Posts to a Slack **incoming webhook**. The webhook URL already pins the
/// destination channel, so the only payload is the message text.
#[derive(Clone)]
pub struct SlackNotifier {
    http: reqwest::Client,
    webhook_url: String,
    from_staging: bool,
}

impl SlackNotifier {
    /// Production constructor: targets the given incoming-webhook URL
    /// (`SLACK_WEBHOOK_URL`). Staging labeling follows
    /// [`cloud::labels_slack_from_staging_env`].
    #[must_use]
    pub fn new(webhook_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            webhook_url: webhook_url.into(),
            from_staging: cloud::labels_slack_from_staging_env(),
        }
    }

    #[cfg(test)]
    fn with_staging_origin(webhook_url: impl Into<String>, from_staging: bool) -> Self {
        Self {
            http: reqwest::Client::new(),
            webhook_url: webhook_url.into(),
            from_staging,
        }
    }

    /// Build the JSON body for the Slack incoming-webhook endpoint. Pure —
    /// exposed for unit-testing the request shape without an HTTP round-trip.
    #[must_use]
    pub fn build_request_body(text: &str) -> serde_json::Value {
        json!({ "text": text })
    }
}

#[async_trait]
impl Notifier for SlackNotifier {
    async fn notify(&self, text: String) -> Result<(), NotifyError> {
        let text = cloud::outbound_slack_text(&text, self.from_staging);
        let resp = self
            .http
            .post(&self.webhook_url)
            .json(&Self::build_request_body(&text))
            .send()
            .await
            .map_err(|e| NotifyError::Transport(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(NotifyError::Rejected(status.as_u16()))
        }
    }
}

/// Slack rejects a single message's `text` past 4000 characters and splits it
/// into separate messages at that boundary — which lands mid-code-block for a
/// long ops digest, leaving the first message an *unterminated* fence that
/// Slack renders as literal backticks in proportional text. So the body is
/// split here, on line boundaries, into chunks small enough that each fenced
/// message stays well under the limit. Reserves headroom below 4000 for the
/// bold header and the fence lines.
const SLACK_CHUNK_BUDGET: usize = 3500;

/// Render an internal ops email as one or more Slack messages: the subject as a
/// bold header, then the plain-text body in a code block so fixed-width ops
/// tables keep their columns in Slack. A body that would overflow Slack's
/// 4000-character message limit is split into several messages, each an
/// independently valid (self-closed) code block and each numbered `(i/n)` in
/// its header. Pure and exposed so the formatting is unit-tested. The HTML part
/// is intentionally dropped — Slack renders the plain text and the body already
/// reads as a standalone ops notice.
#[must_use]
pub fn ops_slack_messages(email: &OutboundEmail) -> Vec<String> {
    let body = email.body.trim_end().replace("```", "'''");
    let chunks = chunk_body_lines(&body, SLACK_CHUNK_BUDGET);
    let total = chunks.len();
    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let header = if total > 1 {
                format!("*{}* ({}/{total})", email.subject, i + 1)
            } else {
                format!("*{}*", email.subject)
            };
            format!("{header}\n```\n{chunk}\n```")
        })
        .collect()
}

/// Group `body`'s lines into chunks whose character count stays within
/// `budget`, never splitting a line (a table row or fence must stay intact).
/// Always returns at least one chunk (empty for an empty body). A single line
/// longer than `budget` becomes its own chunk unsplit — ops lines (URLs, table
/// rows) are far shorter than the budget, so this only guards pathological
/// input.
fn chunk_body_lines(body: &str, budget: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in body.split('\n') {
        // +1 for the '\n' that would join this line onto the current chunk.
        let projected = current.chars().count() + line.chars().count() + 1;
        if !current.is_empty() && projected > budget {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    chunks.push(current);
    chunks
}

/// An [`EmailService`] adapter that delivers an ops notice to a [`Notifier`]
/// (Slack) **instead of** sending email. The internal/ops workflows render
/// their notice as an [`OutboundEmail`] and hand it to their `EmailService`;
/// backing them with this adapter posts that notice to the engineering channel
/// and sends no mail.
///
/// Slack is now the single delivery path for the ops signal, so — unlike the
/// former best-effort dual-send mirror — a delivery failure **is** propagated
/// as an [`EmailError`]. That fails the workflow's durable `ctx.run("notify")`
/// step, so Restate retries and redelivers once Slack recovers, rather than
/// silently dropping the only copy of the signal. Back ONLY internal/ops
/// services — see the module-level boundary note.
#[derive(Clone)]
pub struct SlackOpsDelivery {
    notifier: Arc<dyn Notifier>,
}

impl SlackOpsDelivery {
    #[must_use]
    pub fn new(notifier: Arc<dyn Notifier>) -> Self {
        Self { notifier }
    }
}

#[async_trait]
impl EmailService for SlackOpsDelivery {
    async fn send(&self, email: OutboundEmail) -> Result<SendReceipt, EmailError> {
        // A long digest posts as several fenced messages; a failure partway
        // through surfaces so Restate retries the whole `notify` step (an ops
        // notice is at-least-once — a duplicate repost is harmless).
        for message in ops_slack_messages(&email) {
            self.notifier
                .notify(message)
                .await
                .map_err(|err| EmailError::Transport(err.to_string()))?;
        }
        // No mail was sent, so there is no provider message id to surface.
        Ok(SendReceipt { message_id: None })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ops_slack_messages, upsert_channel_canvas, CanvasWrite, CapturingNotifier,
        CapturingSlackBot, Notifier, NotifyError, SlackBot, SlackBotClient, SlackBotError,
        SlackNotifier, SlackOpsDelivery, SLACK_CHUNK_BUDGET,
    };
    use crate::email::service::{EmailError, EmailService, OutboundEmail};
    use async_trait::async_trait;
    use std::sync::Arc;

    fn ops_email() -> OutboundEmail {
        OutboundEmail::new(
            "nick@neonlaw.com",
            "Durable execution OK — heartbeat 2026-06-19 18:00 UTC",
            "The durable-execution heartbeat ran end to end.",
        )
    }

    #[test]
    fn slack_body_is_text_field() {
        let body = SlackNotifier::build_request_body("hello ops");
        assert_eq!(body, serde_json::json!({ "text": "hello ops" }));
    }

    #[tokio::test]
    async fn correlated_history_read_confirms_a_receipt_without_posting() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/conversations.history"))
            .and(query_param("channel", "synthetic-channel"))
            .and(query_param("limit", "15"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok":true, "messages":[{"ts":"123.456","client_msg_id":"synthetic-correlation"}]
            })))
            .expect(2)
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("synthetic-token", server.uri());
        let receipt = bot
            .message_receipt_by_correlation("synthetic-channel", "synthetic-correlation")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.timestamp, "123.456");
        assert!(bot
            .message_receipt_by_correlation("synthetic-channel", "another-correlation")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn a_second_upsert_edits_the_same_canvas_and_never_stacks_a_second() {
        let bot = CapturingSlackBot::new();
        let first = upsert_channel_canvas(&bot, "CINTERNAL", "first")
            .await
            .expect("create");
        let second = upsert_channel_canvas(&bot, "CINTERNAL", "second")
            .await
            .expect("edit");
        let CanvasWrite::Created(id) = first else {
            panic!("the first write creates the canvas: {first:?}");
        };
        assert_eq!(second, CanvasWrite::Edited(id));
        assert_eq!(bot.canvas_channels(), ["CINTERNAL"], "one canvas");
        assert_eq!(bot.canvas_markdown("CINTERNAL").as_deref(), Some("second"));
        assert_eq!(bot.canvas_markdown("COTHER"), None);
    }

    #[tokio::test]
    async fn a_bot_without_canvases_reports_the_capability_missing() {
        struct Plain;
        #[async_trait]
        impl SlackBot for Plain {
            async fn create_private_channel(
                &self,
                _: &str,
            ) -> Result<super::SlackChannel, SlackBotError> {
                unreachable!()
            }
            async fn post_message(&self, _: &str, _: &str) -> Result<(), SlackBotError> {
                Ok(())
            }
            async fn user_id_by_email(&self, _: &str) -> Result<Option<String>, SlackBotError> {
                Ok(None)
            }
            async fn channel_member_ids(&self, _: &str) -> Result<Vec<String>, SlackBotError> {
                Ok(Vec::new())
            }
            async fn invite_to_channel(&self, _: &str, _: &[String]) -> Result<(), SlackBotError> {
                Ok(())
            }
        }
        let error = upsert_channel_canvas(&Plain, "C1", "x").await.unwrap_err();
        assert!(matches!(error, SlackBotError::Api(code) if code == super::CANVAS_UNSUPPORTED));
    }

    #[tokio::test]
    async fn the_web_api_creates_a_missing_channel_canvas_and_edits_an_existing_one() {
        use wiremock::matchers::{body_partial_json, method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/conversations.info"))
            .and(query_param("channel", "CNEW"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":true,"channel":{"id":"CNEW","properties":{}}}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/conversations.info"))
            .and(query_param("channel", "CHAS"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"ok":true,"channel":{"id":"CHAS","properties":{"canvas":{"file_id":"F42"}}}}"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/conversations.canvases.create"))
            .and(body_partial_json(serde_json::json!({
                "channel_id": "CNEW",
                "document_content": { "type": "markdown", "markdown": "# Body" }
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"ok":true,"canvas_id":"F1"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/canvases.edit"))
            .and(body_partial_json(serde_json::json!({
                "canvas_id": "F42",
                "changes": [{
                    "operation": "replace",
                    "document_content": { "type": "markdown", "markdown": "# Body" }
                }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .expect(1)
            .mount(&server)
            .await;

        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        assert_eq!(
            upsert_channel_canvas(&bot, "CNEW", "# Body").await.unwrap(),
            CanvasWrite::Created("F1".to_string())
        );
        assert_eq!(
            upsert_channel_canvas(&bot, "CHAS", "# Body").await.unwrap(),
            CanvasWrite::Edited("F42".to_string())
        );
    }

    #[tokio::test]
    async fn a_canvas_created_between_lookup_and_create_is_edited_not_duplicated() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/conversations.info"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true,"channel":{}}"#))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/conversations.info"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"ok":true,"channel":{"properties":{"canvas":{"file_id":"F7"}}}}"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/conversations.canvases.create"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":false,"error":"channel_canvas_already_exists"}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/canvases.edit"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .expect(1)
            .mount(&server)
            .await;

        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        assert_eq!(
            upsert_channel_canvas(&bot, "C1", "x").await.unwrap(),
            CanvasWrite::Edited("F7".to_string())
        );
    }

    #[tokio::test]
    async fn a_missing_scope_names_the_scope_slack_needed() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/conversations.info"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true,"channel":{}}"#))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/conversations.canvases.create"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":false,"error":"missing_scope","needed":"canvases:write","provided":"chat:write"}"#),
            )
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let error = upsert_channel_canvas(&bot, "C1", "x").await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "Slack API rejected conversations.canvases.create: missing_scope (needed: canvases:write; provided: chat:write)"
        );
        assert!(matches!(
            error,
            SlackBotError::MissingScope { method, needed, provided }
                if method == "conversations.canvases.create"
                    && needed == "canvases:write"
                    && provided == "chat:write"
        ));
    }

    #[tokio::test]
    async fn a_missing_scope_without_needed_is_still_typed() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/conversations.members"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":false,"error":"missing_scope"}"#),
            )
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let error = bot.channel_member_ids("C1").await.unwrap_err();
        assert!(matches!(
            error,
            SlackBotError::MissingScope { method, needed, provided }
                if method == "conversations.members"
                    && needed == "unspecified"
                    && provided == "unspecified"
        ));
    }

    #[tokio::test]
    async fn bot_creates_private_channel_and_posts_to_its_id() {
        let bot = CapturingSlackBot::new();
        let channel = bot
            .create_private_channel("sample-project")
            .await
            .expect("capturing bot creates a channel");
        bot.post_message(&channel.id, "Client viewed this Project in the portal.")
            .await
            .expect("capturing bot posts");

        assert_eq!(bot.created_channels(), vec!["sample-project"]);
        assert_eq!(
            bot.posted_messages(),
            vec![(
                channel.id,
                "Client viewed this Project in the portal.".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn bot_uses_channel_id_and_bearer_token_on_the_web_api() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/conversations.create"))
            .and(header("authorization", "Bearer xoxb-test"))
            .and(body_partial_json(serde_json::json!({
                "name": "sample-project",
                "is_private": true
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"ok":true,"channel":{"id":"C123","name":"sample-project"}}"#,
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .and(header("authorization", "Bearer xoxb-test"))
            .and(body_partial_json(serde_json::json!({
                "channel": "C123",
                "text": "hello"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .expect(1)
            .mount(&server)
            .await;

        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let channel = bot
            .create_private_channel("sample-project")
            .await
            .expect("Slack create succeeds");
        bot.post_message(&channel.id, "hello")
            .await
            .expect("Slack post succeeds");
    }

    #[tokio::test]
    async fn bot_resolves_members_reads_channel_members_and_invites_batches() {
        use wiremock::matchers::{body_partial_json, method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users.lookupByEmail"))
            .and(query_param("email", "lawyer@example.com"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"ok":true,"user":{"id":"U123"}}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/conversations.members"))
            .and(query_param("channel", "G123"))
            .and(query_param("limit", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"ok":true,"members":["U456"],"response_metadata":{"next_cursor":""}}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/conversations.invite"))
            .and(body_partial_json(serde_json::json!({
                "channel": "G123",
                "users": "U123,U789"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .expect(1)
            .mount(&server)
            .await;

        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        assert_eq!(
            bot.user_id_by_email("lawyer@example.com")
                .await
                .expect("lookup succeeds"),
            Some("U123".to_string())
        );
        assert_eq!(
            bot.channel_member_ids("G123")
                .await
                .expect("member listing succeeds"),
            vec!["U456"]
        );
        bot.invite_to_channel("G123", &["U123".into(), "U789".into()])
            .await
            .expect("invite succeeds");
    }

    #[test]
    fn channel_names_carry_the_environment_prefix_and_a_slack_safe_slug() {
        use super::project_channel_name;
        assert_eq!(
            project_channel_name(false, "sample-estate"),
            "project-sample-estate"
        );
        assert_eq!(
            project_channel_name(true, "sample-estate"),
            "staging-sample-estate"
        );
        assert_eq!(
            project_channel_name(false, "Acme v. Co."),
            "project-acme-v--co-"
        );
        assert_eq!(project_channel_name(true, &"x".repeat(200)).len(), 80);
    }

    #[tokio::test]
    async fn bot_finds_a_private_channel_by_name_across_pages() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/conversations.list"))
            .and(query_param("types", "private_channel"))
            .and(query_param("cursor", "next"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"ok":true,"channels":[{"id":"G2","name":"project-b"}],"response_metadata":{"next_cursor":""}}"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/conversations.list"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"ok":true,"channels":[{"id":"G1","name":"project-a"}],"response_metadata":{"next_cursor":"next"}}"#,
            ))
            .mount(&server)
            .await;

        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let found = bot
            .find_private_channel_by_name("project-b")
            .await
            .expect("list succeeds")
            .expect("channel found");
        assert_eq!(found.id, "G2");
        assert!(bot
            .find_private_channel_by_name("project-missing")
            .await
            .expect("list succeeds")
            .is_none());
    }

    #[tokio::test]
    async fn membership_lookup_distinguishes_missing_users_incomplete_and_api_errors() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users.lookupByEmail"))
            .and(query_param("email", "missing@example.com"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":false,"error":"users_not_found"}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users.lookupByEmail"))
            .and(query_param("email", "incomplete@example.com"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users.lookupByEmail"))
            .and(query_param("email", "denied@example.com"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"ok":false,"error":"missing_scope","needed":"users:read.email"}"#,
            ))
            .mount(&server)
            .await;

        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        assert_eq!(
            bot.user_id_by_email("missing@example.com")
                .await
                .expect("missing users are not errors"),
            None
        );
        assert!(matches!(
            bot.user_id_by_email("incomplete@example.com").await,
            Err(SlackBotError::IncompleteResponse)
        ));
        assert!(matches!(
            bot.user_id_by_email("denied@example.com").await,
            Err(SlackBotError::MissingScope { method, needed, .. })
                if method == "users.lookupByEmail" && needed == "users:read.email"
        ));
    }

    #[tokio::test]
    async fn membership_get_requests_page_members_and_keep_http_retry_classes() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/conversations.members"))
            .and(query_param("channel", "C-PAGE"))
            .and(query_param("limit", "200"))
            .and(query_param("cursor", "next-page"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"ok":true,"members":["U2"],"response_metadata":{"next_cursor":""}}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/conversations.members"))
            .and(query_param("channel", "C-PAGE"))
            .and(query_param("limit", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"ok":true,"members":["U1"],"response_metadata":{"next_cursor":"next-page"}}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        assert_eq!(
            bot.channel_member_ids("C-PAGE")
                .await
                .expect("member pages combine"),
            ["U1", "U2"]
        );
    }

    #[tokio::test]
    async fn membership_get_maps_api_and_http_failures() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/conversations.members"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":false,"error":"channel_not_found"}"#),
            )
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        assert!(matches!(
            bot.channel_member_ids("C-MISSING").await,
            Err(SlackBotError::Api(error)) if error == "channel_not_found"
        ));

        for status in [403, 503] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/users.lookupByEmail"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
            let error = bot
                .user_id_by_email("lawyer@example.com")
                .await
                .expect_err("non-success HTTP status is an error");
            match (status, error) {
                (403, SlackBotError::HttpStatus(403))
                | (503, SlackBotError::RetryableHttpStatus(503)) => {}
                (status, error) => panic!("unexpected Slack HTTP error for {status}: {error:?}"),
            }
        }

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users.lookupByEmail"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "9"))
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        assert!(matches!(
            bot.user_id_by_email("lawyer@example.com").await,
            Err(SlackBotError::RateLimited {
                retry_after_seconds: Some(9)
            })
        ));
    }

    #[tokio::test]
    async fn membership_invites_split_batches_and_report_slack_errors() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/conversations.invite"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .expect(2)
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let user_ids = (0..101)
            .map(|index| format!("U{index}"))
            .collect::<Vec<_>>();
        bot.invite_to_channel("C-INVITE", &user_ids)
            .await
            .expect("both invite batches succeed");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/conversations.invite"))
            .and(body_partial_json(
                serde_json::json!({"channel":"C-INVITE","users":"U1"}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":false,"error":"already_in_channel"}"#),
            )
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        assert!(matches!(
            bot.invite_to_channel("C-INVITE", &["U1".to_string()]).await,
            Err(SlackBotError::Api(error)) if error == "already_in_channel"
        ));
    }

    #[tokio::test]
    async fn receipt_post_returns_coordinates_and_disables_unfurls() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .and(header("authorization", "Bearer xoxb-test"))
            .and(body_partial_json(serde_json::json!({
                "channel": "C123",
                "text": "hello",
                "client_msg_id": "corr-123",
                "unfurl_links": false,
                "unfurl_media": false
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":true,"channel":"C123","ts":"1700000000.000001"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;

        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let receipt = bot
            .post_message_with_receipt("C123", "hello", Some("corr-123"))
            .await
            .expect("Slack receipt succeeds");
        assert_eq!(receipt.channel_id, "C123");
        assert_eq!(receipt.timestamp, "1700000000.000001");
        assert_eq!(receipt.correlation_id.as_deref(), Some("corr-123"));
    }

    #[tokio::test]
    async fn thread_reply_posts_under_the_parent_timestamp() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .and(body_partial_json(serde_json::json!({
                "channel": "C123",
                "text": "the full body",
                "thread_ts": "1700000000.000001"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":true,"channel":"C123","ts":"1700000000.000002"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;

        SlackBotClient::with_base_url("xoxb-test", server.uri())
            .post_thread_reply("C123", "1700000000.000001", "the full body")
            .await
            .expect("thread reply succeeds");
    }

    #[tokio::test]
    async fn receipt_post_marks_missing_coordinates_as_incomplete() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"ok":true,"channel":"C123"}"#),
            )
            .mount(&server)
            .await;

        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let error = bot
            .post_message_with_receipt("C123", "hello", Some("corr-123"))
            .await
            .expect_err("a missing timestamp is ambiguous");
        assert!(matches!(error, super::SlackBotError::IncompleteResponse));
    }

    #[tokio::test]
    async fn slack_api_errors_preserve_retry_classification() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "17"))
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let error = bot
            .post_message("C123", "hello")
            .await
            .expect_err("rate limits must be surfaced");
        assert!(matches!(
            error,
            super::SlackBotError::RateLimited {
                retry_after_seconds: Some(17)
            }
        ));

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let error = bot
            .post_message("C123", "hello")
            .await
            .expect_err("server failures must be retryable");
        assert!(matches!(
            error,
            super::SlackBotError::RetryableHttpStatus(503)
        ));

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let error = bot
            .post_message("C123", "hello")
            .await
            .expect_err("client failures must be permanent");
        assert!(matches!(error, super::SlackBotError::HttpStatus(400)));
    }

    #[tokio::test]
    async fn slack_api_payload_errors_are_distinct_from_http_errors() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":false,"error":"channel_not_found"}"#),
            )
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let error = bot
            .post_message("C123", "hello")
            .await
            .expect_err("Slack API errors must be surfaced");
        assert!(matches!(
            error,
            super::SlackBotError::Api(message) if message == "channel_not_found"
        ));

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/conversations.create"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"ok":false,"error":"name_taken"}"#),
            )
            .mount(&server)
            .await;
        let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
        let error = bot
            .create_private_channel("sample-project")
            .await
            .expect_err("channel creation errors must be surfaced");
        assert!(matches!(
            error,
            super::SlackBotError::Api(message) if message == "name_taken"
        ));
    }

    /// A closed Project's channel is archived once; Slack's answer for an
    /// already-archived or vanished channel is the steady state, not a
    /// failure, and any other refusal is surfaced as itself.
    #[tokio::test]
    async fn archiving_is_idempotent_and_surfaces_real_refusals() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        for (body, expected) in [
            (r#"{"ok":true}"#, Ok(true)),
            (r#"{"ok":false,"error":"already_archived"}"#, Ok(false)),
            (r#"{"ok":false,"error":"channel_not_found"}"#, Ok(false)),
            (
                r#"{"ok":false,"error":"not_in_channel"}"#,
                Err("not_in_channel"),
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/conversations.archive"))
                .and(body_partial_json(serde_json::json!({ "channel": "C123" })))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .expect(1)
                .mount(&server)
                .await;
            let bot = SlackBotClient::with_base_url("xoxb-test", server.uri());
            let outcome = bot.archive_channel("C123").await;
            match expected {
                Ok(archived) => assert_eq!(outcome.expect(body), archived, "{body}"),
                Err(code) => assert!(
                    matches!(&outcome, Err(super::SlackBotError::Api(message)) if message == code),
                    "{body}: {outcome:?}"
                ),
            }
        }
    }

    #[tokio::test]
    async fn staging_bot_post_appends_from_staging() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .and(body_partial_json(serde_json::json!({
                "channel": "C123",
                "text": "hello\nfrom Staging"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .expect(1)
            .mount(&server)
            .await;

        let bot = SlackBotClient::with_staging_origin("xoxb-test", server.uri(), true);
        bot.post_message("C123", "hello")
            .await
            .expect("Slack post succeeds");
    }

    #[tokio::test]
    async fn staging_webhook_notify_appends_from_staging() {
        use wiremock::matchers::{body_json, method};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(serde_json::json!({
                "text": "hello ops\nfrom Staging"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;

        let notifier = SlackNotifier::with_staging_origin(server.uri(), true);
        notifier
            .notify("hello ops".into())
            .await
            .expect("webhook post succeeds");
    }

    #[test]
    fn ops_slack_short_body_is_one_bold_subject_then_fenced_body() {
        let msgs = ops_slack_messages(&ops_email());
        assert_eq!(msgs.len(), 1, "a short body fits one message");
        let text = &msgs[0];
        assert!(text.starts_with("*Durable execution OK"));
        assert!(text.contains("\n```"));
        assert!(text.contains("ran end to end"));
        assert!(text.ends_with("```"));
    }

    #[test]
    fn ops_slack_text_escapes_nested_code_fences() {
        let email = OutboundEmail::new("ops@example.com", "Ops", "before ``` after\n");
        let msgs = ops_slack_messages(&email);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("before ''' after"));
        // Only the two fence lines this renderer added — the body's own fence
        // was neutralised to `'''`, so it can't leak an unbalanced ```.
        assert_eq!(msgs[0].matches("```").count(), 2);
    }

    #[test]
    fn ops_slack_long_body_splits_into_self_closed_fenced_messages() {
        // A body that dwarfs Slack's 4000-char limit — the archives digest's
        // failure mode. Each line is short; there are simply many of them.
        let body = (0..400)
            .map(|i| format!("  table_{i:03}: gs://bucket/iceberg/table_{i:03}/data/part.parquet"))
            .collect::<Vec<_>>()
            .join("\n");
        let email = OutboundEmail::new("ops@example.com", "Archives digest", body.clone());
        let msgs = ops_slack_messages(&email);

        assert!(
            msgs.len() > 1,
            "an oversized body must split, got {} message(s)",
            msgs.len()
        );
        for (i, msg) in msgs.iter().enumerate() {
            // Every message is independently under Slack's hard limit...
            assert!(
                msg.chars().count() < 4000,
                "message {} is {} chars, over Slack's 4000 limit",
                i + 1,
                msg.chars().count()
            );
            // ...a self-closed code block (exactly the opening + closing fence,
            // never an unterminated one that renders as literal backticks)...
            assert_eq!(
                msg.matches("```").count(),
                2,
                "message {} has an unbalanced fence",
                i + 1
            );
            assert!(
                msg.ends_with("```"),
                "message {} must close its fence",
                i + 1
            );
            // ...and carries the bold, numbered header so the parts read as one.
            assert!(
                msg.starts_with(&format!("*Archives digest* ({}/{})", i + 1, msgs.len())),
                "message {} header: {:?}",
                i + 1,
                msg.lines().next()
            );
        }

        // No content is lost across the split: concatenating the fenced payloads
        // reproduces every original line.
        let rejoined: String = msgs
            .iter()
            .flat_map(|m| m.lines().skip(2)) // drop header + opening fence
            .filter(|l| *l != "```") // drop each closing fence
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rejoined, body);
    }

    #[test]
    fn chunk_budget_leaves_headroom_below_slack_limit() {
        // The per-message budget plus header and fences must clear 4000.
        const { assert!(SLACK_CHUNK_BUDGET < 4000 - 200) };
    }

    #[tokio::test]
    async fn capturing_notifier_records_messages() {
        let n = CapturingNotifier::new();
        n.notify("first".into())
            .await
            .expect("capturing never fails");
        n.notify("second".into())
            .await
            .expect("capturing never fails");
        assert_eq!(
            n.captured(),
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[tokio::test]
    async fn delivery_posts_notice_to_slack_only() {
        let notifier = Arc::new(CapturingNotifier::new());
        let delivery = SlackOpsDelivery::new(notifier.clone());

        delivery.send(ops_email()).await.expect("send succeeds");

        assert_eq!(notifier.captured().len(), 1, "notice posted to Slack");
        assert!(notifier.captured()[0].contains("heartbeat"));
    }

    #[tokio::test]
    async fn delivery_posts_every_chunk_of_an_oversized_digest() {
        let notifier = Arc::new(CapturingNotifier::new());
        let delivery = SlackOpsDelivery::new(notifier.clone());
        let body = (0..400)
            .map(|i| format!("  table_{i:03}: gs://bucket/iceberg/table_{i:03}/part.parquet"))
            .collect::<Vec<_>>()
            .join("\n");
        let email = OutboundEmail::new("ops@example.com", "Archives digest", body);

        delivery.send(email.clone()).await.expect("send succeeds");

        assert_eq!(
            notifier.captured(),
            ops_slack_messages(&email),
            "every fenced chunk is posted, in order"
        );
        assert!(
            notifier.captured().len() > 1,
            "an oversized digest posts as multiple messages"
        );
    }

    /// A notifier whose every send fails — proves the adapter surfaces the error.
    struct FailingNotifier;
    #[async_trait]
    impl Notifier for FailingNotifier {
        async fn notify(&self, _text: String) -> Result<(), NotifyError> {
            Err(NotifyError::Rejected(404))
        }
    }

    #[tokio::test]
    async fn delivery_propagates_failure_so_durable_step_retries() {
        let delivery = SlackOpsDelivery::new(Arc::new(FailingNotifier));

        // Slack is the only delivery path now, so a Slack outage MUST fail the
        // durable notify step (Restate then retries) rather than be swallowed.
        let err = delivery
            .send(ops_email())
            .await
            .expect_err("a Slack failure must surface to the caller");

        assert!(matches!(err, EmailError::Transport(_)));
    }
}
