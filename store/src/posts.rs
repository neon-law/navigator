//! Published and draft Blog Posts scoped to a registered Brand.
//!
//! The `post` row is the Post: its Markdown, schedule, captions, and
//! social links live in the database. The historical Posts are applied from
//! `store/seeds/neon/Post.yaml` by [`seed_post`], which only creates a
//! missing Post. [`import_source`] is the one write seam and keeps a Post's
//! stable Brand+slug identity, so the worker's `published` promotion, the row
//! id every `social_publication` receipt is keyed by, and the receipts
//! themselves survive an edit.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::firm_capability::{FirmCapability, FirmCapabilityDecision};
use crate::persons::Role;
use crate::surreal::{record_id, record_uuid, SurrealDb};

const TABLE: &str = "post";
const SELECT: &str = "id, brand_key, slug, title, description, body_html, body_markdown, status, publish_at, inserted_at, updated_at, video_asset, vertical_video, tiktok_ai_generated, video_orientation, facebook_caption, instagram_caption, threads_caption, youtube_caption, selected_social_accounts, source_digest, social_links";
const VIDEO_TABLE: &str = "blog_video";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Post {
    pub id: Uuid,
    pub brand_key: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub body_html: String,
    /// The authored Markdown; rendered and sanitized when served.
    pub body_markdown: Option<String>,
    pub status: String,
    pub publish_at: Option<DateTime<Utc>>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub video_asset: Option<String>,
    /// The author's declaration that `video_asset` is portrait media.
    /// Historical rows without a declaration read `false`.
    pub vertical_video: bool,
    /// Explicit editorial AI-content disclosure for TikTok; absent blocks publishing.
    pub tiktok_ai_generated: Option<bool>,
    /// The orientation measured from the uploaded video's own track
    /// header, when this deployment holds the upload's [`BlogVideo`] row.
    pub video_orientation: Option<String>,
    pub facebook_caption: Option<String>,
    pub instagram_caption: Option<String>,
    pub threads_caption: Option<String>,
    pub youtube_caption: Option<String>,
    /// Social account record keys chosen for this post.
    pub selected_social_accounts: Vec<String>,
    /// Digest of the editorial source last imported into this row.
    pub source_digest: Option<String>,
    /// Confirmed provider links the source carries, shown as display data.
    pub social_links: Vec<SocialLink>,
}

/// One Post's link to a social provider. Display data only: recording it never
/// sends. `account` is the Brand's [`crate::social_handles`] handle for the
/// provider, which must already exist. `post_id` and `permalink` are `None`
/// until the provider post is known, and are encoded after the fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, SurrealValue)]
pub struct SocialLink {
    pub provider: String,
    pub account: String,
    pub post_id: Option<String>,
    pub permalink: Option<String>,
}

/// A validated public provider link, without account credentials or inferred handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicSocialLink {
    pub provider: String,
    pub permalink: String,
}

/// Public links for this Post alone. Explicit valid links take precedence over
/// confirmed publication receipts; incomplete and unconfirmed receipts stay private.
#[must_use]
pub fn public_social_links(
    post: &Post,
    publications: &[crate::social_publications::SocialPublication],
) -> Vec<PublicSocialLink> {
    let mut links = std::collections::BTreeMap::new();
    for link in &post.social_links {
        if validate_social_link(link).is_ok() {
            if let Some(permalink) = &link.permalink {
                links
                    .entry(link.provider.clone())
                    .or_insert_with(|| permalink.clone());
            }
        }
    }
    let mut receipts: Vec<_> = publications
        .iter()
        .filter(|publication| {
            publication.post_id == post.id
                && publication.state == crate::social_publications::CONFIRMED
        })
        .collect();
    receipts.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    for publication in receipts {
        let (Some(post_id), Some(permalink)) =
            (&publication.provider_post_id, &publication.permalink)
        else {
            continue;
        };
        if validate_publication_link(&publication.provider, post_id, permalink).is_ok() {
            links
                .entry(publication.provider.clone())
                .or_insert_with(|| permalink.clone());
        }
    }
    links
        .into_iter()
        .map(|(provider, permalink)| PublicSocialLink {
            provider,
            permalink,
        })
        .collect()
}

#[derive(SurrealValue)]
struct PostRow {
    id: surrealdb::types::RecordId,
    brand_key: String,
    slug: String,
    title: String,
    description: String,
    body_html: String,
    body_markdown: Option<String>,
    status: String,
    publish_at: Option<surrealdb::types::Datetime>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
    video_asset: Option<String>,
    vertical_video: Option<bool>,
    tiktok_ai_generated: Option<bool>,
    video_orientation: Option<String>,
    facebook_caption: Option<String>,
    instagram_caption: Option<String>,
    threads_caption: Option<String>,
    youtube_caption: Option<String>,
    selected_social_accounts: Option<Vec<String>>,
    source_digest: Option<String>,
    social_links: Option<Vec<SocialLink>>,
}

