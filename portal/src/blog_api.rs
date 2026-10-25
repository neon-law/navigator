use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use store::posts::{AccountAlias, SourceCaptions, SourcePost};
use store::social_accounts::SocialProvider;

use crate::api::{AdminSession, ApiError, ApiState};
use crate::session::SessionData;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublishPost {
    brand: String,
    slug: String,
    title: String,
    description: String,
    facebook_caption: Option<String>,
    instagram_caption: Option<String>,
    threads_caption: Option<String>,
    youtube_caption: Option<String>,
    body_markdown: String,
    providers: Vec<SocialProvider>,
    account_ids: std::collections::BTreeMap<String, String>,
    publish_at: Option<DateTime<Utc>>,
    video_asset: Option<String>,
    tiktok_ai_generated: Option<bool>,
}

/// Store approved website copy without scheduling provider delivery.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordPost {
    brand: String,
    slug: String,
    title: String,
    description: String,
    body_markdown: String,
    publish_at: DateTime<Utc>,
    video_asset: Option<String>,
}

pub(crate) async fn record(
    State(state): State<ApiState>,
    AdminSession(actor): AdminSession,
    Json(input): Json<RecordPost>,
) -> Result<Response, BlogError> {
    authorize(&state, &actor, &input.brand).await?;
    let now = Utc::now();
    if input.publish_at > now {
        return Err(BlogError::Invalid(
            "record requires a publication time in the past".into(),
        ));
    }
    if let Some(post) = store::posts::find_by_brand_slug(&state.surreal, &input.brand, &input.slug)
        .await
        .map_err(database)?
    {
        return Err(BlogError::AlreadyExists(post.id));
    }
    let vertical_video = match &input.video_asset {
        Some(key) => Some(register_video(&state, &input.brand, key).await?),
        None => None,
    };
    let source = SourcePost {
        brand_key: &input.brand,
        slug: &input.slug,
        title: &input.title,
        description: &input.description,
        body_html: "",
        body_markdown: Some(&input.body_markdown),
        status: "published",
        publish_at: input.publish_at,
        video_asset: input.video_asset.as_deref(),
        vertical_video,
        tiktok_ai_generated: None,
        captions: SourceCaptions::default(),
        accounts: &[],
        social_links: &[],
    };
    let result = store::posts::import_source(&state.surreal, &source, now)
        .await
        .map_err(|error| BlogError::Invalid(error.to_string()))?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"post_id":result.post_id,"status":result.status})),
    )
        .into_response())
}

/// Native website form for the same approved Post command used by the CLI.
#[derive(Clone, Deserialize)]
pub(crate) struct SchedulePost {
    #[serde(default, rename = "_csrf")]
    csrf_token: String,
    brand: String,
    slug: String,
    title: String,
    description: String,
    facebook_caption: Option<String>,
    instagram_caption: Option<String>,
    threads_caption: Option<String>,
    youtube_caption: Option<String>,
    body_markdown: String,
    publish_at: String,
    video_asset: String,
    tiktok_ai_generated: String,
    facebook: Option<String>,
    instagram: Option<String>,
    threads: Option<String>,
    youtube: Option<String>,
    tiktok: Option<String>,
    linkedin: Option<String>,
    x: Option<String>,
}

impl SchedulePost {
    fn into_publish(self) -> Result<PublishPost, BlogError> {
        let publish_at = if self.publish_at.trim().is_empty() {
            None
        } else {
            Some(DateTime::parse_from_rfc3339(self.publish_at.trim())
                .map_err(|_| BlogError::Invalid("Use a publication time with its UTC offset, such as 2026-10-19T09:00:00-05:00.".into()))?
                .with_timezone(&Utc))
        };
        let mut providers = Vec::new();
        let mut account_ids = std::collections::BTreeMap::new();
        for (provider, account) in [
            (SocialProvider::Facebook, self.facebook),
            (SocialProvider::Instagram, self.instagram),
            (SocialProvider::Threads, self.threads),
            (SocialProvider::Youtube, self.youtube),
            (SocialProvider::Tiktok, self.tiktok),
            (SocialProvider::Linkedin, self.linkedin),
            (SocialProvider::X, self.x),
        ] {
            if let Some(account) = account.filter(|value| !value.trim().is_empty()) {
                account_ids.insert(provider.as_str().to_string(), account);
                providers.push(provider);
            }
        }
        let tiktok_ai_generated = match self.tiktok_ai_generated.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        Ok(PublishPost {
            brand: self.brand,
            slug: self.slug,
            title: self.title,
            description: self.description,
            facebook_caption: self
                .facebook_caption
                .filter(|value| !value.trim().is_empty()),
            instagram_caption: self
                .instagram_caption
                .filter(|value| !value.trim().is_empty()),
            threads_caption: self
                .threads_caption
                .filter(|value| !value.trim().is_empty()),
            youtube_caption: self
                .youtube_caption
                .filter(|value| !value.trim().is_empty()),
            body_markdown: self.body_markdown,
            providers,
            account_ids,
            publish_at,
            video_asset: (!self.video_asset.trim().is_empty()).then_some(self.video_asset),
            tiktok_ai_generated,
        })
    }
}

