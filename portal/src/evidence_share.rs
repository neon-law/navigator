//! Firm-side invitation management and recipient access for one filed revision.

use std::fmt::Write as _;

use axum::{
    extract::{Form, Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Extension, Router,
};
use serde::Deserialize;
use store::{assets::Asset, evidence_shares as shares, postgres::PostgresDb};
use uuid::Uuid;
use webapp::app_chrome::{firm_brand_mark, AppBrandMark};
use webapp::html_escape::escape_attr as escape;

use crate::{session::SessionSource, AppState, SessionData};

const TERMS: &str = concat!(
    "Evidence access terms, version 2. ",
    "This invitation allows you to view and download the text export and any filed images shared with your account. ",
    "By selecting Accept and continue, you electronically sign these terms. ",
    "Navigator records your account and the time of acceptance, and maintains counts of timeline ",
    "views and download requests for this invitation. ",
    "Navigator also totals daily usage without storing account or invitation identifiers in the totals."
);

pub(crate) fn routes(state: &AppState) -> Router<AppState> {
    share_routes(state, None)
}

#[derive(Clone)]
struct ShareState {
    app: AppState,
    database: Option<PostgresDb>,
}

impl ShareState {
    async fn database(&self) -> Result<PostgresDb, StatusCode> {
        if let Some(db) = &self.database {
            return Ok(db.clone());
        }
        PostgresDb::from_env().await.map_err(unavailable)
    }
}

fn share_routes(state: &AppState, database: Option<PostgresDb>) -> Router<AppState> {
    Router::new()
        .route(
            "/app/projects/{code}/documents/{asset}/evidence-shares",
            get(manage).post(manage_action),
        )
        .route(
            "/app/evidence-shares/{id}",
            get(view)
                .head(|| async { StatusCode::METHOD_NOT_ALLOWED })
                .post(accept),
        )
        .route(
            "/app/evidence-shares/{id}/download",
            get(download).head(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .route(
            "/app/evidence-shares/{id}/media/{asset}",
            get(shared_media).head(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            (state.sessions.clone(), crate::csrf::CsrfMode::Strict),
            crate::csrf::require_csrf,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            (state.sessions.clone(), state.policy.clone()),
            crate::policy::require_policy,
        ))
        .with_state(ShareState {
            app: state.clone(),
            database,
        })
}

type Page = Result<Response, StatusCode>;

fn missing<T>(value: Option<T>) -> Result<T, StatusCode> {
    value.ok_or(StatusCode::NOT_FOUND)
}

fn unavailable<E>(_: E) -> StatusCode {
    StatusCode::SERVICE_UNAVAILABLE
}

fn person(session: &SessionData) -> Result<Uuid, StatusCode> {
    if session.source != SessionSource::Browser || session.viewing_as_dri.is_some() {
        return Err(StatusCode::NOT_FOUND);
    }
    missing(session.person_id)
}

fn recipient(session: &SessionData) -> Result<(Uuid, &str), StatusCode> {
    if !session.email_verified {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok((person(session)?, missing(session.email.as_deref())?))
}

async fn filed_asset(state: &AppState, project: Uuid, id: Uuid) -> Result<Asset, StatusCode> {
    missing(
        store::assets::find_by_id(&state.surreal, id)
            .await
            .map_err(unavailable)?
            .filter(|asset| asset.project_id == Some(project) && shares::eligible(asset)),
    )
}

async fn manage_asset(
    state: &AppState,
    session: &SessionData,
    code: &str,
    id: Uuid,
) -> Result<(Uuid, Asset), StatusCode> {
    let actor = person(session)?;
    let project = missing(
        store::projects::find_by_code(&state.surreal, code)
            .await
            .map_err(unavailable)?,
    )?;
    let viewer =
        store::access::matter_viewer(&state.surreal, Some(actor), session.role, project.id)
            .await
            .map_err(unavailable)?;
    if !viewer.is_some_and(store::access::MatterViewer::is_firm_side) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok((actor, filed_asset(state, project.id, id).await?))
}

async fn verified_bytes(state: &AppState, asset: &Asset) -> Result<Vec<u8>, StatusCode> {
    let object = state
        .storage
        .get(&asset.storage_key)
        .await
        .map_err(unavailable)?;
    if i64::try_from(object.bytes.len()).ok() != Some(asset.byte_size)
        || store::assets::sha256_hex(&object.bytes) != asset.sha256_hex
    {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(object.bytes)
}

async fn source(state: &AppState, asset: &Asset) -> Result<String, StatusCode> {
    String::from_utf8(verified_bytes(state, asset).await?)
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)
}

fn private(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn page(mark: Option<Extension<AppBrandMark>>, title: &str, body: &str) -> Response {
    let mark = mark.map_or_else(firm_brand_mark, |Extension(mark)| mark);
    let firm = escape(&mark.firm_name);
    let title = escape(title);
    let identity = mark.logo.map_or_else(
        || format!("<p>{firm}</p>"),
        |logo| {
            format!(
                r#"<img class="firm-mark" src="{}" alt="{}">"#,
                escape(&logo.src),
                escape(&logo.brand_name)
            )
        },
    );
    private(Html(format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>{title} | {firm}</title>
<style>
*{{box-sizing:border-box}}body{{margin:0;background:#eef2f6;color:#172637;font:1rem/1.55 system-ui}}
header{{background:white;padding:1.75rem 1rem;border-bottom:1px solid #cfdae6}}header div,main{{max-width:48rem;margin:auto}}
.firm-mark{{display:block;max-width:100%;height:auto;max-height:3rem;margin-bottom:.8rem}}
main{{padding:1rem}}h1,p{{margin:0 0 .75rem}}header p{{margin:0}}h1{{font-size:1.7rem}}
article,section{{background:white;border:1px solid #cfdae6;border-radius:.6rem;padding:1rem;margin:0 0 .8rem}}
article{{border-left:4px solid #35689b}}.timestamp,.muted{{color:#42566d}}article p{{white-space:pre-wrap;overflow-wrap:anywhere;margin:.5rem 0 0}}
a{{color:#164e84}}form{{margin:.5rem 0}}label{{display:block}}input,button{{font:inherit;padding:.4rem .65rem;margin:.3rem .25rem .3rem 0;max-width:100%}}
button{{cursor:pointer}}nav{{display:flex;gap:1rem;flex-wrap:wrap;margin:0 0 1rem}}
</style></head><body><header><div>{identity}<h1>{title}</h1></div></header><main>{body}</main></body></html>"#
    )).into_response())
}

fn csrf(session: &SessionData) -> String {
    format!(
        r#"<input type="hidden" name="_csrf" value="{}">"#,
        escape(&session.csrf_token)
    )
}

fn invitation(id: Uuid) -> String {
    format!("/app/evidence-shares/{id}")
}

#[derive(Deserialize, Default)]
struct TimelineSelection {
    timeline: Option<String>,
}

async fn selected_media(
    state: &AppState,
    project: Uuid,
    revision: Uuid,
    slug: Option<&str>,
) -> Result<Vec<shares::MediaRevision>, StatusCode> {
    let Some(slug) = slug else {
        return Ok(Vec::new());
    };
    let pages = store::matter_pages::list_for_project(&state.surreal, project)
        .await
        .map_err(unavailable)?;
    let page = missing(pages.into_iter().map(|page| page.entry).find(|entry| {
        entry.slug == slug
            && entry.kind == "whatsapp_timeline"
            && entry
                .document_revision
                .as_deref()
                .and_then(|value| value.parse().ok())
                == Some(revision)
    }))?;
    let media = shares::resolve_media(&state.surreal, project, &page.media)
        .await
        .map_err(unavailable)?;
    if media.len() != page.media.len() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    Ok(media)
}

fn media_fingerprint(media: &[shares::MediaRevision]) -> Result<String, StatusCode> {
    let json = serde_json::to_vec(media).map_err(unavailable)?;
    Ok(store::assets::sha256_hex(&json))
}

async fn manage(
    State(state): State<ShareState>,
    Extension(session): Extension<SessionData>,
    Path((code, id)): Path<(String, Uuid)>,
    Query(selection): Query<TimelineSelection>,
    mark: Option<Extension<AppBrandMark>>,
) -> Page {
    let (_, asset) = manage_asset(&state.app, &session, &code, id).await?;
    let project = missing(asset.project_id)?;
    let db = state.database().await?;
    let rows = shares::list(&db, project, id).await.map_err(unavailable)?;
    let media = selected_media(&state.app, project, id, selection.timeline.as_deref()).await?;
    let fingerprint = media_fingerprint(&media)?;
    let mut scope = format!(
        "<p>This invitation includes the text export and {} filed images.</p>",
        media.len()
    );
    if !media.is_empty() {
        scope.push_str("<details><summary>Review image revisions</summary><ul>");
        for image in &media {
            let _ = write!(scope, "<li>{}</li>", escape(&image.name));
        }
        scope.push_str("</ul></details>");
    }
    let csrf = csrf(&session);
    let email_field = webapp::components::Field::email("Recipient email", "email", "")
        .required()
        .maxlength(254)
        .render_html();
    let mut body = format!(
        r#"<p><a href="/app/projects/{}/documents/{id}">Back to document</a></p>
<section><p>{}</p><p>Each invitation lets one verified email address read and download this revision for 14 days.</p>{scope}
<form method="post">{csrf}<input type="hidden" name="media_fingerprint" value="{fingerprint}">{email_field}
<button>Create invitation</button></form></section>"#,
        escape(&code),
        escape(asset.filename.as_deref().unwrap_or_default())
    );
    for row in rows {
        let status = if row.revoked_at.is_some() {
            "Revoked".to_owned()
        } else if row.expires_at <= chrono::Utc::now() {
            "Expired".to_owned()
        } else if let Some(accepted_at) = row.accepted_at {
            format!("Accepted {}", accepted_at.format("%Y-%m-%d %H:%M UTC"))
        } else {
            "Awaiting acceptance".to_owned()
        };
        let _ = write!(
            body,
            "<section><p><strong>{}</strong></p><p>{status} · {} views · {} download requests</p>",
            escape(&row.recipient_email),
            row.opens,
            row.downloads
        );
        if row.revoked_at.is_none() && row.expires_at > chrono::Utc::now() {
            let _ = write!(
                body,
                r#"<p><a href="{}">Invitation link</a> (copy link address to share)</p>
<form method="post">{csrf}<button name="revoke" value="{}">Revoke invitation</button></form>"#,
                invitation(row.id),
                row.id
            );
        }
        body.push_str("</section>");
    }
    Ok(page(mark, "Evidence invitations", &body))
}

#[derive(Deserialize)]
struct ManageAction {
    email: Option<String>,
    revoke: Option<Uuid>,
    media_fingerprint: Option<String>,
}

async fn manage_action(
    State(state): State<ShareState>,
    Extension(session): Extension<SessionData>,
    Path((code, id)): Path<(String, Uuid)>,
    Query(selection): Query<TimelineSelection>,
    Form(action): Form<ManageAction>,
) -> Page {
    let (actor, asset) = manage_asset(&state.app, &session, &code, id).await?;
    let project = missing(asset.project_id)?;
    let db = state.database().await?;
    match (action.email, action.revoke) {
        (None, Some(revoke)) => {
            shares::revoke(&db, revoke, project, id)
                .await
                .map_err(unavailable)?;
        }
        (Some(email), None) => {
            let email = email.trim();
            if email.len() > 254
                || !email.is_ascii()
                || email
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
                || !email.split_once('@').is_some_and(|(user, domain)| {
                    !user.is_empty() && domain.contains('.') && !domain.contains('@')
                })
            {
                return Err(StatusCode::BAD_REQUEST);
            }
            let text = source(&state.app, &asset).await?;
            if !webapp::whatsapp_export::supported(&text) {
                return Ok(private(
                    (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "This document does not contain a supported WhatsApp text export.",
                    )
                        .into_response(),
                ));
            }
            let media =
                selected_media(&state.app, project, id, selection.timeline.as_deref()).await?;
            if selection.timeline.is_some()
                && action.media_fingerprint.as_deref() != Some(&media_fingerprint(&media)?)
            {
                return Err(StatusCode::CONFLICT);
            }
            shares::create_with_media(
                &db,
                project,
                id,
                &asset.sha256_hex,
                email,
                actor,
                TERMS,
                &media,
            )
            .await
            .map_err(unavailable)?;
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    }
    let suffix = selection
        .timeline
        .as_deref()
        .filter(|value| rules::pages::is_valid_slug(value))
        .map_or_else(String::new, |value| format!("?timeline={value}"));
    Ok(private(
        Redirect::to(&format!(
            "/app/projects/{code}/documents/{id}/evidence-shares{suffix}"
        ))
        .into_response(),
    ))
}

async fn accept(
    State(state): State<ShareState>,
    Extension(session): Extension<SessionData>,
    Path(id): Path<Uuid>,
) -> Page {
    let (person, email) = recipient(&session)?;
    let db = state.database().await?;
    if !shares::accept(&db, id, email, person)
        .await
        .map_err(unavailable)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(private(Redirect::to(&invitation(id)).into_response()))
}

#[derive(Deserialize, Default)]
struct TimelineQuery {
    #[serde(default)]
    page: usize,
}

async fn view(
    State(state): State<ShareState>,
    Extension(session): Extension<SessionData>,
    Path(id): Path<Uuid>,
    Query(query): Query<TimelineQuery>,
    mark: Option<Extension<AppBrandMark>>,
) -> Page {
    if !session.email_verified {
        let body = format!(
            r#"<section><p>Sign in with a verified email address to open an evidence invitation.</p>
<p><a href="/auth/login?return_to={}">Sign in again</a></p></section>"#,
            invitation(id),
        );
        return Ok((
            StatusCode::FORBIDDEN,
            page(mark, "Verify your email", &body),
        )
            .into_response());
    }
    let (person, email) = recipient(&session)?;
    let db = state.database().await?;
    let share = missing(
        shares::for_recipient(&db, id, email, person)
            .await
            .map_err(unavailable)?,
    )?;
    if share.accepted_at.is_none() {
        let body = format!(
            r#"<section><p>{}</p><form method="post">{}
<button type="submit">Accept and continue</button></form></section>"#,
            escape(&share.terms),
            csrf(&session)
        );
        return Ok(page(mark, "Evidence access terms", &body));
    }
    let text = shared_source(&state.app, &share).await?;
    let base = invitation(id);
    let images: Vec<_> = share
        .media
        .iter()
        .map(|image| {
            let preview = format!("{base}/media/{}", image.id);
            webapp::whatsapp_export::TimelineImage {
                name: image.name.clone(),
                at: image.at,
                original_time: image.original_time,
                download: format!("{preview}?download=true"),
                preview,
            }
        })
        .collect();
    let body = webapp::whatsapp_export::timeline_with_images(
        &text,
        query.page,
        &base,
        &format!("{base}/download"),
        &images,
    );
    record_access(&db, id, email, person, false).await?;
    Ok(page(mark, "WhatsApp timeline", &body))
}

async fn shared_source(
    state: &AppState,
    share: &shares::EvidenceShare,
) -> Result<String, StatusCode> {
    let asset = filed_asset(state, share.project_id, share.asset_id).await?;
    if asset.sha256_hex != share.sha256 {
        return Err(StatusCode::NOT_FOUND);
    }
    source(state, &asset).await
}

async fn record_access(
    db: &PostgresDb,
    id: Uuid,
    email: &str,
    person: Uuid,
    download: bool,
) -> Result<(), StatusCode> {
    if !shares::record_access(db, id, email, person, download)
        .await
        .map_err(unavailable)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(())
}

async fn download(
    State(state): State<ShareState>,
    Extension(session): Extension<SessionData>,
    Path(id): Path<Uuid>,
) -> Page {
    let (person, email) = recipient(&session)?;
    let db = state.database().await?;
    let share = accepted_share(&db, id, email, person).await?;
    let text = shared_source(&state.app, &share).await?;
    record_access(&db, id, email, person, true).await?;
    Ok(private(
        (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=whatsapp-export.txt",
                ),
            ],
            text,
        )
            .into_response(),
    ))
}

#[derive(Deserialize, Default)]
struct MediaQuery {
    #[serde(default)]
    download: bool,
}

async fn shared_media(
    State(state): State<ShareState>,
    Extension(session): Extension<SessionData>,
    Path((id, asset_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<MediaQuery>,
) -> Page {
    let (person, email) = recipient(&session)?;
    let db = state.database().await?;
    let share = accepted_share(&db, id, email, person).await?;
    let image = missing(share.media.iter().find(|image| image.id == asset_id))?;
    let asset = missing(
        store::assets::find_by_id(&state.app.surreal, asset_id)
            .await
            .map_err(unavailable)?
            .filter(|asset| asset.sha256_hex == image.sha256),
    )?;
    let content_type = missing(shares::image_content_type(&asset, share.project_id))?;
    let bytes = verified_bytes(&state.app, &asset).await?;
    if query.download {
        record_access(&db, id, email, person, true).await?;
    } else {
        accepted_share(&db, id, email, person).await?;
    }
    let disposition = match (query.download, content_type) {
        (true, "image/png") => "attachment; filename=evidence.png",
        (true, _) => "attachment; filename=evidence.jpg",
        (false, _) => "inline",
    };
    Ok(private(
        (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CONTENT_DISPOSITION, disposition),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
    ))
}

async fn accepted_share(
    db: &PostgresDb,
    id: Uuid,
    email: &str,
    person: Uuid,
) -> Result<shares::EvidenceShare, StatusCode> {
    missing(
        shares::for_recipient(db, id, email, person)
            .await
            .map_err(unavailable)?
            .filter(|share| share.accepted_at.is_some()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use sqlx::postgres::{PgConnectOptions, PgSslMode};
    use store::{documents, persons::Role, test_support};
    use tower::ServiceExt;

    async fn request(
        router: &Router,
        sessions: &crate::SessionStore,
        session: Option<&SessionData>,
        path: &str,
        form: Option<String>,
    ) -> Response {
        let mut request = Request::builder().uri(path);
        if let Some(session) = session {
            request = request.header(
                header::COOKIE,
                format!("navigator_session={}", sessions.encode(session)),
            );
        }
        let body = if let Some(form) = form {
            request = request
                .method("POST")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            Body::from(form)
        } else {
            Body::empty()
        };
        router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap()
    }

    fn session(person_id: Uuid, email: &str, role: Role) -> SessionData {
        SessionData {
            person_id: Some(person_id),
            email: Some(email.to_owned()),
            email_verified: true,
            ..SessionData::fresh(email, role)
        }
    }

    async fn temporary_database() -> Option<(PostgresDb, PostgresDb, String)> {
        let port = store::test_support::postgres_test_port();
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("synthetic")
            .database(
                &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                    .unwrap_or_else(|_| "smoke".into()),
            )
            .ssl_mode(PgSslMode::Disable);
        let admin = PostgresDb::connect(options.clone()).await.unwrap();
        let name = format!("share_test_{}", Uuid::now_v7().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(admin.pool())
            .await
            .unwrap();
        let db = PostgresDb::connect(options.database(&name)).await.unwrap();
        sqlx::raw_sql(concat!(
            include_str!(
                "../../store/postgres/migrations/20261116220000_create_evidence_share.sql"
            ),
            "ALTER TABLE evidence_share ADD COLUMN media JSONB NOT NULL DEFAULT '[]'::jsonb;",
        ))
        .execute(db.pool())
        .await
        .unwrap();
        Some((admin, db, name))
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn evidence_share_routes_gate_terms_and_download_on_the_recipient() {
        let fixture_name = format!("evidence_share_{}", Uuid::now_v7().simple());
        let Some(fixture) = test_support::server_surreal(&fixture_name).await else {
            return;
        };
        let Some((admin, db, database)) = temporary_database().await else {
            return;
        };
        let surreal = fixture.db;
        store::surreal::test_support::seed_compiled_brands(&surreal).await;
        let lawyer = test_support::ensure_person(
            &surreal,
            &store::persons::NewPerson {
                role: Role::Lawyer,
                ..store::persons::NewPerson::new("Synthetic Lawyer", "evidence-lawyer@example.test")
            },
        )
        .await;
        let project_id = test_support::seed_project_surreal(&surreal, "Synthetic Evidence").await;
        let project = store::projects::find_by_id(&surreal, project_id)
            .await
            .unwrap()
            .unwrap();
        store::projects::designate_dri_in_surreal(
            &surreal,
            project_id,
            lawyer.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
        let mut state = crate::test_support::app_state(surreal).await;
        state.policy = crate::policy::PolicyClient::embedded().unwrap();
        let source = b"[11/3/26, 9:00:00 AM] Alex: <script>synthetic</script>\r\n";
        let document = documents::ingest_bytes(
            &state.surreal,
            &state.storage,
            &documents::IngestArgs {
                project_id,
                source: documents::source::UPLOAD,
                filename: "whatsapp.txt",
                kind: "exhibit",
                content_type: "text/plain",
                description: None,
                secondary_storage_key: None,
                visibility: documents::visibility::INTERNAL,
            },
            source,
        )
        .await
        .unwrap();
        let mut image_bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut image_bytes, image::ImageFormat::Png)
            .unwrap();
        let image_bytes = image_bytes.into_inner();
        let image = documents::ingest_bytes(
            &state.surreal,
            &state.storage,
            &documents::IngestArgs {
                project_id,
                source: documents::source::UPLOAD,
                filename: "synthetic.png",
                kind: "exhibit",
                content_type: "image/png",
                description: None,
                secondary_storage_key: None,
                visibility: documents::visibility::INTERNAL,
            },
            &image_bytes,
        )
        .await
        .unwrap();
        let page = rules::pages::PageEntry {
            slug: "whatsapp-timeline".into(),
            kind: "whatsapp_timeline".into(),
            title: "WhatsApp timeline".into(),
            draft: None,
            document_revision: Some(document.asset_id.to_string()),
            media: vec![rules::pages::TimelineMedia {
                document_revision: image.asset_id.to_string(),
                created_at: Some(
                    chrono::DateTime::parse_from_rfc3339("2026-11-03T09:01:00Z").unwrap(),
                ),
            }],
            lenses: [(
                "lawyer".into(),
                vec![
                    "whatsapp_timeline".into(),
                    "boundary_note".into(),
                    "provenance_statement".into(),
                ],
            )]
            .into(),
        };
        store::matter_pages::replace_for_project(&state.surreal, project_id, &[page])
            .await
            .unwrap();
        let router = crate::session_boundary(
            share_routes(&state, Some(db.clone())),
            &state.sessions,
            &state.auth,
        )
        .with_state(state.clone())
        .layer(Extension(AppBrandMark {
            firm_name: "Guieb & Shook".into(),
            logo: Some(webapp::components::AppLogo {
                src: "/public/brand/guieb-shook/logo.svg".into(),
                brand_name: "Guieb & Shook".into(),
                href: "/".into(),
            }),
            ..AppBrandMark::default()
        }))
        .layer(tower_cookies::CookieManagerLayer::new());
        let lawyer = session(lawyer.id, &lawyer.email, Role::Lawyer);
        let guest = session(Uuid::now_v7(), "recipient@example.test", Role::Client);
        let stranger = session(Uuid::now_v7(), "stranger@example.test", Role::Lawyer);
        let manage = format!(
            "/app/projects/{}/documents/{}/evidence-shares?timeline=whatsapp-timeline",
            project.code, document.asset_id
        );
        assert_eq!(
            request(&router, &state.sessions, Some(&stranger), &manage, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        let selected = selected_media(
            &state,
            project_id,
            document.asset_id,
            Some("whatsapp-timeline"),
        )
        .await
        .unwrap();
        let fingerprint = media_fingerprint(&selected).unwrap();
        let form = format!(
            "email=recipient%40example.test&media_fingerprint={fingerprint}&_csrf={}",
            lawyer.csrf_token
        );
        assert_eq!(
            request(&router, &state.sessions, Some(&lawyer), &manage, Some(form))
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        let grants = shares::list(&db, project_id, document.asset_id)
            .await
            .unwrap();
        let id = grants[0].id;
        let url = invitation(id);
        let download = format!("{url}/download");
        let image_url = format!("{url}/media/{}", image.asset_id);
        assert_eq!(grants[0].media.len(), 1);
        assert_eq!(grants[0].media[0].id, image.asset_id);
        assert_eq!(
            request(&router, &state.sessions, Some(&guest), &image_url, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert!(request(&router, &state.sessions, None, &url, None)
            .await
            .status()
            .is_redirection());
        assert_eq!(
            request(&router, &state.sessions, Some(&stranger), &url, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        let terms = request(&router, &state.sessions, Some(&guest), &url, None).await;
        assert_eq!(terms.status(), StatusCode::OK);
        let terms = to_bytes(terms.into_body(), 64 * 1024).await.unwrap();
        let terms = std::str::from_utf8(&terms).unwrap();
        assert!(terms.contains("Accept and continue"));
        assert!(terms.contains("Evidence access terms | Guieb &amp; Shook"));
        assert!(terms.contains(r#"src="/public/brand/guieb-shook/logo.svg""#));
        assert!(!terms.contains("synthetic</script>"));
        assert_eq!(
            request(&router, &state.sessions, Some(&guest), &download, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(
                &router,
                &state.sessions,
                Some(&guest),
                &url,
                Some(String::new())
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        let form = format!("_csrf={}", guest.csrf_token);
        assert_eq!(
            request(&router, &state.sessions, Some(&guest), &url, Some(form))
                .await
                .status(),
            StatusCode::SEE_OTHER
        );

        let timeline = request(&router, &state.sessions, Some(&guest), &url, None).await;
        assert_eq!(
            timeline.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        let timeline = to_bytes(timeline.into_body(), 64 * 1024).await.unwrap();
        assert!(std::str::from_utf8(&timeline)
            .unwrap()
            .contains("&lt;script&gt;synthetic&lt;/script&gt;"));
        assert!(std::str::from_utf8(&timeline).unwrap().contains(&image_url));
        assert_eq!(
            request(&router, &state.sessions, Some(&stranger), &image_url, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        let wrong_image = format!("{url}/media/{}", Uuid::now_v7());
        assert_eq!(
            request(&router, &state.sessions, Some(&guest), &wrong_image, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        let raster = request(&router, &state.sessions, Some(&guest), &image_url, None).await;
        assert_eq!(raster.status(), StatusCode::OK);
        assert_eq!(raster.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(raster.headers()[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(
            to_bytes(raster.into_body(), 64 * 1024)
                .await
                .unwrap()
                .as_ref(),
            image_bytes
        );
        let response = request(&router, &state.sessions, Some(&guest), &download, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap()
                .as_ref(),
            source
        );
        let form = format!("revoke={id}&_csrf={}", lawyer.csrf_token);
        assert_eq!(
            request(&router, &state.sessions, Some(&lawyer), &manage, Some(form))
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            request(&router, &state.sessions, Some(&guest), &download, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );

        assert_eq!(
            request(&router, &state.sessions, Some(&guest), &image_url, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        drop(router);
        db.pool().close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
            .execute(admin.pool())
            .await
            .unwrap();
    }
}