impl PostRow {
    fn into_post(self) -> Option<Post> {
        Some(Post {
            id: record_uuid(&self.id)?,
            brand_key: self.brand_key,
            slug: self.slug,
            title: self.title,
            description: self.description,
            body_html: self.body_html,
            body_markdown: self.body_markdown,
            status: self.status,
            publish_at: self.publish_at.map(Into::into),
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
            video_asset: self.video_asset,
            vertical_video: self.vertical_video.unwrap_or(false),
            tiktok_ai_generated: self.tiktok_ai_generated,
            video_orientation: self.video_orientation,
            facebook_caption: self.facebook_caption,
            instagram_caption: self.instagram_caption,
            threads_caption: self.threads_caption,
            youtube_caption: self.youtube_caption,
            selected_social_accounts: self.selected_social_accounts.unwrap_or_default(),
            source_digest: self.source_digest,
            social_links: self.social_links.unwrap_or_default(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PostError {
    #[error("database: {0}")]
    Db(String),
    #[error("not allowed")]
    Denied,
    #[error("the Brand was not found")]
    BrandNotFound,
    #[error("the Blog Post was not found")]
    NotFound,
    #[error("invalid Blog Post status")]
    InvalidStatus,
    #[error("social account `{0}` belongs to another Brand")]
    CrossBrandSocialAccount(String),
    #[error("video asset `{0}` was uploaded for another Brand")]
    CrossBrandVideo(String),
    #[error("video asset `{0}` is not a registered upload in this deployment")]
    VideoAssetMissing(String),
    #[error("vertical_video is {declared}, but the uploaded video measures {measured}")]
    VideoOrientationMismatch { declared: bool, measured: String },
    #[error("{provider} is not an eligible destination: {reason}")]
    IneligibleDestination {
        provider: String,
        reason: &'static str,
    },
    #[error("the Brand has no {provider} handle recorded; record it before linking a Post to it")]
    UnknownSocialHandle { provider: String },
    #[error("the {provider} link names `{declared}`, but the Brand's handle is `{handle}`")]
    HandleMismatch {
        provider: String,
        declared: String,
        handle: String,
    },
    #[error("invalid social link: {0}")]
    InvalidSocialLink(String),
    #[error("invalid Post copy: {0}")]
    InvalidCopy(String),
    #[error("the {provider} link names {declared}, but the confirmed publication is {confirmed}")]
    ReceiptMismatch {
        provider: String,
        declared: String,
        confirmed: String,
    },
    #[error("the Post is already public; its publication time cannot move from {current} to {requested}")]
    ScheduleLocked {
        current: DateTime<Utc>,
        requested: DateTime<Utc>,
    },
}

fn scheduled_is_live(status: &str, publish_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    status == "scheduled" && publish_at.is_some_and(|at| at <= now)
}

/// Hold a future scheduled publication without deleting the Post.
///
/// Cancel is the one runtime action on a Post's schedule: a safety stop
/// before the publisher sends. [`import_source`] keeps the hold while the
/// source's schedule is unchanged; a new `publish_at` in Git schedules again.
pub async fn cancel(
    db: &SurrealDb,
    actor_role: Role,
    actor_person_id: Uuid,
    post_id: Uuid,
) -> Result<(), PostError> {
    let post = post_by_id(db, post_id).await?;
    authorize_brand(db, actor_role, actor_person_id, &post.brand_key).await?;
    if post.status != "scheduled" || post.publish_at.is_some_and(|at| at <= Utc::now()) {
        return Err(PostError::InvalidStatus);
    }
    db.query("UPDATE $id SET status = 'cancelled', updated_at = time::now()")
        .bind(("id", record_id(TABLE, post_id)))
        .await?
        .check()?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::Post(post_id)).await;
    Ok(())
}

async fn authorize_brand(
    db: &SurrealDb,
    actor_role: Role,
    actor_person_id: Uuid,
    brand_key: &str,
) -> Result<(), PostError> {
    let brand = crate::brands::find_by_key(db, brand_key)
        .await
        .map_err(|error| PostError::Db(error.to_string()))?
        .ok_or(PostError::BrandNotFound)?;
    let Some(firm_id) = brand.firm_id else {
        return if actor_role == Role::Owner {
            Ok(())
        } else {
            Err(PostError::Denied)
        };
    };
    let decision = FirmCapability::ManageBrandContent
        .resolve(db, actor_role, Some(actor_person_id), firm_id)
        .await
        .map_err(|error| PostError::Db(error.to_string()))?;
    if decision == FirmCapabilityDecision::Allowed {
        Ok(())
    } else {
        Err(PostError::Denied)
    }
}

async fn post_by_id(db: &SurrealDb, post_id: Uuid) -> Result<Post, PostError> {
    let mut response = db
        .query(format!("SELECT {SELECT} FROM ONLY $id"))
        .bind(("id", record_id(TABLE, post_id)))
        .await?
        .check()?;
    let row: Option<PostRow> = response.take(0)?;
    row.and_then(PostRow::into_post).ok_or(PostError::NotFound)
}

/// Load one Blog Post by its stable id for durable publication workflows.
pub async fn find_by_id(db: &SurrealDb, post_id: Uuid) -> Result<Option<Post>, PostError> {
    match post_by_id(db, post_id).await {
        Ok(post) => Ok(Some(post)),
        Err(PostError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

impl From<surrealdb::Error> for PostError {
    fn from(value: surrealdb::Error) -> Self {
        Self::Db(value.to_string())
    }
}

/// Public posts visible to this registered Brand, newest first.
pub async fn published_for_brand(
    db: &SurrealDb,
    brand_key: &str,
    now: DateTime<Utc>,
) -> Result<Vec<Post>, PostError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE brand_key = $brand \
             AND (status = 'published' OR (status = 'scheduled' AND publish_at <= $now)) \
             AND (publish_at IS NONE OR publish_at <= $now) ORDER BY publish_at DESC, inserted_at DESC"
        ))
        .bind(("brand", brand_key.to_string()))
        .bind(("now", surrealdb::types::Datetime::from(now)))
        .await?
        .check()?;
    let rows: Vec<PostRow> = response.take(0)?;
    Ok(rows.into_iter().filter_map(PostRow::into_post).collect())
}

/// The full catalog the actor may manage. Each row is independently checked
/// against its Brand's owning Firm.
pub async fn for_actor(
    db: &SurrealDb,
    actor_role: Role,
    actor_person_id: Uuid,
) -> Result<Vec<Post>, PostError> {
    let keys = manageable_brand_keys(db, actor_role, actor_person_id).await?;
    let mut posts = Vec::new();
    let now = Utc::now();
    for key in keys {
        posts.extend(
            posts_for_brand(db, &key)
                .await?
                .into_iter()
                .map(|mut post| {
                    if scheduled_is_live(&post.status, post.publish_at, now) {
                        post.status = "published".into();
                    }
                    post
                }),
        );
    }
    posts.sort_by_key(|post| std::cmp::Reverse(post.updated_at));
    Ok(posts)
}

/// Registered Firm-scoped Brand keys for which this actor has publication
/// authority. Used to build the form's selector as well as server-side writes.
pub async fn manageable_brand_keys(
    db: &SurrealDb,
    actor_role: Role,
    actor_person_id: Uuid,
) -> Result<Vec<String>, PostError> {
    let brands = crate::brands::all_firm_scoped(db)
        .await
        .map_err(|error| PostError::Db(error.to_string()))?;
    let mut keys = Vec::new();
    for brand in brands {
        let Some(firm_id) = brand.firm_id else {
            continue;
        };
        let decision = FirmCapability::ManageBrandContent
            .resolve(db, actor_role, Some(actor_person_id), firm_id)
            .await
            .map_err(|error| PostError::Db(error.to_string()))?;
        if decision == FirmCapabilityDecision::Allowed {
            keys.push(brand.key);
        }
    }
    Ok(keys)
}

/// One Brand's projected Post by slug, whatever its status.
pub async fn find_by_brand_slug(
    db: &SurrealDb,
    brand_key: &str,
    slug: &str,
) -> Result<Option<Post>, PostError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE brand_key = $brand AND slug = $slug LIMIT 1"
        ))
        .bind(("brand", brand_key.to_string()))
        .bind(("slug", slug.to_string()))
        .await?
        .check()?;
    let row: Option<PostRow> = response.take(0)?;
    Ok(row.and_then(PostRow::into_post))
}

/// Every slug this Brand's projection holds, public or not.
pub async fn slugs_for_brand(
    db: &SurrealDb,
    brand_key: &str,
) -> Result<std::collections::HashSet<String>, PostError> {
    let mut response = db
        .query(format!(
            "SELECT VALUE slug FROM {TABLE} WHERE brand_key = $brand"
        ))
        .bind(("brand", brand_key.to_string()))
        .await?
        .check()?;
    let slugs: Vec<String> = response.take(0)?;
    Ok(slugs.into_iter().collect())
}

/// Every Post, for the PostgreSQL parity check.
pub(crate) async fn all(db: &SurrealDb) -> Result<Vec<Post>, PostError> {
    let mut response = db
        .query(format!("SELECT {SELECT} FROM {TABLE}"))
        .await?
        .check()?;
    let rows: Vec<PostRow> = response.take(0)?;
    Ok(rows.into_iter().filter_map(PostRow::into_post).collect())
}

async fn posts_for_brand(db: &SurrealDb, brand_key: &str) -> Result<Vec<Post>, PostError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE brand_key = $brand ORDER BY updated_at DESC"
        ))
        .bind(("brand", brand_key.to_string()))
        .await?
        .check()?;
    let rows: Vec<PostRow> = response.take(0)?;
    Ok(rows.into_iter().filter_map(PostRow::into_post).collect())
}

/// A video uploaded for one Brand's Posts, measured at upload time from the
/// MP4 track header so a Post's `vertical_video` declaration is checked
/// against the media rather than trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlogVideo {
    pub brand_key: String,
    pub object_key: String,
    pub width: u32,
    pub height: u32,
    pub orientation: String,
}

#[derive(SurrealValue)]
struct BlogVideoRow {
    brand_key: String,
    object_key: String,
    width: i64,
    height: i64,
    orientation: String,
}

/// The orientation a displayed `width`×`height` frame measures.
#[must_use]
pub fn orientation_for(width: u32, height: u32) -> &'static str {
    match height.cmp(&width) {
        std::cmp::Ordering::Greater => "vertical",
        std::cmp::Ordering::Less => "landscape",
        std::cmp::Ordering::Equal => "square",
    }
}

/// Record an uploaded Brand video and its measured display dimensions.
pub async fn record_video(
    db: &SurrealDb,
    brand_key: &str,
    object_key: &str,
    width: u32,
    height: u32,
) -> Result<BlogVideo, PostError> {
    let orientation = orientation_for(width, height);
    let id = Uuid::now_v7();
    db.query(
        "CREATE $id SET brand_key = $brand, object_key = $key, \
         width = $width, height = $height, orientation = $orientation, inserted_at = time::now()",
    )
    .bind(("id", record_id(VIDEO_TABLE, id)))
    .bind(("brand", brand_key.to_string()))
    .bind(("key", object_key.to_string()))
    .bind(("width", i64::from(width)))
    .bind(("height", i64::from(height)))
    .bind(("orientation", orientation.to_string()))
    .await?
    .check()?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::BlogVideo(id)).await;
    Ok(BlogVideo {
        brand_key: brand_key.to_string(),
        object_key: object_key.to_string(),
        width,
        height,
        orientation: orientation.to_string(),
    })
}

/// The registered upload behind a video asset key, from any Brand.
pub async fn find_video(db: &SurrealDb, object_key: &str) -> Result<Option<BlogVideo>, PostError> {
    let mut response = db
        .query(format!(
            "SELECT brand_key, object_key, width, height, orientation FROM {VIDEO_TABLE} \
             WHERE object_key = $key LIMIT 1"
        ))
        .bind(("key", object_key.to_string()))
        .await?
        .check()?;
    let row: Option<BlogVideoRow> = response.take(0)?;
    Ok(row.map(|row| BlogVideo {
        brand_key: row.brand_key,
        object_key: row.object_key,
        width: u32::try_from(row.width).unwrap_or_default(),
        height: u32::try_from(row.height).unwrap_or_default(),
        orientation: row.orientation,
    }))
}

/// Social providers whose destination needs portrait video.
const VERTICAL_VIDEO_PROVIDERS: &[&str] = &["instagram", "tiktok"];

/// Whether `provider` may receive this Post, and why not when it may not.
///
/// Instagram and TikTok need a video the deployment has measured as
/// portrait, declared `vertical_video: true`. YouTube needs a video of any
/// orientation. Facebook and Threads accept text or video. Eligibility never
/// selects an account; it only explains why a selected one is refused.
pub fn provider_eligibility(
    provider: &str,
    video_asset: Option<&str>,
    vertical_video: bool,
    video_orientation: Option<&str>,
) -> Result<(), &'static str> {
    if VERTICAL_VIDEO_PROVIDERS.contains(&provider) {
        if video_asset.is_none() {
            return Err("it needs a video");
        }
        if !vertical_video {
            return Err("it needs vertical_video: true");
        }
        return match video_orientation {
            Some("vertical") => Ok(()),
            Some(_) => Err("the uploaded video is not portrait"),
            None => Err("the video's orientation has not been measured in this deployment"),
        };
    }
    if matches!(provider, "linkedin" | "x") && video_asset.is_some() {
        return Err("LinkedIn and X publish text only; remove the video or use a different Post");
    }
    if provider == "youtube" && video_asset.is_none() {
        return Err("it needs a video");
    }
    Ok(())
}

/// One declared social destination: a provider and that provider's own
/// non-secret account id. Never a token or an internal record key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountAlias {
    pub provider: String,
    pub provider_account_id: String,
}

impl std::fmt::Display for AccountAlias {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.provider, self.provider_account_id)
    }
}

