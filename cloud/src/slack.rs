//! Firm-private Slack Web API adapter.
//!
//! The adapter accepts only provider-issued member IDs. Navigator never turns
//! an email address or a client participation row into a Slack invite.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;

const SLACK_BASE_URL: &str = "https://slack.com/api";
/// Slack's own cap for one `conversations.list` page.
const CHANNEL_PAGE_SIZE: u32 = 200;
/// A Firm past this many pages of private channels is not a matter list.
/// Bounded so a cursor the provider never clears cannot spin forever.
const MAX_CHANNEL_PAGES: usize = 20;

/// Line appended to every outbound Slack body from the persistent staging
/// deployment. Same spelling `store::NAVIGATOR_SIMULATED_MATTERS` documents.
pub const SLACK_STAGING_MARK: &str = "from Staging";

/// Same name as `store::NAVIGATOR_ENVIRONMENT`.
const NAVIGATOR_ENVIRONMENT: &str = "NAVIGATOR_ENVIRONMENT";
/// Same name as `store::NAVIGATOR_SIMULATED_MATTERS`.
const NAVIGATOR_SIMULATED_MATTERS: &str = "NAVIGATOR_SIMULATED_MATTERS";

/// Persistent staging is the production runtime profile over simulated
/// matters. `NAVIGATOR_ENVIRONMENT` admits only `dev` or `production`;
/// staging is never a profile value. The hosted staging row sets
/// `NAVIGATOR_SIMULATED_MATTERS=true` in its `config.toml` so a process
/// that otherwise matches production can still label its Slack traffic.
#[must_use]
pub fn labels_slack_from_staging_from<F: Fn(&str) -> Option<String>>(get: F) -> bool {
    let production_profile = matches!(
        get(NAVIGATOR_ENVIRONMENT).as_deref(),
        None | Some("" | "production")
    );
    production_profile && get(NAVIGATOR_SIMULATED_MATTERS).as_deref() == Some("true")
}

/// [`labels_slack_from_staging_from`] against the process environment.
#[must_use]
pub fn labels_slack_from_staging_env() -> bool {
    labels_slack_from_staging_from(|key| std::env::var(key).ok())
}

/// Append [`SLACK_STAGING_MARK`] as its own last line when this process is
/// the persistent staging deployment. Idempotent on a body that already
/// ends with that line.
#[must_use]
pub fn outbound_slack_text(text: &str, from_staging: bool) -> String {
    if !from_staging {
        return text.to_string();
    }
    let trimmed = text.trim_end();
    let last_line = trimmed.rsplit('\n').next().unwrap_or(trimmed);
    if last_line == SLACK_STAGING_MARK {
        return text.to_string();
    }
    if trimmed.is_empty() {
        return SLACK_STAGING_MARK.to_string();
    }
    format!("{trimmed}\n{SLACK_STAGING_MARK}")
}

/// A plain-text stand-in for a Block Kit payload. Slack requires a top-level
/// `text` alongside `blocks` (for notifications and screen readers), and an
/// implementor that cannot post blocks delivers this instead. The words come
/// from the blocks themselves — section text and fields, header text, context
/// text — so a degraded delivery degrades to the same message, and an
/// image-only payload degrades to empty rather than to invented copy.
#[must_use]
pub fn blocks_to_fallback_text(blocks: &[serde_json::Value]) -> String {
    let mut texts = Vec::new();
    for block in blocks {
        collect_block_text(block, &mut texts);
    }
    texts.join("\n")
}

fn collect_block_text(block: &serde_json::Value, texts: &mut Vec<String>) {
    let Some(object) = block.as_object() else {
        return;
    };
    match object.get("type").and_then(serde_json::Value::as_str) {
        Some("section") => {
            push_block_text(object.get("text"), texts);
            if let Some(fields) = object.get("fields").and_then(serde_json::Value::as_array) {
                for field in fields {
                    push_block_text(Some(field), texts);
                }
            }
        }
        Some("header") => push_block_text(object.get("text"), texts),
        Some("context") => {
            if let Some(elements) = object.get("elements").and_then(serde_json::Value::as_array) {
                for element in elements {
                    push_block_text(Some(element), texts);
                }
            }
        }
        _ => {}
    }
}

fn push_block_text(value: Option<&serde_json::Value>, texts: &mut Vec<String>) {
    if let Some(text) = value
        .and_then(|value| value.get("text"))
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        texts.push(text.to_string());
    }
}

#[derive(Debug, Error)]
pub enum SlackError {
    #[error("Slack credential is unavailable")]
    MissingCredential,
    #[error("Slack request failed")]
    Transport,
    #[error("Slack returned HTTP {0}")]
    HttpStatus(u16),
    #[error("Slack rejected the request")]
    Api,
    #[error("Slack returned an incomplete response")]
    IncompleteResponse,
    #[error("Slack channel name is already in use")]
    NameTaken,
    #[error("Slack returned a channel that is not private")]
    NotPrivate,
}