pub(crate) async fn schedule(
    State(state): State<ApiState>,
    AdminSession(actor): AdminSession,
    axum::Form(input): axum::Form<SchedulePost>,
) -> Result<Response, BlogError> {
    let result = match input.clone().into_publish() {
        Ok(command) => publish(State(state), AdminSession(actor), Json(command)).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(_) => Ok(axum::response::Redirect::to("/app/social/calendar").into_response()),
        Err(BlogError::Api(error)) if !matches!(error, ApiError::Db(_)) => {
            Err(BlogError::Api(error))
        }
        Err(error) => {
            let message = match error {
                BlogError::Invalid(message) => message,
                BlogError::AlreadyPublic(_) => "This Post is already public. Review its status before scheduling again.".into(),
                _ => "The publisher is unavailable. Your entered copy is preserved below; review the calendar before retrying.".into(),
            };
            Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                axum::response::Html(schedule_refusal(&input, &message)),
            )
                .into_response())
        }
    }
}

/// A refused native submission retains its authored copy without putting it in a URL.
fn schedule_refusal(input: &SchedulePost, message: &str) -> String {
    let mut fields = Vec::new();
    for (name, label, value) in [
        ("brand", "Brand", input.brand.as_str()),
        ("slug", "Slug", &input.slug),
        ("title", "Title", &input.title),
        (
            "description",
            "Website description (200 characters maximum)",
            &input.description,
        ),
        ("body_markdown", "Website Post", &input.body_markdown),
        (
            "publish_at",
            "Publication time (with UTC offset)",
            &input.publish_at,
        ),
        ("video_asset", "Uploaded video asset", &input.video_asset),
        (
            "tiktok_ai_generated",
            "AI-generated video (true or false)",
            &input.tiktok_ai_generated,
        ),
    ] {
        fields.push(if name == "body_markdown" {
            webapp::components::Field::textarea(label, name, value, 5)
        } else {
            webapp::components::Field::text(label, name, value)
        });
    }
    for (name, label, value) in [
        (
            "facebook_caption",
            "Facebook caption",
            &input.facebook_caption,
        ),
        (
            "instagram_caption",
            "Instagram caption",
            &input.instagram_caption,
        ),
        ("threads_caption", "Threads caption", &input.threads_caption),
        ("youtube_caption", "YouTube caption", &input.youtube_caption),
    ] {
        fields.push(webapp::components::Field::textarea(
            label,
            name,
            value.as_deref().unwrap_or(""),
            4,
        ));
    }
    for (provider, value) in [
        ("facebook", &input.facebook),
        ("instagram", &input.instagram),
        ("threads", &input.threads),
        ("youtube", &input.youtube),
        ("tiktok", &input.tiktok),
        ("linkedin", &input.linkedin),
        ("x", &input.x),
    ] {
        if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
            fields.push(webapp::components::Field::select(
                format!("{provider} destination"),
                provider,
                vec![webapp::components::Choice::new(value, value)],
                Some(value.clone()),
            ));
        }
    }
    webapp::brand_blog_posts::schedule_refusal_html(
        &input.brand,
        &input.csrf_token,
        message,
        fields,
    )
}

#[derive(Debug)]
pub(crate) enum BlogError {
    Api(ApiError),
    Invalid(String),
    AlreadyPublic(uuid::Uuid),
    AlreadyExists(uuid::Uuid),
    Unavailable,
}

impl From<ApiError> for BlogError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl IntoResponse for BlogError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Api(error) => return error.into_response(),
            Self::Invalid(error) => (StatusCode::UNPROCESSABLE_ENTITY, json!({"error":error})),
            Self::AlreadyPublic(id) => (
                StatusCode::CONFLICT,
                json!({"error":"Post already public; inspect its status before another publication","post_id":id}),
            ),
            Self::AlreadyExists(id) => (
                StatusCode::CONFLICT,
                json!({"error":"Post already exists; inspect its status before recording","post_id":id}),
            ),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error":"social publisher unavailable"}),
            ),
        };
        (status, Json(body)).into_response()
    }
}