/// Per-provider caption copy a Post's source declares.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCaptions<'a> {
    pub facebook: Option<&'a str>,
    pub instagram: Option<&'a str>,
    pub threads: Option<&'a str>,
    pub youtube: Option<&'a str>,
}

/// A Post's editorial source, already parsed, typed, and sanitized.
#[derive(Debug, Clone)]
pub struct SourcePost<'a> {
    pub brand_key: &'a str,
    pub slug: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub body_html: &'a str,
    /// The authored Markdown, when the Post is Markdown-authored.
    pub body_markdown: Option<&'a str>,
    /// `draft`, `scheduled`, or `published`.
    pub status: &'a str,
    pub publish_at: DateTime<Utc>,
    pub video_asset: Option<&'a str>,
    pub vertical_video: Option<bool>,
    pub tiktok_ai_generated: Option<bool>,
    pub captions: SourceCaptions<'a>,
    pub accounts: &'a [AccountAlias],
    /// Provider links. Each names a handle the Brand has recorded.
    pub social_links: &'a [SocialLink],
}

const TITLE_MAX: usize = 120;
const DESCRIPTION_MAX: usize = 200;

/// The copy contract every Post meets, whoever writes it: a kebab-case slug, a
/// title and description of a bounded length with no stray whitespace, a body,
/// and captions free of control characters.
fn validate_copy(post: &SourcePost<'_>) -> Result<(), PostError> {
    let bad = |reason: String| Err(PostError::InvalidCopy(reason));
    let slug_ok = !post.slug.is_empty()
        && !post.slug.starts_with('-')
        && !post.slug.ends_with('-')
        && !post.slug.contains("--")
        && post
            .slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !slug_ok {
        return bad(format!("slug `{}` must be kebab-case", post.slug));
    }
    for (label, text, max) in [
        ("title", post.title, TITLE_MAX),
        ("description", post.description, DESCRIPTION_MAX),
    ] {
        if text.trim().is_empty() {
            return bad(format!("the {label} is required"));
        }
        if text != text.trim() || text.contains('\n') {
            return bad(format!("the {label} must be one trimmed line"));
        }
        if text.chars().count() > max {
            return bad(format!("the {label} is longer than {max} characters"));
        }
    }
    if post
        .body_markdown
        .unwrap_or(post.body_html)
        .trim()
        .is_empty()
    {
        return bad("the body is required".into());
    }
    for (provider, caption) in [
        ("facebook", post.captions.facebook),
        ("instagram", post.captions.instagram),
        ("threads", post.captions.threads),
        ("youtube", post.captions.youtube),
    ] {
        if caption.is_some_and(|text| text.chars().any(|c| c.is_control() && c != '\n')) {
            return bad(format!(
                "the {provider} caption contains a control character"
            ));
        }
    }
    Ok(())
}

/// The hosts each provider's permalinks live on.
const SOCIAL_LINK_HOSTS: &[(&str, &[&str])] = &[
    (
        "facebook",
        &[
            "www.facebook.com",
            "facebook.com",
            "m.facebook.com",
            "fb.watch",
        ],
    ),
    ("instagram", &["www.instagram.com", "instagram.com"]),
    (
        "threads",
        &[
            "www.threads.net",
            "threads.net",
            "www.threads.com",
            "threads.com",
        ],
    ),
    ("tiktok", &["www.tiktok.com", "tiktok.com"]),
    ("youtube", &["www.youtube.com", "youtube.com", "youtu.be"]),
    ("linkedin", &["www.linkedin.com", "linkedin.com"]),
    (
        "x",
        &["x.com", "www.x.com", "twitter.com", "www.twitter.com"],
    ),
];

/// Check the shape of one link, without consulting the database: a known
/// provider, a valid handle, and, when present, a safe post id and an HTTPS
/// permalink on that provider's own host. `post_id` and `permalink` may both
/// be `None`: the link is not yet encoded.
///
/// # Errors
///
/// Why the link is refused.
pub fn validate_social_link(link: &SocialLink) -> Result<(), String> {
    validate_provider_link(
        &link.provider,
        link.post_id.as_deref(),
        link.permalink.as_deref(),
    )?;
    if !crate::social_handles::is_valid_handle(&link.account) {
        return Err(format!("the {} link has an invalid handle", link.provider));
    }
    Ok(())
}

/// Validate a provider receipt and its returned public URL without inventing an account handle.
///
/// # Errors
///
/// An unknown provider, unsafe receipt identity, or URL outside that provider's HTTPS hosts.
pub fn validate_publication_link(
    provider: &str,
    post_id: &str,
    permalink: &str,
) -> Result<(), String> {
    validate_provider_link(provider, Some(post_id), Some(permalink))
}

fn validate_provider_link(
    provider: &str,
    post_id: Option<&str>,
    permalink: Option<&str>,
) -> Result<(), String> {
    let hosts = SOCIAL_LINK_HOSTS
        .iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, hosts)| *hosts)
        .ok_or_else(|| format!("unknown social provider `{provider}`"))?;
    if let Some(post_id) = post_id {
        let safe = !post_id.is_empty()
            && post_id.len() <= 128
            && post_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
        if !safe {
            return Err(format!("the {provider} link has an unsafe post_id"));
        }
    }
    if let Some(permalink) = permalink {
        let rest = permalink
            .strip_prefix("https://")
            .ok_or_else(|| format!("the {provider} permalink must use https"))?;
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let safe_chars = permalink
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c));
        if !safe_chars || !hosts.contains(&host) {
            return Err(format!(
                "the {provider} permalink must be an HTTPS link on {}",
                hosts.join(", ")
            ));
        }
    }
    Ok(())
}

/// Every link must be well formed and name a handle the Brand has already
/// recorded for that provider.
async fn validate_links(
    db: &SurrealDb,
    brand_key: &str,
    links: &[SocialLink],
) -> Result<(), PostError> {
    for link in links {
        validate_social_link(link).map_err(PostError::InvalidSocialLink)?;
        let handle = crate::social_handles::find(db, brand_key, &link.provider)
            .await
            .map_err(|error| PostError::Db(error.to_string()))?
            .ok_or_else(|| PostError::UnknownSocialHandle {
                provider: link.provider.clone(),
            })?;
        if !handle.handle.eq_ignore_ascii_case(&link.account) {
            return Err(PostError::HandleMismatch {
                provider: link.provider.clone(),
                declared: link.account.clone(),
                handle: handle.handle,
            });
        }
    }
    Ok(())
}

/// Encode a Post's link to one provider after the fact. The provider and the
/// Brand's recorded handle are required; the permalink is derived from the
/// handle and `provider_post_id` when the provider's permalink is determined by
/// them (X), or taken from `permalink` otherwise. Replaces any earlier link to
/// that provider and never sends.
///
/// # Errors
///
/// A Post or handle that does not exist, a malformed or contradicted link, or a
/// database failure.
pub async fn encode_social_link(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
    provider_post_id: &str,
    permalink: Option<&str>,
) -> Result<SocialLink, PostError> {
    let post = post_by_id(db, post_id).await?;
    let handle = crate::social_handles::find(db, &post.brand_key, provider)
        .await
        .map_err(|error| PostError::Db(error.to_string()))?
        .ok_or_else(|| PostError::UnknownSocialHandle {
            provider: provider.to_string(),
        })?;
    let permalink = permalink.map(str::to_string).or_else(|| {
        crate::social_handles::derived_permalink(provider, &handle.handle, provider_post_id)
    });
    let link = SocialLink {
        provider: provider.to_string(),
        account: handle.handle,
        post_id: Some(provider_post_id.to_string()),
        permalink,
    };
    validate_social_link(&link).map_err(PostError::InvalidSocialLink)?;
    check_receipts(db, post_id, std::slice::from_ref(&link)).await?;
    let mut links: Vec<SocialLink> = post
        .social_links
        .into_iter()
        .filter(|existing| existing.provider != provider)
        .collect();
    links.push(link.clone());
    links.sort_by(|a, b| a.provider.cmp(&b.provider));
    db.query("UPDATE $id SET social_links = $links, updated_at = time::now()")
        .bind(("id", record_id(TABLE, post_id)))
        .bind(("links", links))
        .await?
        .check()?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::Post(post_id)).await;
    Ok(link)
}

/// Apply one seeded Post: create it when its Brand+slug is new, and leave an
/// existing Post alone. Seeds are initial data, so the database stays the
/// authority once a Post exists; an edit made there is never overwritten by a
/// later boot, and a link encoded after the fact survives.
///
/// # Errors
///
/// A Brand that is not a compiled Brand, or any refusal [`import_source`]
/// raises for the Post's copy, links, or schedule.
pub async fn seed_post(
    db: &SurrealDb,
    post: &SourcePost<'_>,
    now: DateTime<Utc>,
) -> Result<ImportChange, PostError> {
    if crate::seed::compiled_brand_name(post.brand_key).is_none() {
        return Err(PostError::InvalidCopy(format!(
            "`{}` is not a registered Brand",
            post.brand_key
        )));
    }
    if find_by_brand_slug(db, post.brand_key, post.slug)
        .await?
        .is_some()
    {
        return Ok(ImportChange::Unchanged);
    }
    import_source(db, post, now)
        .await
        .map(|result| result.change)
}

/// What one import did to the projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportChange {
    /// The Brand+slug identity was new.
    Created,
    /// Article copy, media, or captions changed. Runtime state was kept.
    Editorial,
    /// The status or a not-yet-public publication time changed.
    Schedule,
    /// Only the confirmed social links changed: display data, never a send.
    Receipts,
    /// The source matches the projection; nothing was written.
    Unchanged,
}