/// `is_archived` is `false` on every channel `find_private_channel` can
/// return — `conversations.list` is called with `exclude_archived: true`, so
/// a search result is never stale in that dimension. It only ever reads
/// `true` on the result of [`SlackService::get_channel`], which looks up an
/// already-recorded id directly rather than by name and so can observe a
/// channel a search would silently hide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackChannel {
    pub id: String,
    pub name: String,
    pub is_archived: bool,
    pub is_private: bool,
}

/// An ID issued by Slack for an eligible Firm participant. It is deliberately
/// not constructible from an email address in this module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SlackMemberId(String);

impl SlackMemberId {
    pub fn new(provider_id: impl Into<String>) -> Result<Self, SlackError> {
        let provider_id = provider_id.into();
        if provider_id.is_empty() || !provider_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(SlackError::Api);
        }
        Ok(Self(provider_id))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A Firm's Slack bot, addressed by channel name or id. The name is the
/// Project's environment-qualified channel name (`navigator-<code>` or
/// `staging-<code>`), never a bare Project code, so a deployment's channels
/// cannot collide with another's in the same workspace.
#[async_trait]
pub trait SlackService: Send + Sync {
    async fn find_private_channel(
        &self,
        channel_name: &str,
    ) -> Result<Option<SlackChannel>, SlackError>;
    async fn create_private_channel(&self, channel_name: &str) -> Result<SlackChannel, SlackError>;
    async fn invite_firm_members(
        &self,
        channel_id: &str,
        members: &[SlackMemberId],
    ) -> Result<(), SlackError>;
    async fn post_message(&self, channel_id: &str, text: &str) -> Result<(), SlackError>;
    /// Look up the exact channel Navigator already recorded, by id. Unlike a
    /// name search, this sees a channel even after it has been renamed away
    /// from its recorded name or archived — the two states a search-based
    /// lookup cannot distinguish from "never existed". `Ok(None)` means the
    /// id no longer resolves to any channel (deleted, or a foreign/malformed
    /// id).
    async fn get_channel(&self, channel_id: &str) -> Result<Option<SlackChannel>, SlackError>;
    /// Rename a channel the bot is a member of, keeping its id and history.
    /// The reconcile uses it to bring a recorded channel back to its
    /// canonical name (ENG-1286). Slack's `name_taken` is reported as such.
    async fn rename_channel(
        &self,
        channel_id: &str,
        channel_name: &str,
    ) -> Result<SlackChannel, SlackError>;
}

/// The canonical URL for a private channel, from its id. Slack's own
/// `/archives/{id}` form works for a channel the caller is a member of,
/// public or private, so it needs no workspace subdomain.
#[must_use]
pub fn channel_url(channel_id: &str) -> String {
    format!("https://slack.com/archives/{channel_id}")
}

/// Idempotent find-then-create channel provisioning. A failed lookup never
/// creates a second channel. The returned flag distinguishes a channel this
/// call created from one it adopted: an operator reading `ensure` output has
/// to be able to tell a first provisioning from a re-run, and an unconditional
/// `true` would report every re-run as a new Firm-private channel.
pub async fn ensure_private_channel<S: SlackService + ?Sized>(
    service: &S,
    channel_name: &str,
    members: &[SlackMemberId],
) -> Result<(SlackChannel, bool), SlackError> {
    let (channel, created) = match service.find_private_channel(channel_name).await? {
        Some(channel) => (channel, false),
        None => (service.create_private_channel(channel_name).await?, true),
    };
    if !channel.is_private {
        return Err(SlackError::NotPrivate);
    }
    if channel.name != channel_name {
        return Err(SlackError::Api);
    }
    service.invite_firm_members(&channel.id, members).await?;
    Ok((channel, created))
}

/// Storage is keyed by id, not by project code: a renamed channel keeps its
/// id but no longer carries the name that would let a name search find it,
/// and an archived channel keeps its id but drops out of every name search
/// too (`conversations.list` excludes archived channels). Both are real
/// Slack behaviors this fake has to reproduce for [`SlackService::get_channel`]
/// to be worth testing against.
#[derive(Debug, Clone, Default)]
pub struct FakeSlack {
    by_id: Arc<Mutex<BTreeMap<String, SlackChannel>>>,
    members: Arc<Mutex<BTreeMap<String, BTreeSet<String>>>>,
    messages: Arc<Mutex<Vec<(String, String)>>>,
    unavailable: Arc<Mutex<bool>>,
    next_id: Arc<Mutex<usize>>,
    /// Project codes whose name Slack reports as already taken by something
    /// this fake does not otherwise know about — a channel the bot cannot
    /// see (wrong workspace, or a scope the bot lacks), which is a real
    /// Slack refusal distinct from "unavailable".
    name_taken: Arc<Mutex<BTreeSet<String>>>,
}

impl FakeSlack {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_unavailable(&self, unavailable: bool) {
        *self.unavailable.lock().expect("Slack fake lock poisoned") = unavailable;
    }