fn database(error: impl std::fmt::Display) -> BlogError {
    BlogError::Api(ApiError::Db(error.to_string()))
}

async fn authorize(state: &ApiState, actor: &SessionData, brand: &str) -> Result<(), BlogError> {
    let person = actor.person_id.ok_or(ApiError::Forbidden)?;
    let keys = store::posts::manageable_brand_keys(&state.surreal, actor.role, person)
        .await
        .map_err(database)?;
    if keys.iter().any(|key| key == brand) {
        Ok(())
    } else {
        Err(ApiError::NotFound.into())
    }
}

pub(crate) async fn status(
    State(state): State<ApiState>,
    AdminSession(actor): AdminSession,
    Path((brand, slug)): Path<(String, String)>,
) -> Result<Response, BlogError> {
    authorize(&state, &actor, &brand).await?;
    let accounts = store::social_accounts::for_actor(
        &state.surreal,
        actor.role,
        actor.person_id.ok_or(ApiError::Forbidden)?,
    )
    .await
    .map_err(database)?
    .into_iter()
    .filter(|account| account.brand_key == brand)
    .collect::<Vec<_>>();
    let post = store::posts::find_by_brand_slug(&state.surreal, &brand, &slug)
        .await
        .map_err(database)?;
    let publications = match &post {
        Some(post) => store::social_publications::for_posts(&state.surreal, &[post.id])
            .await
            .map_err(database)?,
        None => Vec::new(),
    };
    let connection_setup = [
        "facebook",
        "instagram",
        "threads",
        "youtube",
        "linkedin",
        "x",
        "tiktok",
    ]
    .into_iter()
    .map(|provider| {
        (
            provider,
            crate::social_oauth::connection_configured(provider),
        )
    })
    .collect::<std::collections::BTreeMap<_, _>>();
    Ok(Json(json!({
        "post":post,"publications":publications,"accounts":accounts,
        "connection_setup":connection_setup,
        "publisher_configured":crate::cron_schedules::restate_broker_url().is_some()
    }))
    .into_response())
}

async fn selected_accounts(
    state: &ApiState,
    actor: &SessionData,
    input: &PublishPost,
) -> Result<Vec<AccountAlias>, BlogError> {
    if input.providers.is_empty() {
        return Err(BlogError::Invalid("choose at least one provider".into()));
    }
    if input.account_ids.len() != input.providers.len() {
        return Err(BlogError::Invalid(
            "account_ids must identify every requested provider account".into(),
        ));
    }
    let person = actor.person_id.ok_or(ApiError::Forbidden)?;
    let connected = store::social_accounts::for_actor(&state.surreal, actor.role, person)
        .await
        .map_err(database)?;
    let mut accounts = Vec::new();
    for provider in &input.providers {
        if accounts
            .iter()
            .any(|account: &AccountAlias| account.provider == provider.as_str())
        {
            return Err(BlogError::Invalid(
                "each provider may appear only once".into(),
            ));
        }
        let matches: Vec<_> = connected
            .iter()
            .filter(|account| {
                account.brand_key == input.brand
                    && account.provider == *provider
                    && account.status == "connected"
            })
            .collect();
        let [account] = matches.as_slice() else {
            return Err(BlogError::Invalid(format!(
                "{} requires exactly one connected account for this Brand",
                provider.as_str()
            )));
        };
        if input.account_ids.get(provider.as_str()) != Some(&account.provider_account_id) {
            return Err(BlogError::Invalid(format!(
                "{} account does not match the approved destination",
                provider.as_str()
            )));
        }
        if !provider.missing_scopes(&account.scopes).is_empty() {
            return Err(BlogError::Invalid(format!(
                "{} needs reconnection with publishing permissions",
                provider.as_str()
            )));
        }
        accounts.push(AccountAlias {
            provider: provider.as_str().into(),
            provider_account_id: account.provider_account_id.clone(),
        });
    }
    Ok(accounts)
}