/// The outcome of importing one source Post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportResult {
    pub post_id: Uuid,
    pub change: ImportChange,
    /// The projection's status after the import.
    pub status: String,
    /// True when the Post now waits on a future social publication.
    pub social_scheduled: bool,
    /// Declared accounts this deployment has not connected for the Brand.
    pub unresolved_accounts: Vec<String>,
}

#[derive(SurrealValue)]
struct ResolvedAccountRow {
    id: surrealdb::types::RecordId,
    brand_key: String,
    status: String,
}

async fn resolve_accounts(
    db: &SurrealDb,
    brand_key: &str,
    accounts: &[AccountAlias],
) -> Result<(Vec<(String, String)>, Vec<String>), PostError> {
    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    for alias in accounts {
        let mut response = db
            .query(
                "SELECT id, brand_key, status FROM social_account \
                 WHERE provider = $provider AND provider_account_id = $account",
            )
            .bind(("provider", alias.provider.clone()))
            .bind(("account", alias.provider_account_id.clone()))
            .await?
            .check()?;
        let rows: Vec<ResolvedAccountRow> = response.take(0)?;
        if rows.iter().any(|row| row.brand_key != brand_key) {
            return Err(PostError::CrossBrandSocialAccount(alias.to_string()));
        }
        match rows.into_iter().find(|row| row.status == "connected") {
            Some(row) => {
                let key = match row.id.key {
                    surrealdb::types::RecordIdKey::String(key) => key,
                    other => format!("{other:?}"),
                };
                resolved.push((alias.provider.clone(), key));
            }
            None => unresolved.push(alias.to_string()),
        }
    }
    Ok((resolved, unresolved))
}

#[derive(SurrealValue)]
struct ExistingRow {
    id: surrealdb::types::RecordId,
    status: String,
    publish_at: Option<surrealdb::types::Datetime>,
    source_digest: Option<String>,
    social_links: Option<Vec<SocialLink>>,
}

/// Refuse a link whose provider post id contradicts this deployment's own
/// confirmed publication for the same Post and provider.
async fn check_receipts(
    db: &SurrealDb,
    post_id: Uuid,
    links: &[SocialLink],
) -> Result<(), PostError> {
    if links.is_empty() {
        return Ok(());
    }
    let confirmed = crate::social_publications::for_posts(db, &[post_id])
        .await
        .map_err(|error| PostError::Db(error.to_string()))?;
    for link in links {
        if let Some(publication) = confirmed.iter().find(|row| {
            row.provider == link.provider && row.state == crate::social_publications::CONFIRMED
        }) {
            let confirmed_id = publication.provider_post_id.clone().unwrap_or_default();
            let Some(declared) = link.post_id.as_deref() else {
                continue;
            };
            if confirmed_id != declared {
                return Err(PostError::ReceiptMismatch {
                    provider: link.provider.clone(),
                    declared: declared.to_string(),
                    confirmed: confirmed_id,
                });
            }
        }
    }
    Ok(())
}

fn source_digest(post: &SourcePost<'_>, selected: &[String]) -> String {
    let mut source = String::new();
    for part in [
        post.title,
        post.description,
        post.body_html,
        post.status,
        &post.publish_at.to_rfc3339(),
        post.video_asset.unwrap_or_default(),
        &post.vertical_video.unwrap_or(false).to_string(),
        post.captions.facebook.unwrap_or_default(),
        post.captions.instagram.unwrap_or_default(),
        post.captions.threads.unwrap_or_default(),
        post.captions.youtube.unwrap_or_default(),
        &selected.join(","),
    ] {
        source.push_str(&part.len().to_string());
        source.push(':');
        source.push_str(part);
    }
    if let Some(markdown) = post.body_markdown {
        source.push_str("body_markdown:");
        source.push_str(&markdown.len().to_string());
        source.push(':');
        source.push_str(markdown);
    }
    if let Some(disclosure) = post.tiktok_ai_generated {
        source.push_str(if disclosure {
            "tiktok_ai_generated:true"
        } else {
            "tiktok_ai_generated:false"
        });
    }
    crate::assets::sha256_hex(source.as_bytes())
}