    #[must_use]
    pub fn invited_members(&self, channel_id: &str) -> Vec<String> {
        self.members
            .lock()
            .expect("Slack fake lock poisoned")
            .get(channel_id)
            .map(|members| members.iter().cloned().collect())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn messages(&self) -> Vec<(String, String)> {
        self.messages
            .lock()
            .expect("Slack fake lock poisoned")
            .clone()
    }

    /// Simulate a name Slack refuses on create with `name_taken` — a
    /// channel the bot cannot see, not a transport failure.
    pub fn simulate_name_taken(&self, project_code: &str) {
        self.name_taken
            .lock()
            .expect("Slack fake lock poisoned")
            .insert(project_code.to_string());
    }

    /// Simulate someone archiving the channel by hand in Slack. It keeps its
    /// id (so [`SlackService::get_channel`] still finds it) but stops
    /// appearing in any name search, exactly like a real archived channel.
    pub fn archive(&self, channel_id: &str) {
        if let Some(channel) = self
            .by_id
            .lock()
            .expect("Slack fake lock poisoned")
            .get_mut(channel_id)
        {
            channel.is_archived = true;
        }
    }

    /// Simulate someone renaming the channel away from the Project code by
    /// hand. It keeps its id, so `get_channel` still finds it under its new
    /// name, but a name search for the old code no longer will.
    pub fn rename(&self, channel_id: &str, new_name: &str) {
        if let Some(channel) = self
            .by_id
            .lock()
            .expect("Slack fake lock poisoned")
            .get_mut(channel_id)
        {
            channel.name = new_name.to_string();
        }
    }

    /// Simulate a provider-side privacy change while the Project still
    /// records the channel id.
    pub fn set_public(&self, channel_id: &str) {
        if let Some(channel) = self
            .by_id
            .lock()
            .expect("Slack fake lock poisoned")
            .get_mut(channel_id)
        {
            channel.is_private = false;
        }
    }

    /// Simulate the provider deleting the channel while its id remains on the
    /// Project row.
    pub fn remove(&self, channel_id: &str) {
        self.by_id
            .lock()
            .expect("Slack fake lock poisoned")
            .remove(channel_id);
    }

    /// The current channel findable under `project_code` — `None` once that
    /// channel has been renamed or archived, matching what a name search
    /// would see.
    #[must_use]
    pub fn channel(&self, project_code: &str) -> Option<SlackChannel> {
        self.by_id
            .lock()
            .expect("Slack fake lock poisoned")
            .values()
            .find(|channel| !channel.is_archived && channel.name == project_code)
            .cloned()
    }

    fn check_available(&self) -> Result<(), SlackError> {
        if *self.unavailable.lock().expect("Slack fake lock poisoned") {
            Err(SlackError::Transport)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl SlackService for FakeSlack {
    async fn find_private_channel(
        &self,
        project_code: &str,
    ) -> Result<Option<SlackChannel>, SlackError> {
        self.check_available()?;
        Ok(self.channel(project_code))
    }

    async fn create_private_channel(&self, project_code: &str) -> Result<SlackChannel, SlackError> {
        self.check_available()?;
        if let Some(channel) = self.channel(project_code) {
            return Ok(channel);
        }
        if self
            .name_taken
            .lock()
            .expect("Slack fake lock poisoned")
            .contains(project_code)
        {
            return Err(SlackError::NameTaken);
        }
        let mut next_id = self.next_id.lock().expect("Slack fake lock poisoned");
        *next_id += 1;
        let channel = SlackChannel {
            id: format!("C{:016X}", *next_id),
            name: project_code.to_string(),
            is_archived: false,
            is_private: true,
        };
        self.by_id
            .lock()
            .expect("Slack fake lock poisoned")
            .insert(channel.id.clone(), channel.clone());
        Ok(channel)
    }

    async fn invite_firm_members(
        &self,
        channel_id: &str,
        members: &[SlackMemberId],
    ) -> Result<(), SlackError> {
        self.check_available()?;
        let mut invited = self.members.lock().expect("Slack fake lock poisoned");
        let ids = invited.entry(channel_id.to_string()).or_default();
        ids.extend(members.iter().map(|member| member.0.clone()));
        Ok(())
    }

    async fn post_message(&self, channel_id: &str, text: &str) -> Result<(), SlackError> {
        self.check_available()?;
        self.messages
            .lock()
            .expect("Slack fake lock poisoned")
            .push((channel_id.to_string(), text.to_string()));
        Ok(())
    }

    async fn get_channel(&self, channel_id: &str) -> Result<Option<SlackChannel>, SlackError> {
        self.check_available()?;
        Ok(self
            .by_id
            .lock()
            .expect("Slack fake lock poisoned")
            .get(channel_id)
            .cloned())
    }

    async fn rename_channel(
        &self,
        channel_id: &str,
        channel_name: &str,
    ) -> Result<SlackChannel, SlackError> {
        self.check_available()?;
        if self.channel(channel_name).is_some()
            || self
                .name_taken
                .lock()
                .expect("Slack fake lock poisoned")
                .contains(channel_name)
        {
            return Err(SlackError::NameTaken);
        }
        let mut by_id = self.by_id.lock().expect("Slack fake lock poisoned");
        let channel = by_id.get_mut(channel_id).ok_or(SlackError::Api)?;
        channel.name = channel_name.to_string();
        Ok(channel.clone())
    }
}

#[derive(Debug, Deserialize)]
struct ChannelResponse {
    ok: bool,
    #[serde(default)]
    channel: Option<SlackChannelBody>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackChannelBody {
    id: String,
    name: String,
    #[serde(default)]
    is_archived: bool,
    #[serde(default)]
    is_private: bool,
}

#[derive(Debug, Deserialize)]
struct BasicResponse {
    ok: bool,
    /// Slack's machine-readable error slug. Carried so a refusal can be told
    /// apart rather than reported as whatever the last caller assumed.
    #[serde(default)]
    error: Option<String>,
}

/// Slack Web API adapter. The bot token is injected by the Firm-secret
/// resolver; `SLACK_BOT_TOKEN` is never consulted here.
pub struct SlackClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
    from_staging: bool,
}

impl std::fmt::Debug for SlackClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlackClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl SlackClient {
    /// The production client. The bot token is the Firm's resolved provider
    /// credential, so `SLACK_BOT_TOKEN` is never consulted here. Staging
    /// labeling follows [`labels_slack_from_staging_env`].
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self::from_parts(token, SLACK_BASE_URL, labels_slack_from_staging_env())
    }

    /// Build a client against an explicit origin. Transport tests use this
    /// and do not inherit process staging labels, so a local
    /// `NAVIGATOR_SIMULATED_MATTERS=true` shell cannot rewrite their bodies.
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
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            from_staging,
        }
    }

    /// Slack's read methods are `GET` with query parameters, and they do not
    /// accept a JSON body — a JSON `POST` to `conversations.list` comes back
    /// as `invalid_form_data` rather than a channel list. The write methods
    /// below do accept JSON, so the two transports are separate here.
    async fn get(
        &self,
        method: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value, SlackError> {
        let response = self
            .http
            .get(format!("{}/{method}", self.base_url))
            .bearer_auth(&self.token)
            .query(query)
            .send()
            .await
            .map_err(|_| SlackError::Transport)?;
        if !response.status().is_success() {
            return Err(SlackError::HttpStatus(response.status().as_u16()));
        }
        response.json().await.map_err(|_| SlackError::Transport)
    }

    async fn post(
        &self,
        method: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, SlackError> {
        let response = self
            .http
            .post(format!("{}/{method}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|_| SlackError::Transport)?;
        if !response.status().is_success() {
            return Err(SlackError::HttpStatus(response.status().as_u16()));
        }
        response.json().await.map_err(|_| SlackError::Transport)
    }
}

impl SlackClient {
    /// `chat.postMessage` carrying a Block Kit payload. The top-level `text`
    /// is the fallback derived from the blocks themselves, so notifications
    /// and screen readers preview the same words the blocks render.
    pub async fn post_blocks(
        &self,
        channel_id: &str,
        blocks: &[serde_json::Value],
    ) -> Result<(), SlackError> {
        let body = self
            .post(
                "chat.postMessage",
                json!({
                    "channel": channel_id,
                    "text": blocks_to_fallback_text(blocks),
                    "blocks": blocks,
                    "unfurl_links": false,
                    "unfurl_media": false,
                }),
            )
            .await?;
        let response: BasicResponse =
            serde_json::from_value(body).map_err(|_| SlackError::IncompleteResponse)?;
        if response.ok {
            Ok(())
        } else {
            Err(SlackError::Api)
        }
    }

    /// The private file-upload flow (Slack's v2 external upload): request a
    /// one-use upload URL, post the raw bytes to it, then complete the
    /// upload and return the file id an image block references through
    /// `slack_file`. The bytes never touch a publicly fetchable URL —
    /// confidential content stays inside Slack, per the jj build-out's
    /// locked image decision.
    ///
    /// The upload step is an HTTP `POST` of raw bytes to the returned
    /// `upload_url` (`application/octet-stream`), per Slack's documented
    /// v2 contract; no bearer header is sent to that pre-issued URL.
    pub async fn upload_file(&self, filename: &str, bytes: &[u8]) -> Result<String, SlackError> {
        let body = self
            .post(
                "files.getUploadURLExternal",
                json!({ "filename": filename, "length": bytes.len() }),
            )
            .await?;
        let requested: UploadUrlResponse =
            serde_json::from_value(body).map_err(|_| SlackError::IncompleteResponse)?;
        if !requested.ok {
            return Err(SlackError::Api);
        }
        let upload_url = requested.upload_url.ok_or(SlackError::IncompleteResponse)?;
        let file_id = requested.file_id.ok_or(SlackError::IncompleteResponse)?;

        let uploaded = self
            .http
            .post(&upload_url)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| SlackError::Transport)?;
        if !uploaded.status().is_success() {
            return Err(SlackError::HttpStatus(uploaded.status().as_u16()));
        }

        let body = self
            .post(
                "files.completeUploadExternal",
                json!({ "files": [{ "id": file_id, "title": filename }] }),
            )
            .await?;
        let response: BasicResponse =
            serde_json::from_value(body).map_err(|_| SlackError::IncompleteResponse)?;
        if response.ok {
            Ok(file_id)
        } else {
            Err(SlackError::Api)
        }
    }
}

#[derive(Debug, Deserialize)]
struct UploadUrlResponse {
    ok: bool,
    #[serde(default)]
    upload_url: Option<String>,
    #[serde(default)]
    file_id: Option<String>,
}

#[async_trait]
impl SlackService for SlackClient {
    async fn find_private_channel(
        &self,
        project_code: &str,
    ) -> Result<Option<SlackChannel>, SlackError> {
        let mut cursor = String::new();
        for _ in 0..MAX_CHANNEL_PAGES {
            let mut query = vec![
                ("types", "private_channel".to_string()),
                ("limit", CHANNEL_PAGE_SIZE.to_string()),
                ("exclude_archived", "true".to_string()),
            ];
            if !cursor.is_empty() {
                query.push(("cursor", cursor.clone()));
            }
            let body = self.get("conversations.list", &query).await?;
            let response: ChannelListResponse =
                serde_json::from_value(body).map_err(|_| SlackError::IncompleteResponse)?;
            if !response.ok {
                return Err(SlackError::Api);
            }
            if let Some(channel) = response
                .channels
                .into_iter()
                .find(|channel| channel.name == project_code)
            {
                return Ok(Some(SlackChannel {
                    id: channel.id,
                    name: channel.name,
                    is_archived: channel.is_archived,
                    is_private: channel.is_private,
                }));
            }
            // An empty `next_cursor` is how Slack says "last page"; it is
            // present and empty rather than absent, so both are the end.
            match response.response_metadata.and_then(|meta| meta.next_cursor) {
                Some(next) if !next.is_empty() => cursor = next,
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    async fn create_private_channel(&self, project_code: &str) -> Result<SlackChannel, SlackError> {
        let body = self
            .post(
                "conversations.create",
                json!({ "name": project_code, "is_private": true }),
            )
            .await?;
        let response: ChannelResponse =
            serde_json::from_value(body).map_err(|_| SlackError::IncompleteResponse)?;
        if !response.ok {
            // Only `name_taken` means the name is taken. Reporting every
            // refusal as a name collision sends an operator to rename a
            // channel when the real answer is a missing scope or a revoked
            // token.
            return Err(if response.error.as_deref() == Some("name_taken") {
                SlackError::NameTaken
            } else {
                SlackError::Api
            });
        }
        let channel = response.channel.ok_or(SlackError::IncompleteResponse)?;
        if !channel.is_private {
            return Err(SlackError::NotPrivate);
        }
        if channel.name != project_code {
            return Err(SlackError::Api);
        }
        Ok(SlackChannel {
            id: channel.id,
            name: channel.name,
            is_archived: channel.is_archived,
            is_private: channel.is_private,
        })
    }

    async fn invite_firm_members(
        &self,
        channel_id: &str,
        members: &[SlackMemberId],
    ) -> Result<(), SlackError> {
        if members.is_empty() {
            return Ok(());
        }
        let body = self
            .post(
                "conversations.invite",
                json!({
                    "channel": channel_id,
                    "users": members.iter().map(SlackMemberId::as_str).collect::<Vec<_>>().join(",")
                }),
            )
            .await?;
        let response: BasicResponse =
            serde_json::from_value(body).map_err(|_| SlackError::IncompleteResponse)?;
        // `already_in_channel` is what Slack answers when the member this
        // call names is already there, which is the steady state of an
        // idempotent ensure — treating it as a failure would make every
        // re-run of a provisioned channel report an error.
        if response.ok || response.error.as_deref() == Some("already_in_channel") {
            Ok(())
        } else {
            Err(SlackError::Api)
        }
    }

    async fn post_message(&self, channel_id: &str, text: &str) -> Result<(), SlackError> {
        let text = outbound_slack_text(text, self.from_staging);
        let body = self
            .post(
                "chat.postMessage",
                json!({ "channel": channel_id, "text": text }),
            )
            .await?;
        let response: BasicResponse =
            serde_json::from_value(body).map_err(|_| SlackError::IncompleteResponse)?;
        if response.ok {
            Ok(())
        } else {
            Err(SlackError::Api)
        }
    }

    /// `conversations.info` — the direct lookup [`SlackService::get_channel`]
    /// promises. Slack answers a dead id with `ok: false, error:
    /// "channel_not_found"`, which maps to `Ok(None)` rather than an error
    /// since "gone" is an answer this call is specifically meant to give;
    /// every other refusal is a real transport/API failure.
    async fn get_channel(&self, channel_id: &str) -> Result<Option<SlackChannel>, SlackError> {
        let body = self
            .get("conversations.info", &[("channel", channel_id.to_string())])
            .await?;
        let response: ChannelResponse =
            serde_json::from_value(body).map_err(|_| SlackError::IncompleteResponse)?;
        if !response.ok {
            // Only a dead id means "gone". Every other refusal (a revoked
            // scope, a bad token) is a real failure and must not be reported
            // as if the channel had simply disappeared.
            return if response.error.as_deref() == Some("channel_not_found") {
                Ok(None)
            } else {
                Err(SlackError::Api)
            };
        }
        let channel = response.channel.ok_or(SlackError::IncompleteResponse)?;
        Ok(Some(SlackChannel {
            id: channel.id,
            name: channel.name,
            is_archived: channel.is_archived,
            is_private: channel.is_private,
        }))
    }

    /// `conversations.rename` — the id survives, so the recorded coordinate
    /// and the channel's history do too.
    async fn rename_channel(
        &self,
        channel_id: &str,
        channel_name: &str,
    ) -> Result<SlackChannel, SlackError> {
        let body = self
            .post(
                "conversations.rename",
                json!({ "channel": channel_id, "name": channel_name }),
            )
            .await?;
        let response: ChannelResponse =
            serde_json::from_value(body).map_err(|_| SlackError::IncompleteResponse)?;
        if !response.ok {
            return Err(if response.error.as_deref() == Some("name_taken") {
                SlackError::NameTaken
            } else {
                SlackError::Api
            });
        }
        let channel = response.channel.ok_or(SlackError::IncompleteResponse)?;
        Ok(SlackChannel {
            id: channel.id,
            name: channel.name,
            is_archived: channel.is_archived,
            is_private: channel.is_private,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ChannelListResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    channels: Vec<SlackChannelBody>,
    #[serde(default)]
    response_metadata: Option<ResponseMetadata>,
}

#[derive(Debug, Deserialize)]
struct ResponseMetadata {
    #[serde(default)]
    next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        blocks_to_fallback_text, ensure_private_channel, labels_slack_from_staging_from,
        outbound_slack_text, FakeSlack, SlackClient, SlackError, SlackMemberId, SlackService,
        SLACK_STAGING_MARK,
    };
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Slack's read methods are `GET` with query parameters. This asserts the
    /// transport and the cursor: the channel lives on the second page, so a
    /// client that read one page and stopped would report it missing — and
    /// `ensure` would then create a duplicate of a channel that exists.
    #[tokio::test]
    async fn channel_lookup_is_a_get_that_follows_the_cursor() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/conversations.list"))
            .and(query_param("cursor", "page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "channels": [{ "id": "C2", "name": "sample-project", "is_private": true }],
                "response_metadata": { "next_cursor": "" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/conversations.list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "channels": [{ "id": "C1", "name": "another-matter", "is_private": true }],
                "response_metadata": { "next_cursor": "page-2" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = SlackClient::with_base_url("test-token", server.uri());
        let found = client
            .find_private_channel("sample-project")
            .await
            .unwrap()
            .expect("the channel on the second page is found");
        assert_eq!(found.id, "C2");
    }

    /// Only `name_taken` means the name is taken. A missing scope reported as
    /// a name collision sends an operator to rename a channel that is fine.
    #[tokio::test]
    async fn a_refusal_is_reported_as_itself_not_as_a_name_collision() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/conversations.create"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": false, "error": "missing_scope" })),
            )
            .mount(&server)
            .await;
        let client = SlackClient::with_base_url("test-token", server.uri());
        assert!(matches!(
            client.create_private_channel("sample-project").await,
            Err(SlackError::Api)
        ));
    }

    /// A member already in the channel is the steady state of an idempotent
    /// ensure, so the re-run must succeed rather than report a failure.
    #[tokio::test]
    async fn an_invite_for_an_existing_member_is_not_a_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/conversations.invite"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "ok": false, "error": "already_in_channel" }),
                ),
            )
            .mount(&server)
            .await;
        let client = SlackClient::with_base_url("test-token", server.uri());
        client
            .invite_firm_members("C1", &[SlackMemberId::new("U123").unwrap()])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn private_channel_invites_only_external_firm_ids_and_is_idempotent() {
        let slack = FakeSlack::new();
        let members = vec![
            SlackMemberId::new("U123").unwrap(),
            SlackMemberId::new("U456").unwrap(),
        ];
        let (first, created) = ensure_private_channel(&slack, "sample-project", &members)
            .await
            .unwrap();
        let (second, created_again) = ensure_private_channel(&slack, "sample-project", &members)
            .await
            .unwrap();
        assert_eq!(first, second);
        assert!(created, "the first ensure creates the Firm-private channel");
        assert!(
            !created_again,
            "a re-run adopts the existing channel and must not report a creation"
        );
        assert_eq!(slack.invited_members(&first.id), vec!["U123", "U456"]);
    }

    #[tokio::test]
    async fn a_public_channel_with_the_project_name_is_refused() {
        let slack = FakeSlack::new();
        let (channel, _) = ensure_private_channel(&slack, "sample-project", &[])
            .await
            .unwrap();
        slack.set_public(&channel.id);
        assert!(matches!(
            ensure_private_channel(&slack, "sample-project", &[]).await,
            Err(SlackError::NotPrivate)
        ));
    }

    #[tokio::test]
    async fn unavailable_lookup_does_not_create_or_invite() {
        let slack = FakeSlack::new();
        slack.set_unavailable(true);
        let member = SlackMemberId::new("U123").unwrap();
        assert!(matches!(
            ensure_private_channel(&slack, "sample-project", &[member]).await,
            Err(SlackError::Transport)
        ));
        assert!(slack.messages().is_empty());
    }

    #[test]
    fn member_ids_reject_email_shaped_values() {
        assert!(SlackMemberId::new("person@example.com").is_err());
    }

    /// Persistent staging is production-profile plus an explicit simulated
    /// matters flag. Local KIND (`dev`) and real production do not label.
    #[test]
    fn staging_slack_label_follows_simulated_matters_on_production_profile() {
        let lookup = |pairs: &[(&str, &str)]| {
            let owned: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            move |key: &str| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        };
        assert!(
            labels_slack_from_staging_from(lookup(&[("NAVIGATOR_SIMULATED_MATTERS", "true")])),
            "unset NAVIGATOR_ENVIRONMENT is production"
        );
        assert!(labels_slack_from_staging_from(lookup(&[
            ("NAVIGATOR_ENVIRONMENT", "production"),
            ("NAVIGATOR_SIMULATED_MATTERS", "true"),
        ])));
        assert!(!labels_slack_from_staging_from(lookup(&[
            ("NAVIGATOR_ENVIRONMENT", "dev"),
            ("NAVIGATOR_SIMULATED_MATTERS", "true"),
        ])));
        assert!(!labels_slack_from_staging_from(lookup(&[(
            "NAVIGATOR_ENVIRONMENT",
            "production"
        )])));
        assert!(!labels_slack_from_staging_from(lookup(&[
            ("NAVIGATOR_ENVIRONMENT", "production"),
            ("NAVIGATOR_SIMULATED_MATTERS", "false"),
        ])));
    }

    #[test]
    fn outbound_slack_text_appends_from_staging_once() {
        assert_eq!(outbound_slack_text("hello", false), "hello");
        assert_eq!(
            outbound_slack_text("hello", true),
            format!("hello\n{SLACK_STAGING_MARK}")
        );
        assert_eq!(
            outbound_slack_text("hello\nfrom Staging", true),
            "hello\nfrom Staging"
        );
        assert_eq!(outbound_slack_text("", true), SLACK_STAGING_MARK);
        assert_eq!(
            outbound_slack_text("*Heartbeat*\n```\nok\n```", true),
            format!("*Heartbeat*\n```\nok\n```\n{SLACK_STAGING_MARK}")
        );
    }

    #[tokio::test]
    async fn staging_post_message_appends_from_staging() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .and(body_partial_json(serde_json::json!({
                "channel": "C1",
                "text": "A client viewed this Project in the portal.\nfrom Staging"
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = SlackClient::with_staging_origin("test-token", server.uri(), true);
        client
            .post_message("C1", "A client viewed this Project in the portal.")
            .await
            .unwrap();
    }

    /// A name search can never see an archived or renamed channel —
    /// `conversations.list` is called with `exclude_archived: true`, and a
    /// rename means the name no longer matches the query. `get_channel` is
    /// the only lookup that still finds either, by going straight to the
    /// recorded id.
    #[tokio::test]
    async fn get_channel_sees_an_archived_or_renamed_channel_a_search_would_miss() {
        let slack = FakeSlack::new();
        let (channel, _) = ensure_private_channel(&slack, "sample-project", &[])
            .await
            .unwrap();

        slack.archive(&channel.id);
        assert_eq!(
            slack.find_private_channel("sample-project").await.unwrap(),
            None
        );
        let looked_up = slack
            .get_channel(&channel.id)
            .await
            .unwrap()
            .expect("still resolves by id");
        assert!(looked_up.is_archived);
        assert_eq!(looked_up.name, "sample-project");

        let slack = FakeSlack::new();
        let (channel, _) = ensure_private_channel(&slack, "renamed-project", &[])
            .await
            .unwrap();
        slack.rename(&channel.id, "someone-renamed-this");
        assert_eq!(
            slack.find_private_channel("renamed-project").await.unwrap(),
            None
        );
        let looked_up = slack
            .get_channel(&channel.id)
            .await
            .unwrap()
            .expect("still resolves by id");
        assert!(!looked_up.is_archived);
        assert_eq!(looked_up.name, "someone-renamed-this");
    }

    /// ENG-1286: a rename keeps the id, so a recorded coordinate survives it,
    /// and a name another channel already holds is refused as `name_taken`.
    #[tokio::test]
    async fn rename_keeps_the_id_and_refuses_a_taken_name() {
        let slack = FakeSlack::new();
        let (channel, _) = ensure_private_channel(&slack, "project-sample", &[])
            .await
            .unwrap();
        let renamed = slack
            .rename_channel(&channel.id, "navigator-sample")
            .await
            .unwrap();
        assert_eq!(renamed.id, channel.id);
        assert_eq!(renamed.name, "navigator-sample");
        assert_eq!(
            slack.find_private_channel("project-sample").await.unwrap(),
            None
        );
        let (other, _) = ensure_private_channel(&slack, "navigator-other", &[])
            .await
            .unwrap();
        assert!(matches!(
            slack.rename_channel(&other.id, "navigator-sample").await,
            Err(SlackError::NameTaken)
        ));
        assert!(matches!(
            slack.rename_channel("never-created", "navigator-x").await,
            Err(SlackError::Api)
        ));
    }

    #[tokio::test]
    async fn rename_posts_the_channel_and_name_and_reports_a_taken_name() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/conversations.rename"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "channel": "C1",
                "name": "navigator-sample"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "channel": { "id": "C1", "name": "navigator-sample", "is_private": true, "is_archived": false }
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/conversations.rename"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "channel": "C2",
                "name": "navigator-taken"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "error": "name_taken"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = SlackClient::with_base_url("test-token", server.uri());
        let renamed = client
            .rename_channel("C1", "navigator-sample")
            .await
            .unwrap();
        assert_eq!(
            (renamed.id.as_str(), renamed.name.as_str()),
            ("C1", "navigator-sample")
        );
        assert!(matches!(
            client.rename_channel("C2", "navigator-taken").await,
            Err(SlackError::NameTaken)
        ));
    }

    #[tokio::test]
    async fn get_channel_returns_none_for_an_id_that_was_never_created() {
        let slack = FakeSlack::new();
        assert_eq!(slack.get_channel("never-created").await.unwrap(), None);
    }

    #[test]
    fn channel_url_is_the_archives_form() {
        assert_eq!(
            super::channel_url("C0123456789"),
            "https://slack.com/archives/C0123456789"
        );
    }

    #[test]
    fn blocks_to_fallback_text_carries_the_blocks_own_words() {
        let blocks = serde_json::json!([
            {
                "type": "header",
                "text": { "type": "plain_text", "text": "Push to sample-project" }
            },
            {
                "type": "section",
                "text": { "type": "mrkdwn", "text": "*qvrpxyq* — amend the filing" },
                "fields": [
                    { "type": "mrkdwn", "text": "*Author:* T-bot" },
                    { "type": "plain_text", "text": "+12 −3" }
                ]
            },
            {
                "type": "context",
                "elements": [{ "type": "mrkdwn", "text": "by Navigator" }]
            },
            { "type": "image", "image_url": "irrelevant", "alt_text": "chart" }
        ]);
        assert_eq!(
            blocks_to_fallback_text(blocks.as_array().unwrap()),
            "Push to sample-project\n*qvrpxyq* — amend the filing\n*Author:* T-bot\n+12 −3\nby Navigator"
        );
    }

    #[test]
    fn blocks_without_text_degrade_to_an_empty_fallback() {
        let blocks = serde_json::json!([
            { "type": "image", "image_url": "internal", "alt_text": "diff chart" },
            { "type": "divider" }
        ]);
        assert_eq!(blocks_to_fallback_text(blocks.as_array().unwrap()), "");
        assert_eq!(blocks_to_fallback_text(&[]), "");
    }

    /// `chat.postMessage` must carry the blocks themselves plus the derived
    /// fallback text — Slack renders blocks only when a top-level `text`
    /// accompanies them.
    #[tokio::test]
    async fn post_blocks_posts_the_block_kit_payload() {
        use wiremock::matchers::{body_partial_json, method, path};

        let blocks = serde_json::json!([
            { "type": "header", "text": { "type": "plain_text", "text": "Push to sample-project" } },
            { "type": "section", "text": { "type": "mrkdwn", "text": "*qvrpxyq* — amend the filing" } }
        ]);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .and(body_partial_json(serde_json::json!({
                "channel": "C1",
                "text": "Push to sample-project\n*qvrpxyq* — amend the filing",
                "blocks": blocks
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "channel": "C1", "ts": "1773000000.000100"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = SlackClient::with_base_url("test-token", server.uri());
        client
            .post_blocks("C1", blocks.as_array().unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_blocks_refusal_is_an_api_error() {
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": false, "error": "invalid_blocks" })),
            )
            .mount(&server)
            .await;
        let client = SlackClient::with_base_url("test-token", server.uri());
        assert!(matches!(
            client.post_blocks("C1", &[]).await,
            Err(SlackError::Api)
        ));
    }

    /// The private upload flow: request the one-use URL, post the raw bytes
    /// to it, complete the upload, and keep the file id an image block
    /// references through `slack_file` — no publicly fetchable URL anywhere.
    #[tokio::test]
    async fn the_upload_flow_completes_and_yields_the_file_id() {
        use wiremock::matchers::{body_partial_json, header, method, path};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/files.getUploadURLExternal"))
            .and(body_partial_json(
                serde_json::json!({ "filename": "diff-chart.png", "length": 12 }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "upload_url": format!("{}/files-upload/one-use", server.uri()),
                "file_id": "F012AB3CDE4"
            })))
            .expect(1)
            .mount(&server)
            .await;
        // The byte upload goes to Slack's file-upload service, not the Web
        // API origin: no bearer header, raw octet-stream body.
        Mock::given(method("POST"))
            .and(path("/files-upload/one-use"))
            .and(header("content-type", "application/octet-stream"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/files.completeUploadExternal"))
            .and(body_partial_json(serde_json::json!({
                "files": [{ "id": "F012AB3CDE4", "title": "diff-chart.png" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "files": [{ "id": "F012AB3CDE4" }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = SlackClient::with_base_url("test-token", server.uri());
        let file_id = client
            .upload_file("diff-chart.png", b"PNG-DATA-12\n")
            .await
            .unwrap();
        assert_eq!(file_id, "F012AB3CDE4");
    }

    #[tokio::test]
    async fn an_upload_url_refusal_fails_before_any_bytes_move() {
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/files.getUploadURLExternal"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": false, "error": "not_allowed" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = SlackClient::with_base_url("test-token", server.uri());
        assert!(matches!(
            client.upload_file("diff-chart.png", b"PNG-DATA").await,
            Err(SlackError::Api)
        ));
    }
}