async fn register_video(state: &ApiState, brand: &str, key: &str) -> Result<bool, BlogError> {
    if !key.starts_with(&format!("img/{brand}/")) || !crate::public_asset_key_is_safe(key) {
        return Err(BlogError::Invalid(
            "video_asset must be below img/<brand>/".into(),
        ));
    }
    let bytes = state.assets_storage.get(key).await.map_err(database)?.bytes;
    if bytes.len() > crate::assets_api::MAX_VIDEO_UPLOAD_BYTES {
        return Err(BlogError::Invalid(
            "video_asset exceeds the 64 MiB upload limit".into(),
        ));
    }
    let (width, height) = crate::admin::mp4_display_dimensions(&bytes)
        .ok_or_else(|| BlogError::Invalid("video_asset must be a readable MP4 video".into()))?;
    match store::posts::find_video(&state.surreal, key)
        .await
        .map_err(database)?
    {
        Some(video) if video.brand_key != brand => {
            return Err(BlogError::Invalid("video belongs to another Brand".into()))
        }
        Some(video) if video.width != width || video.height != height => {
            return Err(BlogError::Invalid(
                "registered video changed; upload under a new key".into(),
            ))
        }
        Some(_) => {}
        None => {
            store::posts::record_video(&state.surreal, brand, key, width, height)
                .await
                .map_err(database)?;
        }
    }
    Ok(height > width)
}

async fn prepare(
    state: &ApiState,
    actor: &SessionData,
    input: &PublishPost,
    now: DateTime<Utc>,
) -> Result<workflows::SocialPublishRequest, BlogError> {
    authorize(state, actor, &input.brand).await?;
    let existing = store::posts::find_by_brand_slug(&state.surreal, &input.brand, &input.slug)
        .await
        .map_err(database)?;
    if let Some(post) = &existing {
        // Repeating a command cannot move a live Post onto a new send schedule.
        if post.status == "published"
            || (post.status == "scheduled" && post.publish_at.is_some_and(|at| at <= now))
        {
            return Err(BlogError::AlreadyPublic(post.id));
        }
        if post.status == "cancelled" && input.publish_at.is_none() {
            return Err(BlogError::Invalid(
                "a cancelled Post needs a new future publish_at".into(),
            ));
        }
    }
    let accounts = selected_accounts(state, actor, input).await?;
    let publish_at = input
        .publish_at
        .or_else(|| {
            existing
                .as_ref()
                .filter(|post| post.status == "scheduled")
                .and_then(|post| post.publish_at)
        })
        .unwrap_or(now + chrono::Duration::seconds(2));
    if publish_at <= now {
        return Err(BlogError::Invalid(
            "publish_at must be in the future".into(),
        ));
    }
    let vertical_video = match &input.video_asset {
        Some(key) => Some(register_video(state, &input.brand, key).await?),
        None => None,
    };
    let source = SourcePost {
        brand_key: &input.brand,
        slug: &input.slug,
        title: &input.title,
        description: &input.description,
        body_html: "",
        body_markdown: Some(&input.body_markdown),
        status: "scheduled",
        publish_at,
        video_asset: input.video_asset.as_deref(),
        vertical_video,
        tiktok_ai_generated: input.tiktok_ai_generated,
        captions: SourceCaptions {
            facebook: input.facebook_caption.as_deref(),
            instagram: input.instagram_caption.as_deref(),
            threads: input.threads_caption.as_deref(),
            youtube: input.youtube_caption.as_deref(),
        },
        accounts: &accounts,
        social_links: existing
            .as_ref()
            .map_or(&[][..], |post| post.social_links.as_slice()),
    };
    let result = store::posts::import_source(&state.surreal, &source, now)
        .await
        .map_err(|error| BlogError::Invalid(error.to_string()))?;
    if !result.social_scheduled || !result.unresolved_accounts.is_empty() {
        return Err(BlogError::Invalid(
            "schedule held or an account disconnected; inspect Post status".into(),
        ));
    }
    Ok(workflows::SocialPublishRequest {
        post_id: result.post_id,
        publish_at,
    })
}

pub(crate) async fn publish(
    State(state): State<ApiState>,
    AdminSession(actor): AdminSession,
    Json(input): Json<PublishPost>,
) -> Result<Response, BlogError> {
    authorize(&state, &actor, &input.brand).await?;
    let broker = crate::cron_schedules::restate_broker_url().ok_or(BlogError::Unavailable)?;
    queue_publication(
        &state,
        &actor,
        &input,
        &broker,
        std::env::var("RESTATE_AUTH_TOKEN").ok().as_deref(),
    )
    .await
}