/// Write one approved Post source into its Brand's projection.
///
/// The identity is Brand+slug, so a repeat import updates the same row and
/// every `social_publication` receipt keyed by that row survives. A source
/// edit never re-sends anything: an already-public Post keeps its status and
/// publication time, and a new `scheduled` Post whose time has passed is
/// imported as `published` with no social send. Only a future `scheduled`
/// time leaves the Post waiting on the social publisher.
///
/// # Errors
///
/// Refuses a declared account or video that belongs to another Brand, a
/// video whose measured orientation contradicts `vertical_video`, a selected
/// destination the media cannot satisfy, and a move of an already-public
/// Post's publication time.
#[allow(clippy::too_many_lines)]
pub async fn import_source(
    db: &SurrealDb,
    post: &SourcePost<'_>,
    now: DateTime<Utc>,
) -> Result<ImportResult, PostError> {
    if !matches!(post.status, "draft" | "scheduled" | "published") {
        return Err(PostError::InvalidStatus);
    }
    validate_copy(post)?;
    validate_links(db, post.brand_key, post.social_links).await?;
    let video = match post.video_asset {
        Some(key) => find_video(db, key).await?,
        None => None,
    };
    if let (Some(key), Some(video)) = (post.video_asset, video.as_ref()) {
        if video.brand_key != post.brand_key {
            return Err(PostError::CrossBrandVideo(key.to_string()));
        }
        if let Some(declared) = post.vertical_video {
            if declared != (video.orientation == "vertical") {
                return Err(PostError::VideoOrientationMismatch {
                    declared,
                    measured: video.orientation.clone(),
                });
            }
        }
    }
    let orientation = video.as_ref().map(|video| video.orientation.clone());
    let (resolved, unresolved_accounts) =
        resolve_accounts(db, post.brand_key, post.accounts).await?;
    if post.status != "draft" {
        for (provider, _) in &resolved {
            if provider == "tiktok" && post.tiktok_ai_generated.is_none() {
                return Err(PostError::IneligibleDestination {
                    provider: provider.clone(),
                    reason: "declare tiktok_ai_generated: true or false",
                });
            }
            if let (Some(key), None) = (post.video_asset, video.as_ref()) {
                return Err(PostError::VideoAssetMissing(key.to_string()));
            }
            provider_eligibility(
                provider,
                post.video_asset,
                post.vertical_video.unwrap_or(false),
                orientation.as_deref(),
            )
            .map_err(|reason| PostError::IneligibleDestination {
                provider: provider.clone(),
                reason,
            })?;
        }
    }
    let selected: Vec<String> = resolved.into_iter().map(|(_, key)| key).collect();

    let mut response = db
        .query(format!(
            "SELECT id, status, publish_at, source_digest, social_links FROM {TABLE} \
             WHERE brand_key = $brand AND slug = $slug LIMIT 1"
        ))
        .bind(("brand", post.brand_key.to_string()))
        .bind(("slug", post.slug.to_string()))
        .await?
        .check()?;
    let existing: Option<ExistingRow> = response.take(0)?;

    // A scheduled time that has already passed publishes the article and
    // schedules no social send.
    let source_status = if post.status == "scheduled" && post.publish_at <= now {
        "published"
    } else {
        post.status
    };
    let (id, status, change) = match existing {
        None => (
            Uuid::now_v7(),
            source_status.to_string(),
            ImportChange::Created,
        ),
        Some(row) => {
            let id = record_uuid(&row.id).ok_or(PostError::NotFound)?;
            check_receipts(db, id, post.social_links).await?;
            let links_changed =
                row.social_links.as_deref().unwrap_or_default() != post.social_links;
            let current_at: Option<DateTime<Utc>> = row.publish_at.map(Into::into);
            let live = row.status == "published"
                || (row.status == "scheduled" && current_at.is_some_and(|at| at <= now));
            let held = row.status == "cancelled"
                && source_status == "scheduled"
                && current_at == Some(post.publish_at);
            let status = if held {
                row.status.clone()
            } else if live && source_status != "draft" {
                if current_at != Some(post.publish_at) {
                    return Err(PostError::ScheduleLocked {
                        current: current_at.unwrap_or(post.publish_at),
                        requested: post.publish_at,
                    });
                }
                row.status.clone()
            } else {
                source_status.to_string()
            };
            let digest = source_digest(post, &selected);
            if row.source_digest.as_deref() == Some(digest.as_str()) && row.status == status {
                if links_changed {
                    (id, status, ImportChange::Receipts)
                } else {
                    (id, status, ImportChange::Unchanged)
                }
            } else if row.status != status || current_at != Some(post.publish_at) {
                (id, status, ImportChange::Schedule)
            } else {
                (id, status, ImportChange::Editorial)
            }
        }
    };
    let social_scheduled = status == "scheduled" && post.publish_at > now && !selected.is_empty();
    let result = ImportResult {
        post_id: id,
        change,
        status: status.clone(),
        social_scheduled,
        unresolved_accounts,
    };
    if change == ImportChange::Unchanged {
        return Ok(result);
    }
    if change == ImportChange::Receipts {
        db.query("UPDATE $id SET social_links = $links, updated_at = time::now()")
            .bind(("id", record_id(TABLE, id)))
            .bind(("links", post.social_links.to_vec()))
            .await?
            .check()?;
        crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::Post(id)).await;
        return Ok(result);
    }
    let digest = source_digest(post, &selected);
    let verb = if change == ImportChange::Created {
        "CREATE"
    } else {
        "UPDATE"
    };
    db.query(format!(
        "{verb} $id SET brand_key = $brand, slug = $slug, title = $title, \
         description = $description, body_html = $body_html, body_markdown = $body_markdown, \
         status = $status, \
         publish_at = $publish_at, video_asset = $video_asset, \
         vertical_video = $vertical_video, tiktok_ai_generated = $tiktok_ai_generated, video_orientation = $video_orientation, \
         facebook_caption = $facebook_caption, instagram_caption = $instagram_caption, \
         threads_caption = $threads_caption, youtube_caption = $youtube_caption, \
         selected_social_accounts = $selected, source_digest = $digest, \
         social_links = $links, \
         updated_at = time::now()"
    ))
    .bind(("id", record_id(TABLE, id)))
    .bind(("brand", post.brand_key.to_string()))
    .bind(("slug", post.slug.to_string()))
    .bind(("title", post.title.to_string()))
    .bind(("description", post.description.to_string()))
    .bind(("body_html", post.body_html.to_string()))
    .bind(("body_markdown", post.body_markdown.map(str::to_string)))
    .bind(("status", status))
    .bind((
        "publish_at",
        surrealdb::types::Datetime::from(post.publish_at),
    ))
    .bind(("video_asset", post.video_asset.map(str::to_string)))
    .bind(("vertical_video", post.vertical_video.unwrap_or(false)))
    .bind(("tiktok_ai_generated", post.tiktok_ai_generated))
    .bind(("video_orientation", orientation))
    .bind((
        "facebook_caption",
        post.captions.facebook.map(str::to_string),
    ))
    .bind((
        "instagram_caption",
        post.captions.instagram.map(str::to_string),
    ))
    .bind(("threads_caption", post.captions.threads.map(str::to_string)))
    .bind(("youtube_caption", post.captions.youtube.map(str::to_string)))
    .bind(("selected", selected))
    .bind(("digest", digest))
    .bind(("links", post.social_links.to_vec()))
    .await?
    .check()?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::Post(id)).await;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{brands::NewBrand, firms::NewFirm, persons::NewPerson, test_support};

    #[test]
    fn scheduled_posts_are_live_only_at_or_after_their_time() {
        let publish_at = Utc::now();
        assert!(!scheduled_is_live(
            "scheduled",
            Some(publish_at),
            publish_at - chrono::Duration::seconds(1)
        ));
        assert!(scheduled_is_live("scheduled", Some(publish_at), publish_at));
        assert!(!scheduled_is_live(
            "cancelled",
            Some(publish_at),
            publish_at
        ));
    }

    async fn practice(db: &SurrealDb, label: &str) -> (Uuid, Uuid, String) {
        let admin = crate::persons::create(
            db,
            &NewPerson::with_role(
                format!("{label} Admin"),
                format!("{label}-{}@example.com", Uuid::now_v7()),
                Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm = crate::firms::create(
            db,
            &NewFirm {
                name: format!("{label} Firm"),
                status: "active".into(),
                entity_id: test_support::seed_entity(db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        let key = format!("{label}-brand");
        crate::brands::create(
            db,
            Role::Admin,
            Some(admin.id),
            &NewBrand {
                name: format!("{label} Brand"),
                key: key.clone(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        (firm.id, admin.id, key)
    }

    async fn insert_test_post(
        db: &SurrealDb,
        brand: &str,
        slug: &str,
        status: &str,
        publish_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Uuid {
        let mut post = source(brand, slug, slug);
        post.status = status;
        post.publish_at = publish_at;
        import_source(db, &post, now).await.unwrap().post_id
    }

    #[tokio::test]
    async fn public_links_for_database_posts_include_confirmed_receipts_without_handles_or_seed_edits(
    ) {
        let db = test_support::mem_surreal().await;
        let (_, _, brand) = practice(&db, "generated-public-links").await;
        let now = Utc::now();
        let first = insert_test_post(
            &db,
            &brand,
            "generated",
            "published",
            now - chrono::Duration::hours(1),
            now,
        )
        .await;
        let other = insert_test_post(
            &db,
            &brand,
            "other",
            "published",
            now - chrono::Duration::hours(1),
            now,
        )
        .await;
        for (post, provider, remote, url) in [
            (
                first,
                "facebook",
                "fb-receipt",
                Some("https://www.facebook.com/synthetic/posts/fb-receipt"),
            ),
            (
                first,
                "tiktok",
                "tt-receipt",
                Some("https://www.tiktok.com/@synthetic/video/tt-receipt"),
            ),
            (first, "instagram", "ig-receipt", None),
            (
                other,
                "youtube",
                "other-receipt",
                Some("https://www.youtube.com/watch?v=other-receipt"),
            ),
        ] {
            crate::social_publications::admit(&db, post, provider)
                .await
                .unwrap();
            crate::social_publications::mark_confirmed(&db, post, provider, remote, url)
                .await
                .unwrap();
        }
        crate::social_publications::admit(&db, first, "threads")
            .await
            .unwrap();
        crate::social_publications::mark_failed(&db, first, "threads", "synthetic failure")
            .await
            .unwrap();
        let posts = published_for_brand(&db, &brand, now).await.unwrap();
        let post = posts.iter().find(|post| post.id == first).unwrap();
        assert!(post.social_links.is_empty());
        assert!(crate::social_handles::find(&db, &brand, "facebook")
            .await
            .unwrap()
            .is_none());
        let receipts = crate::social_publications::for_posts(&db, &[first, other])
            .await
            .unwrap();
        let links = public_social_links(post, &receipts);
        assert_eq!(
            links
                .iter()
                .map(|link| link.provider.as_str())
                .collect::<Vec<_>>(),
            ["facebook", "tiktok"]
        );
        assert_eq!(
            links[0].permalink,
            "https://www.facebook.com/synthetic/posts/fb-receipt"
        );
        assert_eq!(
            links[1].permalink,
            "https://www.tiktok.com/@synthetic/video/tt-receipt"
        );
    }

    #[tokio::test]
    async fn public_links_preserve_manual_precedence_and_reject_unconfirmed_or_invalid_receipts() {
        let db = test_support::mem_surreal().await;
        let (_, _, brand) = practice(&db, "public-link-validation").await;
        let now = Utc::now();
        let id = insert_test_post(
            &db,
            &brand,
            "generated",
            "published",
            now - chrono::Duration::hours(1),
            now,
        )
        .await;
        let mut post = find_by_id(&db, id).await.unwrap().unwrap();
        let manual = SocialLink {
            provider: "facebook".into(),
            account: "synthetic-page".into(),
            post_id: Some("manual".into()),
            permalink: Some("https://www.facebook.com/synthetic/posts/manual".into()),
        };
        post.social_links = vec![manual.clone(), manual];
        let receipt = crate::social_publications::SocialPublication {
            id: "synthetic-receipt".into(),
            post_id: id,
            provider: "facebook".into(),
            state: crate::social_publications::CONFIRMED.into(),
            attempt_count: 1,
            provider_task_id: None,
            provider_post_id: Some("receipt".into()),
            permalink: Some("https://www.facebook.com/synthetic/posts/receipt".into()),
            last_error: None,
            inserted_at: now,
            updated_at: now,
        };
        assert_eq!(
            public_social_links(&post, std::slice::from_ref(&receipt)),
            vec![PublicSocialLink {
                provider: "facebook".into(),
                permalink: "https://www.facebook.com/synthetic/posts/manual".into()
            }]
        );
        post.social_links.clear();
        for mut invalid in [
            crate::social_publications::SocialPublication {
                state: "unknown".into(),
                ..receipt.clone()
            },
            crate::social_publications::SocialPublication {
                state: "sending".into(),
                ..receipt.clone()
            },
            crate::social_publications::SocialPublication {
                state: "failed".into(),
                ..receipt.clone()
            },
            crate::social_publications::SocialPublication {
                provider_post_id: None,
                ..receipt.clone()
            },
            crate::social_publications::SocialPublication {
                provider_post_id: Some("unsafe/id".into()),
                ..receipt.clone()
            },
            crate::social_publications::SocialPublication {
                permalink: None,
                ..receipt.clone()
            },
            crate::social_publications::SocialPublication {
                permalink: Some("https://example.test/fake".into()),
                ..receipt.clone()
            },
            crate::social_publications::SocialPublication {
                post_id: Uuid::now_v7(),
                ..receipt.clone()
            },
        ] {
            assert!(
                public_social_links(&post, &[invalid.clone()]).is_empty(),
                "{invalid:?}"
            );
            invalid.provider = "unknown".into();
            assert!(public_social_links(&post, &[invalid]).is_empty());
        }
        let mut reverse = vec![
            receipt.clone(),
            crate::social_publications::SocialPublication {
                provider: "tiktok".into(),
                permalink: Some("https://www.tiktok.com/@synthetic/video/receipt".into()),
                ..receipt
            },
        ];
        let ordered = public_social_links(&post, &reverse);
        reverse.reverse();
        assert_eq!(public_social_links(&post, &reverse), ordered);
    }

    #[tokio::test]
    async fn same_slug_is_brand_scoped_and_future_or_draft_posts_are_private() {
        let db = test_support::mem_surreal().await;
        let (_, _, brand_a) = practice(&db, "a").await;
        let (_, _, brand_b) = practice(&db, "b").await;
        let now = Utc::now();
        let past = now - chrono::Duration::days(1);
        insert_test_post(&db, &brand_a, "same-slug", "published", past, now).await;
        insert_test_post(
            &db,
            &brand_a,
            "future",
            "published",
            now + chrono::Duration::days(1),
            now,
        )
        .await;
        let scheduled_at = now + chrono::Duration::days(2);
        insert_test_post(&db, &brand_a, "scheduled", "scheduled", scheduled_at, now).await;
        insert_test_post(&db, &brand_a, "draft", "draft", past, now).await;
        insert_test_post(&db, &brand_b, "same-slug", "published", past, now).await;

        let a = published_for_brand(&db, &brand_a, now).await.unwrap();
        let b = published_for_brand(&db, &brand_b, now).await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].slug, "same-slug");
        let after_schedule =
            published_for_brand(&db, &brand_a, scheduled_at + chrono::Duration::seconds(1))
                .await
                .unwrap();
        assert!(after_schedule.iter().any(|post| post.slug == "scheduled"));
        assert_eq!(b[0].slug, "same-slug");
        assert_ne!(a[0].id, b[0].id);
        let slugs = slugs_for_brand(&db, &brand_a).await.unwrap();
        assert_eq!(
            slugs.len(),
            4,
            "the projection knows drafts and future Posts too"
        );
    }

    #[tokio::test]
    async fn a_due_article_stays_public_when_its_social_send_failed() {
        let db = test_support::mem_surreal().await;
        let now = Utc::now();
        let publish_at = now + chrono::Duration::hours(1);
        let id = insert_test_post(&db, "vesta", "due", "scheduled", publish_at, now).await;
        crate::social_publications::admit(&db, id, "facebook")
            .await
            .unwrap();
        crate::social_publications::mark_failed(&db, id, "facebook", "synthetic provider failure")
            .await
            .unwrap();
        let later = publish_at + chrono::Duration::minutes(5);
        let public = published_for_brand(&db, "vesta", later).await.unwrap();
        assert_eq!(
            public.len(),
            1,
            "the article does not wait on social delivery"
        );
        assert_eq!(
            public[0].status, "scheduled",
            "the worker status is unchanged"
        );
    }

    #[tokio::test]
    async fn firm_admin_catalog_is_isolated_and_owner_sees_all_firms() {
        let db = test_support::mem_surreal().await;
        let (_, admin_a, brand_a) = practice(&db, "catalog-a").await;
        let (_, _, brand_b) = practice(&db, "catalog-b").await;
        let owner = crate::persons::create(
            &db,
            &NewPerson::with_role("Catalog Owner", "catalog-owner@example.com", Role::Owner),
        )
        .await
        .unwrap();
        let now = Utc::now();
        insert_test_post(&db, &brand_a, "one", "draft", now, now).await;
        insert_test_post(&db, &brand_b, "two", "draft", now, now).await;

        let a = for_actor(&db, Role::Admin, admin_a).await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].brand_key, brand_a);
        assert_eq!(
            for_actor(&db, Role::Owner, owner.id).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn cancel_is_firm_scoped_and_holds_until_the_source_reschedules() {
        let db = test_support::mem_surreal().await;
        let (_, admin_a, brand_a) = practice(&db, "schedule-a").await;
        let (_, admin_b, _) = practice(&db, "schedule-b").await;
        let now = Utc::now();
        let first_time = now + chrono::Duration::days(3);
        let mut post = source(&brand_a, "scheduled", "Scheduled");
        post.status = "scheduled";
        post.publish_at = first_time;
        let id = import_source(&db, &post, now).await.unwrap().post_id;
        assert!(matches!(
            cancel(&db, Role::Admin, admin_b, id).await,
            Err(PostError::Denied)
        ));
        cancel(&db, Role::Admin, admin_a, id).await.unwrap();
        assert_eq!(
            find_by_id(&db, id).await.unwrap().unwrap().status,
            "cancelled"
        );
        let repeat = import_source(&db, &post, now).await.unwrap();
        assert_eq!(repeat.status, "cancelled", "a repeat import keeps the hold");
        assert!(!repeat.social_scheduled);
        post.publish_at = first_time + chrono::Duration::days(1);
        let moved = import_source(&db, &post, now).await.unwrap();
        assert_eq!(
            moved.status, "scheduled",
            "a new time in Git schedules again"
        );
        assert_eq!(moved.change, ImportChange::Schedule);
    }

    #[tokio::test]
    async fn historical_blog_post_without_platform_fields_remains_readable() {
        let db = test_support::mem_surreal().await;
        let (_, _, brand) = practice(&db, "historical-fields").await;
        let id = Uuid::now_v7();
        db.query("CREATE $id SET brand_key = $brand, slug = 'older', title = 'Older', description = '', body_html = '', status = 'draft', inserted_at = time::now(), updated_at = time::now()")
            .bind(("id", record_id(TABLE, id)))
            .bind(("brand", brand.clone()))
            .await
            .unwrap()
            .check()
            .unwrap();
        let post = post_by_id(&db, id).await.unwrap();
        assert_eq!(post.brand_key, brand);
        assert_eq!(post.video_asset, None);
        assert_eq!(post.tiktok_ai_generated, None);
        assert_eq!(post.facebook_caption, None);
        assert_eq!(post.instagram_caption, None);
        assert_eq!(post.threads_caption, None);
        assert_eq!(post.youtube_caption, None);
        assert!(post.selected_social_accounts.is_empty());
        assert!(posts_for_brand(&db, &brand)
            .await
            .unwrap()
            .iter()
            .any(|row| row.id == id));
    }

    #[tokio::test]
    async fn platform_captions_round_trip_through_the_import() {
        let db = test_support::mem_surreal().await;
        let mut post = source("neon", "platform-content", "Platform content");
        post.status = "draft";
        post.captions = SourceCaptions {
            facebook: Some("Facebook text"),
            instagram: Some("Instagram text"),
            threads: Some("Threads text"),
            youtube: Some("YouTube text"),
        };
        let id = import_source(&db, &post, Utc::now()).await.unwrap().post_id;
        let created = find_by_id(&db, id).await.unwrap().unwrap();
        assert_eq!(created.facebook_caption.as_deref(), Some("Facebook text"));
        assert_eq!(created.instagram_caption.as_deref(), Some("Instagram text"));
        assert_eq!(created.threads_caption.as_deref(), Some("Threads text"));
        assert_eq!(created.youtube_caption.as_deref(), Some("YouTube text"));
    }

    fn source<'a>(brand: &'a str, slug: &'a str, title: &'a str) -> SourcePost<'a> {
        SourcePost {
            brand_key: brand,
            slug,
            title,
            description: "Synthetic description",
            body_html: "<p>Synthetic</p>",
            body_markdown: None,
            status: "published",
            publish_at: "2026-01-02T09:00:00-08:00"
                .parse::<DateTime<chrono::FixedOffset>>()
                .unwrap()
                .with_timezone(&Utc),
            video_asset: None,
            vertical_video: None,
            tiktok_ai_generated: None,
            captions: SourceCaptions::default(),
            accounts: &[],
            social_links: &[],
        }
    }

    #[test]
    fn disclosure_preserves_legacy_digest_and_invalidates_changed_editorial_choice() {
        let mut post = source("synthetic", "legacy", "Legacy");
        let legacy = source_digest(&post, &[]);
        assert_eq!(
            legacy,
            "7b07da4f66b698203f4aceae8df6c3fddb1ea506c128d1e6ddcf68862721bd82"
        );
        post.tiktok_ai_generated = Some(false);
        let disclosed = source_digest(&post, &[]);
        assert_ne!(legacy, disclosed);
        post.tiktok_ai_generated = Some(true);
        assert_ne!(disclosed, source_digest(&post, &[]));
    }

    async fn connect_account(
        db: &SurrealDb,
        admin: Uuid,
        brand: &str,
        provider: crate::social_accounts::SocialProvider,
        remote: &str,
    ) -> String {
        let kms = cloud::FakeKms::new("synthetic-import-key");
        crate::social_accounts::connect(
            db,
            crate::social_accounts::ConnectSocialAccount {
                actor_role: Role::Admin,
                actor_person_id: admin,
                brand_key: brand,
                provider,
                provider_account_id: remote,
                display_name: None,
                scopes: &["synthetic_scope".to_string()],
                expires_at: None,
                access_token: "synthetic-token",
                refresh_token: None,
            },
            &kms,
        )
        .await
        .unwrap()
        .id
    }

    fn alias(provider: &str, remote: &str) -> AccountAlias {
        AccountAlias {
            provider: provider.into(),
            provider_account_id: remote.into(),
        }
    }

    #[tokio::test]
    async fn repeat_import_is_idempotent_and_source_edits_keep_the_row_and_its_receipts() {
        let db = test_support::mem_surreal().await;
        let now = Utc::now();
        let first = import_source(&db, &source("neon", "welcome", "Welcome"), now)
            .await
            .unwrap();
        assert_eq!(first.change, ImportChange::Created);
        let before = find_by_id(&db, first.post_id).await.unwrap().unwrap();
        let again = import_source(&db, &source("neon", "welcome", "Welcome"), now)
            .await
            .unwrap();
        assert_eq!(again.change, ImportChange::Unchanged);
        assert_eq!(again.post_id, first.post_id);
        let unchanged = find_by_id(&db, first.post_id).await.unwrap().unwrap();
        assert_eq!(
            unchanged.updated_at, before.updated_at,
            "a repeat import writes nothing"
        );

        crate::social_publications::admit(&db, first.post_id, "facebook")
            .await
            .unwrap();
        // Confirmed directly, so the test does not depend on the worker's write API.
        db.query(
            "UPDATE type::record('social_publication', $key) \
             SET state = 'confirmed', provider_post_id = $remote, permalink = $link",
        )
        .bind((
            "key",
            crate::social_publications::publication_key(first.post_id, "facebook"),
        ))
        .bind(("remote", "synthetic-remote-1"))
        .bind(("link", "https://www.facebook.com/synthetic/posts/1"))
        .await
        .unwrap()
        .check()
        .unwrap();

        let edited = import_source(&db, &source("neon", "welcome", "Welcome, revised"), now)
            .await
            .unwrap();
        assert_eq!(edited.change, ImportChange::Editorial);
        assert_eq!(edited.post_id, first.post_id);
        assert!(
            !edited.social_scheduled,
            "an edit is never a publishing request"
        );
        let row = find_by_id(&db, first.post_id).await.unwrap().unwrap();
        assert_eq!(row.title, "Welcome, revised");
        assert_ne!(
            row.source_digest, before.source_digest,
            "the revision is recorded"
        );
        let receipts = crate::social_publications::for_posts(&db, &[first.post_id])
            .await
            .unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].state, "confirmed");
        assert_eq!(
            receipts[0].provider_post_id.as_deref(),
            Some("synthetic-remote-1")
        );
    }

    fn link(provider: &str, post_id: &str) -> SocialLink {
        SocialLink {
            provider: provider.into(),
            account: "synthetic-page".into(),
            post_id: Some(post_id.into()),
            permalink: Some(format!(
                "https://www.facebook.com/synthetic/posts/{post_id}"
            )),
        }
    }

    #[tokio::test]
    async fn a_receipt_only_change_is_display_data_and_never_a_publishing_request() {
        let db = test_support::mem_surreal().await;
        let now = Utc::now();
        crate::social_handles::set(&db, "neon", "facebook", "synthetic-page")
            .await
            .unwrap();
        let mut post = source("neon", "receipts", "Receipts");
        post.status = "scheduled";
        post.publish_at = now + chrono::Duration::hours(1);
        let first = import_source(&db, &post, now).await.unwrap();
        crate::social_publications::admit(&db, first.post_id, "facebook")
            .await
            .unwrap();
        // Confirmed directly, so the test does not depend on the worker's write API.
        db.query(
            "UPDATE type::record('social_publication', $key) \
             SET state = 'confirmed', provider_post_id = $remote, permalink = $link",
        )
        .bind((
            "key",
            crate::social_publications::publication_key(first.post_id, "facebook"),
        ))
        .bind(("remote", "remote-7"))
        .bind(("link", "https://www.facebook.com/synthetic/posts/remote-7"))
        .await
        .unwrap()
        .check()
        .unwrap();
        let later = post.publish_at + chrono::Duration::hours(1);
        db.query("UPDATE $id SET status = 'published'")
            .bind(("id", record_id(TABLE, first.post_id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        let before = find_by_id(&db, first.post_id).await.unwrap().unwrap();

        let links = [link("facebook", "remote-7")];
        post.social_links = &links;
        let receipts = import_source(&db, &post, later).await.unwrap();
        assert_eq!(receipts.change, ImportChange::Receipts);
        assert!(!receipts.social_scheduled);
        let after = find_by_id(&db, first.post_id).await.unwrap().unwrap();
        assert_eq!(after.social_links, links.to_vec());
        assert_eq!(after.status, "published");
        assert_eq!(after.publish_at, before.publish_at);
        assert_eq!(after.source_digest, before.source_digest);
        assert_eq!(
            import_source(&db, &post, later).await.unwrap().change,
            ImportChange::Unchanged,
            "a repeated receipt import writes nothing"
        );
        let publications = crate::social_publications::for_posts(&db, &[first.post_id])
            .await
            .unwrap();
        assert_eq!(publications.len(), 1);
        assert_eq!(publications[0].attempt_count, 1, "nothing was re-admitted");

        let wrong = [link("facebook", "someone-else")];
        post.social_links = &wrong;
        assert!(matches!(
            import_source(&db, &post, later).await,
            Err(PostError::ReceiptMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn the_same_slug_imports_independently_for_two_brands() {
        let db = test_support::mem_surreal().await;
        let now = Utc::now();
        let vesta = import_source(&db, &source("vesta", "hello", "Vesta hello"), now)
            .await
            .unwrap();
        let cyber = import_source(
            &db,
            &source("cyber-injury-law", "hello", "Cyber hello"),
            now,
        )
        .await
        .unwrap();
        assert_ne!(vesta.post_id, cyber.post_id);
        import_source(&db, &source("vesta", "hello", "Vesta hello, revised"), now)
            .await
            .unwrap();
        let cyber_row = published_for_brand(&db, "cyber-injury-law", now)
            .await
            .unwrap();
        assert_eq!(cyber_row.len(), 1);
        assert_eq!(cyber_row[0].title, "Cyber hello");
        let vesta_row = published_for_brand(&db, "vesta", now).await.unwrap();
        assert_eq!(vesta_row[0].title, "Vesta hello, revised");
    }

    #[tokio::test]
    async fn scheduling_is_future_only_and_a_public_post_keeps_its_runtime_status_and_time() {
        let db = test_support::mem_surreal().await;
        let now = Utc::now();
        let mut past = source("neon", "past", "Past");
        past.status = "scheduled";
        past.publish_at = now - chrono::Duration::hours(1);
        let imported = import_source(&db, &past, now).await.unwrap();
        assert_eq!(
            imported.status, "published",
            "a passed time publishes with no send"
        );
        assert!(!imported.social_scheduled);

        let mut future = source("neon", "future", "Future");
        future.status = "scheduled";
        future.publish_at = now + chrono::Duration::hours(2);
        let scheduled = import_source(&db, &future, now).await.unwrap();
        assert_eq!(scheduled.status, "scheduled");
        assert!(published_for_brand(&db, "neon", now)
            .await
            .unwrap()
            .iter()
            .all(|post| post.slug != "future"));

        // The worker promotes the due post; the unchanged source keeps it.
        let later = future.publish_at + chrono::Duration::minutes(1);
        db.query("UPDATE $id SET status = 'published'")
            .bind(("id", record_id(TABLE, scheduled.post_id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        let kept = import_source(&db, &future, later).await.unwrap();
        assert_eq!(kept.status, "published");

        let mut moved = future.clone();
        moved.publish_at = later + chrono::Duration::days(1);
        assert!(matches!(
            import_source(&db, &moved, later).await,
            Err(PostError::ScheduleLocked { .. })
        ));
        let mut drafted = future.clone();
        drafted.status = "draft";
        assert_eq!(
            import_source(&db, &drafted, later).await.unwrap().status,
            "draft",
            "the source may still withdraw a public article"
        );
    }

    #[tokio::test]
    async fn declared_video_must_be_this_brands_measured_upload() {
        let db = test_support::mem_surreal().await;
        let (_, admin, brand) = practice(&db, "video").await;
        let (_, _, other) = practice(&db, "other-video").await;
        connect_account(
            &db,
            admin,
            &brand,
            crate::social_accounts::SocialProvider::Instagram,
            "synthetic-ig",
        )
        .await;
        let portrait = format!("img/{brand}/social/portrait.mp4");
        let landscape = format!("img/{brand}/social/landscape.mp4");
        let foreign = format!("img/{other}/social/foreign.mp4");
        record_video(&db, &brand, &portrait, 1080, 1920)
            .await
            .unwrap();
        record_video(&db, &brand, &landscape, 1920, 1080)
            .await
            .unwrap();
        record_video(&db, &other, &foreign, 1080, 1920)
            .await
            .unwrap();
        let accounts = [alias("instagram", "synthetic-ig")];
        let now = Utc::now();
        let mut post = source(&brand, "video", "Video");
        post.status = "scheduled";
        post.publish_at = now + chrono::Duration::days(1);
        post.accounts = &accounts;

        post.video_asset = Some(&portrait);
        post.vertical_video = Some(true);
        let ok = import_source(&db, &post, now).await.unwrap();
        assert!(ok.social_scheduled);
        let row = find_by_id(&db, ok.post_id).await.unwrap().unwrap();
        assert!(row.vertical_video);
        assert_eq!(row.video_orientation.as_deref(), Some("vertical"));
        assert_eq!(row.video_asset.as_deref(), Some(portrait.as_str()));
        assert_eq!(row.selected_social_accounts.len(), 1);

        post.video_asset = Some(&landscape);
        assert!(matches!(
            import_source(&db, &post, now).await,
            Err(PostError::VideoOrientationMismatch { declared: true, .. })
        ));
        post.video_asset = Some(&portrait);
        post.vertical_video = Some(false);
        assert!(matches!(
            import_source(&db, &post, now).await,
            Err(PostError::VideoOrientationMismatch {
                declared: false,
                ..
            })
        ));
        post.vertical_video = Some(true);
        post.video_asset = Some(&foreign);
        assert!(matches!(
            import_source(&db, &post, now).await,
            Err(PostError::CrossBrandVideo(_))
        ));
        let dangling = format!("img/{brand}/social/never-uploaded.mp4");
        post.video_asset = Some(&dangling);
        assert!(matches!(
            import_source(&db, &post, now).await,
            Err(PostError::VideoAssetMissing(_))
        ));
        post.video_asset = None;
        assert!(matches!(
            import_source(&db, &post, now).await,
            Err(PostError::IneligibleDestination { .. })
        ));
        post.status = "draft";
        post.video_asset = Some(&dangling);
        assert!(
            import_source(&db, &post, now).await.is_ok(),
            "a draft may be incomplete"
        );
    }

    #[tokio::test]
    async fn declared_accounts_resolve_only_within_their_brand() {
        let db = test_support::mem_surreal().await;
        let (_, admin_a, brand_a) = practice(&db, "alias-a").await;
        let (_, admin_b, brand_b) = practice(&db, "alias-b").await;
        connect_account(
            &db,
            admin_a,
            &brand_a,
            crate::social_accounts::SocialProvider::Facebook,
            "synthetic-page-a",
        )
        .await;
        connect_account(
            &db,
            admin_b,
            &brand_b,
            crate::social_accounts::SocialProvider::Facebook,
            "synthetic-page-b",
        )
        .await;
        let now = Utc::now();
        let crossing = [alias("facebook", "synthetic-page-b")];
        let mut post = source(&brand_a, "aliases", "Aliases");
        post.accounts = &crossing;
        assert!(matches!(
            import_source(&db, &post, now).await,
            Err(PostError::CrossBrandSocialAccount(_))
        ));
        let mixed = [
            alias("facebook", "synthetic-page-a"),
            alias("threads", "not-connected-here"),
        ];
        post.accounts = &mixed;
        let imported = import_source(&db, &post, now).await.unwrap();
        assert_eq!(
            imported.unresolved_accounts,
            vec!["threads:not-connected-here"]
        );
        let row = find_by_id(&db, imported.post_id).await.unwrap().unwrap();
        assert_eq!(row.selected_social_accounts.len(), 1);
    }

    #[test]
    fn provider_eligibility_explains_each_refusal() {
        let key = Some("img/vesta/social/a.mp4");
        assert!(provider_eligibility("facebook", None, false, None).is_ok());
        assert!(provider_eligibility("threads", None, false, None).is_ok());
        assert_eq!(
            provider_eligibility("youtube", None, false, None),
            Err("it needs a video")
        );
        assert!(provider_eligibility("youtube", key, false, Some("landscape")).is_ok());
        for provider in ["instagram", "tiktok"] {
            assert_eq!(
                provider_eligibility(provider, None, true, None),
                Err("it needs a video")
            );
            assert_eq!(
                provider_eligibility(provider, key, false, Some("vertical")),
                Err("it needs vertical_video: true")
            );
            assert_eq!(
                provider_eligibility(provider, key, true, Some("landscape")),
                Err("the uploaded video is not portrait")
            );
            assert!(provider_eligibility(provider, key, true, None).is_err());
            assert!(provider_eligibility(provider, key, true, Some("vertical")).is_ok());
        }
    }

    #[tokio::test]
    async fn find_and_manageable_keys_cover_owner_and_admin() {
        let db = test_support::mem_surreal().await;
        let (_, admin, brand) = practice(&db, "legacy-import").await;
        let imported = import_source(
            &db,
            &source("neon", "legacy-welcome", "Legacy welcome"),
            Utc::now(),
        )
        .await
        .unwrap();
        let found = find_by_id(&db, imported.post_id).await.unwrap();
        assert_eq!(
            found.as_ref().map(|row| row.slug.as_str()),
            Some("legacy-welcome")
        );
        assert!(find_by_id(&db, Uuid::now_v7()).await.unwrap().is_none());
        let admin_keys = manageable_brand_keys(&db, Role::Admin, admin)
            .await
            .unwrap();
        assert!(admin_keys.contains(&brand));
        let owner = crate::persons::create(
            &db,
            &NewPerson::with_role(
                "Legacy Owner",
                format!("legacy-owner-{}@example.com", Uuid::now_v7()),
                Role::Owner,
            ),
        )
        .await
        .unwrap();
        let owner_keys = manageable_brand_keys(&db, Role::Owner, owner.id)
            .await
            .unwrap();
        assert!(owner_keys.contains(&brand));
    }

    #[tokio::test]
    async fn a_historical_text_only_post_backfills_vertical_video_false_on_every_apply() {
        let db = test_support::mem_surreal().await;
        db.query("REMOVE FIELD vertical_video ON post")
            .await
            .unwrap()
            .check()
            .unwrap();
        let id = Uuid::now_v7();
        db.query("CREATE $id SET brand_key = 'neon', slug = 'older', title = 'Older', description = '', body_html = '<p>Older</p>', status = 'published', inserted_at = time::now(), updated_at = time::now()")
            .bind(("id", record_id(TABLE, id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        crate::schema::apply(&db).await.unwrap();
        crate::schema::apply(&db).await.unwrap();
        let post = find_by_id(&db, id).await.unwrap().unwrap();
        assert!(!post.vertical_video);
        assert_eq!(
            post.video_asset, None,
            "no video is invented for an old article"
        );
        db.query("UPDATE $id SET status = 'draft'")
            .bind(("id", record_id(TABLE, id)))
            .await
            .unwrap()
            .check()
            .expect("a backfilled row accepts the worker's later update");
    }

    #[tokio::test]
    async fn a_social_link_may_only_name_a_handle_the_brand_already_holds() {
        let db = test_support::mem_surreal().await;
        let now = Utc::now();
        // Nullable: the link is declared before the provider post exists.
        let pending = [SocialLink {
            provider: "x".into(),
            account: "NeonLawUSA".into(),
            post_id: None,
            permalink: None,
        }];
        let mut post = source("neon", "handles", "Handles");
        post.social_links = &pending;
        assert!(matches!(
            import_source(&db, &post, now).await,
            Err(PostError::UnknownSocialHandle { .. })
        ));
        crate::social_handles::set(&db, "neon", "x", "NeonLawUSA")
            .await
            .unwrap();
        let id = import_source(&db, &post, now).await.unwrap().post_id;
        let stored = find_by_id(&db, id).await.unwrap().unwrap();
        assert_eq!(stored.social_links, pending.to_vec());

        let other = [SocialLink {
            account: "SomeoneElse".into(),
            ..pending[0].clone()
        }];
        post.social_links = &other;
        assert!(matches!(
            import_source(&db, &post, now).await,
            Err(PostError::HandleMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn the_x_link_is_encoded_after_the_fact_from_the_handle() {
        let db = test_support::mem_surreal().await;
        let now = Utc::now();
        crate::social_handles::set(&db, "neon", "x", "NeonLawUSA")
            .await
            .unwrap();
        let id = import_source(&db, &source("neon", "later", "Later"), now)
            .await
            .unwrap()
            .post_id;
        let link = encode_social_link(&db, id, "x", "1850000000000000000", None)
            .await
            .unwrap();
        assert_eq!(
            link.permalink.as_deref(),
            Some("https://x.com/NeonLawUSA/status/1850000000000000000")
        );
        let stored = find_by_id(&db, id).await.unwrap().unwrap();
        assert_eq!(stored.social_links, vec![link]);
        assert!(matches!(
            encode_social_link(&db, id, "tiktok", "1", None).await,
            Err(PostError::UnknownSocialHandle { .. })
        ));
    }

    #[tokio::test]
    async fn a_seeded_post_is_created_once_and_never_overwritten() {
        let db = test_support::mem_surreal().await;
        let now = Utc::now();
        let post = source("neon", "seeded", "Seeded");
        assert_eq!(
            seed_post(&db, &post, now).await.unwrap(),
            ImportChange::Created
        );
        let id = find_by_brand_slug(&db, "neon", "seeded")
            .await
            .unwrap()
            .unwrap()
            .id;
        let edited = source("neon", "seeded", "Seeded, edited in the database");
        db.query("UPDATE $id SET title = 'Edited in the database'")
            .bind(("id", record_id(TABLE, id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(
            seed_post(&db, &edited, now).await.unwrap(),
            ImportChange::Unchanged
        );
        let stored = find_by_id(&db, id).await.unwrap().unwrap();
        assert_eq!(stored.title, "Edited in the database");
        assert!(matches!(
            seed_post(&db, &source("not-a-brand", "x", "X"), now).await,
            Err(PostError::InvalidCopy(_))
        ));
    }

    #[tokio::test]
    async fn copy_that_breaks_the_contract_is_refused() {
        let db = test_support::mem_surreal().await;
        let now = Utc::now();
        let long = "x".repeat(DESCRIPTION_MAX + 1);
        for (label, post) in [
            ("slug", source("neon", "Not_Kebab", "T")),
            ("blank title", source("neon", "ok", "  ")),
            ("padded title", source("neon", "ok", " padded ")),
            (
                "long description",
                SourcePost {
                    description: &long,
                    ..source("neon", "ok", "T")
                },
            ),
            (
                "empty body",
                SourcePost {
                    body_html: " ",
                    ..source("neon", "ok", "T")
                },
            ),
        ] {
            assert!(
                matches!(
                    import_source(&db, &post, now).await,
                    Err(PostError::InvalidCopy(_))
                ),
                "{label} should be refused"
            );
        }
    }

    #[test]
    fn x_permalinks_must_live_on_x() {
        let ok = SocialLink {
            provider: "x".into(),
            account: "NeonLawUSA".into(),
            post_id: Some("1".into()),
            permalink: Some("https://x.com/NeonLawUSA/status/1".into()),
        };
        assert!(validate_social_link(&ok).is_ok());
        let wrong_host = SocialLink {
            permalink: Some("https://example.com/NeonLawUSA/status/1".into()),
            ..ok
        };
        assert!(validate_social_link(&wrong_host).is_err());
    }
}