async fn queue_publication(
    state: &ApiState,
    actor: &SessionData,
    input: &PublishPost,
    broker: &str,
    auth_token: Option<&str>,
) -> Result<Response, BlogError> {
    let request = prepare(state, actor, input, Utc::now()).await?;
    let key = format!("{}:{}", request.post_id, request.publish_at.to_rfc3339());
    workflows::start_workflow(
        broker,
        auth_token,
        "SocialPublisher",
        &key,
        "run",
        &request,
        true,
    )
    .await
    .map_err(database)?;
    Ok((StatusCode::ACCEPTED, Json(json!({"post_id":request.post_id,"status":"scheduled","publish_at":request.publish_at,"outcome":"queued","brand":input.brand,"account_ids":input.account_ids}))).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Handle {
    handle: String,
}

pub(crate) async fn handle(
    State(state): State<ApiState>,
    AdminSession(actor): AdminSession,
    Path((brand, provider)): Path<(String, String)>,
    Json(input): Json<Handle>,
) -> Result<Response, BlogError> {
    authorize(&state, &actor, &brand).await?;
    store::social_handles::set(&state.surreal, &brand, &provider, &input.handle)
        .await
        .map_err(|error| BlogError::Invalid(error.to_string()))?;
    Ok(Json(json!({"brand":brand,"provider":provider,"handle":input.handle})).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Link {
    post_id: String,
    permalink: String,
}

pub(crate) async fn link(
    State(state): State<ApiState>,
    AdminSession(actor): AdminSession,
    Path((brand, slug, provider)): Path<(String, String, String)>,
    Json(input): Json<Link>,
) -> Result<Response, BlogError> {
    authorize(&state, &actor, &brand).await?;
    let post = store::posts::find_by_brand_slug(&state.surreal, &brand, &slug)
        .await
        .map_err(database)?
        .ok_or(ApiError::NotFound)?;
    let link = store::posts::encode_social_link(
        &state.surreal,
        post.id,
        &provider,
        &input.post_id,
        Some(&input.permalink),
    )
    .await
    .map_err(|error| BlogError::Invalid(error.to_string()))?;
    Ok(Json(link).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::persons::Role;
    use tower::ServiceExt;

    async fn fixture() -> (ApiState, SessionData) {
        let db = store::test_support::mem_surreal().await;
        let admin = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Publisher",
                "publisher@example.com",
                Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm = store::firms::create(
            &db,
            &store::firms::NewFirm {
                name: "Publication Test Firm".into(),
                status: "active".into(),
                entity_id: store::test_support::seed_entity(&db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        store::brands::create(
            &db,
            Role::Admin,
            Some(admin.id),
            &store::brands::NewBrand {
                name: "Publication Test Brand".into(),
                key: "publication-test".into(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        let app = crate::test_support::app_state(db).await;
        let state = ApiState {
            surreal: app.surreal,
            email: app.email,
            bootstrap_owner_email: None,
            bootstrap_company: "Publication Test Firm".into(),
            questionnaire_runtime: app.questionnaire_runtime,
            storage: app.storage,
            workflow_runtime: app.workflow_runtime,
            assets_storage: app.assets_storage,
            forms_registry: app.forms_registry,
            signature_provider: app.signature_provider,
            contract_reviewer: app.contract_reviewer,
            integration_providers: app.integration_providers,
        };
        let actor = SessionData {
            person_id: Some(admin.id),
            ..SessionData::fresh("publisher", Role::Admin)
        };
        (state, actor)
    }

    fn recorded_input() -> RecordPost {
        serde_json::from_value(json!({
            "brand":"publication-test", "slug":"recorded-video", "title":"A video update",
            "description":"An approved caption.", "body_markdown":"An approved caption.",
            "publish_at":"2026-01-01T12:00:00Z"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn recording_without_connections_is_public_and_never_schedules_delivery() {
        let (state, actor) = fixture().await;
        assert_eq!(
            record(
                State(state.clone()),
                AdminSession(actor.clone()),
                Json(recorded_input())
            )
            .await
            .unwrap()
            .status(),
            StatusCode::CREATED
        );
        let post =
            store::posts::find_by_brand_slug(&state.surreal, "publication-test", "recorded-video")
                .await
                .unwrap()
                .unwrap();
        assert_eq!(post.status, "published");
        assert!(post.selected_social_accounts.is_empty());
        let visible =
            store::posts::published_for_brand(&state.surreal, "publication-test", Utc::now())
                .await
                .unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, post.id);
        assert!(crate::blog::body_html(&post).contains("<p>An approved caption.</p>"));
        handle(
            State(state.clone()),
            AdminSession(actor.clone()),
            Path(("publication-test".into(), "tiktok".into())),
            Json(Handle {
                handle: "example".into(),
            }),
        )
        .await
        .unwrap();
        link(
            State(state.clone()),
            AdminSession(actor.clone()),
            Path((
                "publication-test".into(),
                "recorded-video".into(),
                "tiktok".into(),
            )),
            Json(Link {
                post_id: "1234567890123456789".into(),
                permalink: "https://www.tiktok.com/@example/video/1234567890123456789".into(),
            }),
        )
        .await
        .unwrap();
        let linked = store::posts::find_by_id(&state.surreal, post.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(store::posts::public_social_links(&linked, &[]).len(), 1);

        assert!(
            store::social_publications::for_posts(&state.surreal, &[post.id])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            record(State(state), AdminSession(actor), Json(recorded_input())).await,
            Err(BlogError::AlreadyExists(_))
        ));
    }

    #[tokio::test]
    async fn recording_refuses_foreign_brands_future_dates_and_invalid_media() {
        let (state, actor) = fixture().await;
        let mut input = recorded_input();
        input.brand = "another-brand".into();
        assert_eq!(
            record(
                State(state.clone()),
                AdminSession(actor.clone()),
                Json(input)
            )
            .await
            .unwrap_err()
            .into_response()
            .status(),
            StatusCode::NOT_FOUND
        );
        let mut input = recorded_input();
        input.publish_at = Utc::now() + chrono::Duration::days(1);
        assert!(matches!(
            record(
                State(state.clone()),
                AdminSession(actor.clone()),
                Json(input)
            )
            .await,
            Err(BlogError::Invalid(_))
        ));
        let mut input = recorded_input();
        input.video_asset = Some("img/another-brand/video.mp4".into());
        assert!(matches!(
            record(State(state.clone()), AdminSession(actor), Json(input)).await,
            Err(BlogError::Invalid(_))
        ));
        assert!(store::posts::find_by_brand_slug(
            &state.surreal,
            "publication-test",
            "recorded-video"
        )
        .await
        .unwrap()
        .is_none());
    }

    #[test]
    fn website_schedule_preserves_selected_destinations_and_offset() {
        let input: SchedulePost = serde_json::from_value(json!({
            "brand":"example", "slug":"legacy", "title":"Legacy",
            "description":"Approved caption", "body_markdown":"Approved article",
            "publish_at":"2030-10-19T09:00:00-05:00", "video_asset":"img/example/legacy.mp4",
            "tiktok_ai_generated":"false", "facebook":"page-approved", "instagram":"", "tiktok":"creator-approved"
        })).unwrap();
        let input = input.into_publish().unwrap();
        assert_eq!(
            input.publish_at.unwrap().to_rfc3339(),
            "2030-10-19T14:00:00+00:00"
        );
        assert_eq!(
            input.providers,
            vec![SocialProvider::Facebook, SocialProvider::Tiktok]
        );
        assert_eq!(input.account_ids.get("facebook").unwrap(), "page-approved");
        assert!(!input.account_ids.contains_key("instagram"));
        assert_eq!(input.tiktok_ai_generated, Some(false));
    }

    #[test]
    fn website_schedule_preserves_long_provider_captions_and_refused_copy() {
        let caption = "Synthetic approved caption. ".repeat(12);
        let form: SchedulePost = serde_json::from_value(json!({
            "brand":"example", "slug":"example", "title":"Example",
            "description":"Short website summary", "body_markdown":"Article",
            "publish_at":"2030-10-25T13:11:00Z", "video_asset":"", "tiktok_ai_generated":"",
            "facebook_caption":caption, "instagram_caption":caption,
            "threads_caption":"", "youtube_caption":"Video caption"
        }))
        .unwrap();
        let html = schedule_refusal(&form, "Connect an account");
        assert!(html.contains(&caption));
        assert!(html.contains("name=\"instagram_caption\""));
        let input = form.into_publish().unwrap();
        assert_eq!(input.facebook_caption.as_deref(), Some(caption.as_str()));
        assert_eq!(input.instagram_caption.as_deref(), Some(caption.as_str()));
        assert_eq!(input.threads_caption, None);
        assert_eq!(input.youtube_caption.as_deref(), Some("Video caption"));
        assert_eq!(input.description, "Short website summary");
    }

    #[test]
    fn refused_website_schedule_keeps_copy_and_escapes_markup() {
        let input: SchedulePost = serde_json::from_value(json!({
            "_csrf":"csrf-safe", "brand":"example", "slug":"legacy", "title":"Legacy",
            "description":"A caption", "body_markdown":"Article & family </textarea><script>bad()</script>",
            "publish_at":"", "video_asset":"", "tiktok_ai_generated":"false", "facebook":"approved-page"
        })).unwrap();
        let html = schedule_refusal(&input, "Choose a destination");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;/textarea>&lt;script>"));
        assert!(html.contains("Article &amp; family"));
        assert!(html.contains("csrf-safe"));
        assert!(html.contains("approved-page"));
        assert!(html.contains("Choose a destination"));
    }

    #[test]
    fn website_schedule_rejects_an_ambiguous_local_time() {
        let input: SchedulePost = serde_json::from_value(json!({
            "brand":"example", "slug":"legacy", "title":"Legacy",
            "description":"Caption", "body_markdown":"Article", "publish_at":"2030-10-19T09:00",
            "video_asset":"", "tiktok_ai_generated":""
        }))
        .unwrap();
        assert!(matches!(input.into_publish(), Err(BlogError::Invalid(_))));
    }

    fn input() -> PublishPost {
        serde_json::from_value(json!({
            "brand":"publication-test","slug":"example","title":"Example",
            "description":"A synthetic publication.","body_markdown":"Example body.","providers":["threads"],
            "account_ids":{"threads":"synthetic-threads"}
        })).unwrap()
    }

    #[tokio::test]
    async fn website_command_queues_one_durable_identity_for_a_future_schedule() {
        let (state, actor) = connected_fixture().await;
        let broker = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(202).set_body_string("queued"))
            .expect(2)
            .mount(&broker)
            .await;
        let publish_at = Utc::now() + chrono::Duration::days(2);
        let caption = "Approved synthetic provider caption. ".repeat(12);
        let form: SchedulePost = serde_json::from_value(json!({
            "brand":"publication-test", "slug":"website-schedule", "title":"Website schedule",
            "description":"Approved caption", "body_markdown":"Approved article",
            "publish_at":publish_at.to_rfc3339(), "video_asset":"", "tiktok_ai_generated":"",
            "threads":"synthetic-threads", "threads_caption":caption
        }))
        .unwrap();
        let command = form.into_publish().unwrap();
        for _ in 0..2 {
            let response = queue_publication(&state, &actor, &command, &broker.uri(), None)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
        }
        let post = store::posts::find_by_brand_slug(
            &state.surreal,
            "publication-test",
            "website-schedule",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(post.status, "scheduled");
        assert_eq!(post.threads_caption.as_deref(), Some(caption.as_str()));
        assert_eq!(
            post.publish_at.unwrap().to_rfc3339(),
            publish_at.to_rfc3339()
        );
        let requests = broker.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].url.path(), requests[1].url.path());
        assert!(requests[0].url.path().starts_with("/SocialPublisher/"));
        assert!(requests[0].url.path().ends_with("/run/send"));
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["post_id"], post.id.to_string());
        assert_eq!(body["publish_at"], serde_json::json!(publish_at));
    }

    #[tokio::test]
    async fn denied_brand_and_missing_connection_never_create_a_post() {
        let (state, actor) = fixture().await;
        let foreign = SessionData {
            person_id: Some(uuid::Uuid::now_v7()),
            ..SessionData::fresh("other-admin", Role::Admin)
        };
        assert!(matches!(
            prepare(&state, &foreign, &input(), Utc::now()).await,
            Err(BlogError::Api(ApiError::NotFound))
        ));
        assert!(matches!(
            prepare(&state, &actor, &input(), Utc::now()).await,
            Err(BlogError::Invalid(_))
        ));
        assert!(
            store::posts::find_by_brand_slug(&state.surreal, "publication-test", "example")
                .await
                .unwrap()
                .is_none()
        );
    }

    async fn connected_fixture() -> (ApiState, SessionData) {
        let (state, actor) = fixture().await;
        let provider = SocialProvider::Threads;
        let scopes: Vec<_> = provider
            .required_scopes()
            .iter()
            .map(|scope| (*scope).to_string())
            .collect();
        store::social_accounts::connect(
            &state.surreal,
            store::social_accounts::ConnectSocialAccount {
                actor_role: actor.role,
                actor_person_id: actor.person_id.unwrap(),
                brand_key: "publication-test",
                provider,
                provider_account_id: "synthetic-threads",
                display_name: None,
                scopes: &scopes,
                expires_at: None,
                access_token: "synthetic-token",
                refresh_token: None,
            },
            &cloud::FakeKms::new("synthetic-key"),
        )
        .await
        .unwrap();
        (state, actor)
    }

    #[tokio::test]
    async fn repeat_publication_keeps_its_schedule_and_public_posts_cannot_send_again() {
        let (state, actor) = connected_fixture().await;
        let mut wrong_account = input();
        wrong_account
            .account_ids
            .insert("threads".into(), "another-account".into());
        assert!(matches!(
            prepare(&state, &actor, &wrong_account, Utc::now()).await,
            Err(BlogError::Invalid(_))
        ));
        assert!(
            store::posts::find_by_brand_slug(&state.surreal, "publication-test", "example")
                .await
                .unwrap()
                .is_none()
        );
        let now = Utc::now();
        let first = prepare(&state, &actor, &input(), now).await.unwrap();
        let again = prepare(
            &state,
            &actor,
            &input(),
            now + chrono::Duration::milliseconds(100),
        )
        .await
        .unwrap();
        assert_eq!(first, again);
        let post = store::posts::find_by_id(&state.surreal, first.post_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(post.selected_social_accounts.len(), 1);
        assert!(
            matches!(prepare(&state, &actor, &input(), first.publish_at).await, Err(BlogError::AlreadyPublic(id)) if id == first.post_id)
        );
    }

    async fn read_status(state: &ApiState, actor: &SessionData) -> serde_json::Value {
        let response = status(
            State(state.clone()),
            AdminSession(actor.clone()),
            Path(("publication-test".into(), "example".into())),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn status_identifies_accounts_and_linking_never_sends_or_reschedules() {
        let (state, actor) = connected_fixture().await;
        let before = read_status(&state, &actor).await;
        assert!(before["post"].is_null());
        assert!(before["publisher_configured"].is_boolean());
        let setup = before["connection_setup"].as_object().unwrap();
        assert_eq!(setup.len(), 7);
        assert!(setup.values().all(serde_json::Value::is_boolean));
        assert_eq!(
            before["accounts"][0]["provider_account_id"],
            "synthetic-threads"
        );
        assert!(!before.to_string().contains("synthetic-token"));
        let request = prepare(&state, &actor, &input(), Utc::now()).await.unwrap();
        let response = handle(
            State(state.clone()),
            AdminSession(actor.clone()),
            Path(("publication-test".into(), "threads".into())),
            Json(Handle {
                handle: "example".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = link(
            State(state.clone()),
            AdminSession(actor.clone()),
            Path((
                "publication-test".into(),
                "example".into(),
                "threads".into(),
            )),
            Json(Link {
                post_id: "synthetic-post".into(),
                permalink: "https://www.threads.com/@example/post/synthetic-post".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let after = read_status(&state, &actor).await;
        assert_eq!(after["post"]["id"], request.post_id.to_string());
        assert_eq!(after["post"]["status"], "scheduled");
        assert_eq!(
            after["post"]["publish_at"],
            serde_json::json!(request.publish_at)
        );
        assert_eq!(
            after["post"]["social_links"][0]["post_id"],
            "synthetic-post"
        );
        assert_eq!(after["publications"], serde_json::json!([]));
        let denied = status(
            State(state),
            AdminSession(actor),
            Path(("another-brand".into(), "example".into())),
        )
        .await
        .unwrap_err()
        .into_response();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn publication_errors_keep_actionable_http_statuses() {
        for (error, expected) in [
            (
                BlogError::Invalid("wrong account".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                BlogError::AlreadyPublic(uuid::Uuid::now_v7()),
                StatusCode::CONFLICT,
            ),
            (BlogError::Unavailable, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            assert_eq!(error.into_response().status(), expected);
        }
    }

    #[tokio::test]
    async fn media_validation_rejects_cross_brand_keys_and_malformed_uploads() {
        let (state, _) = fixture().await;
        assert!(matches!(
            register_video(&state, "publication-test", "img/another-brand/video.mp4").await,
            Err(BlogError::Invalid(_))
        ));
        let key = "img/publication-test/video.mp4";
        state
            .assets_storage
            .put(key, b"not an MP4", "video/mp4")
            .await
            .unwrap();
        assert!(matches!(
            register_video(&state, "publication-test", key).await,
            Err(BlogError::Invalid(_))
        ));
        assert!(store::posts::find_video(&state.surreal, key)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn non_admin_requests_are_denied_before_body_parsing() {
        let (state, _) = fixture().await;
        for (actor, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (
                Some(SessionData::fresh("lawyer", Role::Lawyer)),
                StatusCode::FORBIDDEN,
            ),
        ] {
            for (path, content_type) in [
                ("/app/api/blog", "application/json"),
                ("/app/api/blog/record", "application/json"),
                (
                    "/app/api/blog/schedule",
                    "application/x-www-form-urlencoded",
                ),
            ] {
                let mut router = axum::Router::new()
                    .route("/app/api/blog", axum::routing::post(publish))
                    .route("/app/api/blog/record", axum::routing::post(record))
                    .route("/app/api/blog/schedule", axum::routing::post(schedule))
                    .with_state(state.clone());
                if let Some(actor) = &actor {
                    router = router.layer(axum::Extension(actor.clone()));
                }
                let response = router
                    .oneshot(
                        axum::http::Request::builder()
                            .method("POST")
                            .uri(path)
                            .header("content-type", content_type)
                            .body(axum::body::Body::from("not json"))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), expected);
            }
        }
    }
}
