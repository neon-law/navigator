//! JSON doors over Firm and Brand administration, so the CLI can rename a
//! Firm, set its Portal Theme Brand, move a Brand between Firms, detach a Brand, and delete an unworn
//! Brand or an empty Firm without the owner console.
//!
//! Every handler is a thin seam over one `store::firms` or `store::brands`
//! accessor, and that accessor is what authorizes: Owner for a move, Owner or
//! the Firm's own Admin for the rest. A refusal the caller can act on — a
//! Firm still owning Projects or Brands, a Brand still worn or named by a
//! Project — is a `409` carrying the store's own message. A scoped session
//! reaches none of these doors.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::brands::BrandError;
use store::firms::FirmError;
use uuid::Uuid;

use crate::api::{AdminSession, ApiError, ApiState};

fn conflict(message: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({ "error": "conflict", "message": message })),
    )
        .into_response()
}

fn firm_refusal(error: &FirmError) -> Response {
    match error {
        FirmError::NotAuthorized
        | FirmError::NoSuchFirm(_)
        | FirmError::UnknownBrand(_)
        | FirmError::BrandHasNoFirm(_) => ApiError::NotFound.into_response(),
        FirmError::FirmOwnsProjects(_)
        | FirmError::FirmOwnsBrands(_)
        | FirmError::ThemeBrandNotOwned(_, _) => conflict(&error.user_message()),
        _ => {
            tracing::error!(%error, "api: firm administration failed");
            ApiError::Db(error.to_string()).into_response()
        }
    }
}

fn brand_refusal(error: &BrandError) -> Response {
    match error {
        BrandError::NotAuthorized | BrandError::NoSuchBrand(_) => {
            ApiError::NotFound.into_response()
        }
        BrandError::StillReferenced => conflict(&error.user_message()),
        _ => {
            tracing::error!(%error, "api: brand administration failed");
            ApiError::Db(error.to_string()).into_response()
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct EditFirm {
    #[serde(default)]
    name: Option<String>,
    /// Absent leaves the Portal Theme Brand alone; `null` clears it.
    #[expect(clippy::option_option, reason = "absent and null mean different edits")]
    #[serde(default, deserialize_with = "present")]
    portal_theme_brand_key: Option<Option<String>>,
}

/// A field that is present, even as `null`, is `Some`.
#[expect(clippy::option_option, reason = "absent and null mean different edits")]
fn present<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// `PATCH /app/api/firms/{id}` — rename a Firm, or set or clear its Portal
/// Theme Brand. A rename leaves its slug, and so its Forgejo organization,
/// unchanged.
pub(crate) async fn update(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(firm_id): Path<Uuid>,
    Json(input): Json<EditFirm>,
) -> Response {
    if session.scope.is_some() {
        return ApiError::Forbidden.into_response();
    }
    let name = input.name.as_deref().map(str::trim);
    if name.is_some_and(str::is_empty) {
        return ApiError::MalformedBody("name must not be empty".into()).into_response();
    }
    if name.is_none() && input.portal_theme_brand_key.is_none() {
        return ApiError::MalformedBody("name or portal_theme_brand_key is required".into())
            .into_response();
    }
    let edit = store::firms::FirmEdit {
        name: name.map(str::to_string),
        portal_theme_brand_key: input.portal_theme_brand_key,
        ..store::firms::FirmEdit::default()
    };
    match store::firms::update(
        &state.surreal,
        session.role,
        session.person_id,
        firm_id,
        &edit,
    )
    .await
    {
        Ok(firm) => Json(serde_json::json!({
            "id": firm.id,
            "name": firm.name,
            "slug": firm.slug,
            "portal_theme_brand_key": firm.portal_theme_brand_key,
        }))
        .into_response(),
        Err(error) => firm_refusal(&error),
    }
}

/// `DELETE /app/api/firms/{id}` — delete a Firm that owns no Projects and no
/// Brands.
pub(crate) async fn delete(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(firm_id): Path<Uuid>,
) -> Response {
    if session.scope.is_some() {
        return ApiError::Forbidden.into_response();
    }
    match store::firms::delete(&state.surreal, session.role, session.person_id, firm_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => firm_refusal(&error),
    }
}

/// `DELETE /app/api/firms/{id}/brands/{key}` — stop a Firm wearing a Brand.
pub(crate) async fn detach_brand(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path((firm_id, key)): Path<(Uuid, String)>,
) -> Response {
    if session.scope.is_some() {
        return ApiError::Forbidden.into_response();
    }
    match store::firms::detach_brand(
        &state.surreal,
        session.role,
        session.person_id,
        firm_id,
        &key,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => firm_refusal(&error),
    }
}

#[derive(Deserialize)]
pub(crate) struct MoveBrand {
    firm_id: Uuid,
}

/// `PUT /app/api/brands/{key}/firm` — move a Brand to another Firm. Owner
/// only: a move changes whose legal entity the Brand's public pages name.
pub(crate) async fn move_brand(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(key): Path<String>,
    Json(input): Json<MoveBrand>,
) -> Response {
    if session.scope.is_some() {
        return ApiError::Forbidden.into_response();
    }
    match store::firms::move_brand(
        &state.surreal,
        session.role,
        session.person_id,
        &key,
        input.firm_id,
    )
    .await
    {
        Ok(()) => {
            Json(serde_json::json!({ "brand": key, "firm_id": input.firm_id })).into_response()
        }
        Err(error) => firm_refusal(&error),
    }
}

/// `DELETE /app/api/brands/{key}` — delete a Brand no Firm wears and no
/// Project names.
pub(crate) async fn delete_brand(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(key): Path<String>,
) -> Response {
    if session.scope.is_some() {
        return ApiError::Forbidden.into_response();
    }
    let brand = match store::brands::find_by_key(&state.surreal, &key).await {
        Ok(Some(brand)) => brand,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(error) => return brand_refusal(&error),
    };
    match store::brands::delete(&state.surreal, session.role, session.person_id, brand.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => brand_refusal(&error),
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use store::persons::{NewPerson, Role};
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::session::{now_unix_secs, SessionSource, SessionStore};

    fn bearer(person_id: Uuid, role: Role) -> String {
        let token =
            SessionStore::new(crate::test_support::TEST_SESSION_KEY).encode(&crate::SessionData {
                sub: "synthetic-firm-admin-actor".into(),
                email: None,
                person_id: Some(person_id),
                exp: now_unix_secs() + 60,
                role,
                csrf_token: "synthetic-csrf".into(),
                source: SessionSource::Cli,
                provider: None,
                viewing_as_dri: None,
                scope: None,
            });
        format!("Bearer {token}")
    }

    async fn send(
        app: &axum::Router,
        method: &str,
        uri: &str,
        auth: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, String) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, auth);
        let body = match body {
            Some(json) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    async fn firm(db: &store::surreal::SurrealDb, name: &str, admin: Uuid) -> Uuid {
        store::firms::create(
            db,
            &store::firms::NewFirm {
                name: name.into(),
                status: "active".into(),
                entity_id: store::test_support::seed_entity(db).await,
                admin_dri_person_id: admin,
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn person(db: &store::surreal::SurrealDb, email: &str, role: Role) -> Uuid {
        store::persons::create(db, &NewPerson::with_role("Synthetic person", email, role))
            .await
            .unwrap()
            .id
    }

    /// A Brand the Firm's Admin DRI creates and the Firm wears.
    async fn worn_brand(db: &store::surreal::SurrealDb, admin_dri: Uuid, firm: Uuid, key: &str) {
        store::brands::create(
            db,
            Role::Admin,
            Some(admin_dri),
            &store::brands::NewBrand {
                name: format!("Brand {key}"),
                key: key.into(),
                firm_id: Some(firm),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        store::firms::ensure_brand(db, firm, key).await.unwrap();
    }

    /// Folding one Firm into another: the Owner moves its Brand across, the
    /// emptied Firm can then be deleted, and the survivor is renamed — while
    /// another Firm's Admin reaches none of it.
    #[tokio::test]
    async fn an_owner_folds_one_firm_into_another_and_renames_it() {
        let db = store::test_support::mem_surreal().await;
        let owner = person(&db, "firm-api-owner@example.com", Role::Owner).await;
        let admin = person(&db, "firm-api-admin@example.com", Role::Admin).await;
        let outsider = person(&db, "firm-api-outsider@example.com", Role::Admin).await;
        let source = firm(&db, "Example Duplicate", admin).await;
        let target = firm(&db, "Example Survivor", admin).await;
        firm(&db, "Example Elsewhere", outsider).await;
        worn_brand(&db, admin, source, "example-brand").await;
        let app = crate::router(crate::test_support::app_state(db.clone()).await);
        let owner = bearer(owner, Role::Owner);
        let admin = bearer(admin, Role::Admin);
        let outsider = bearer(outsider, Role::Admin);
        let move_to = |firm: Uuid| Some(serde_json::json!({ "firm_id": firm }));

        let (status, body) = send(
            &app,
            "DELETE",
            &format!("/app/api/firms/{source}"),
            &owner,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "a Firm owning a Brand: {body}"
        );

        let route = "/app/api/brands/example-brand/firm";
        let (status, _) = send(&app, "PUT", route, &admin, move_to(target)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "an Admin may not move a Brand"
        );
        let (status, body) = send(&app, "PUT", route, &owner, move_to(target)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            store::firms::brand_keys_for_firm(&db, target)
                .await
                .unwrap(),
            vec!["example-brand".to_string()]
        );

        let (status, _) = send(
            &app,
            "DELETE",
            &format!("/app/api/firms/{source}"),
            &outsider,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "another Firm's Admin");
        let (status, body) = send(
            &app,
            "DELETE",
            &format!("/app/api/firms/{source}"),
            &owner,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(store::firms::find_by_id(&db, source)
            .await
            .unwrap()
            .is_none());

        let rename = Some(serde_json::json!({ "name": "Example Survivor PC" }));
        let (status, body) = send(
            &app,
            "PATCH",
            &format!("/app/api/firms/{target}"),
            &admin,
            rename,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let renamed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(renamed["name"], "Example Survivor PC");
        assert_eq!(renamed["slug"], "example-survivor", "the slug is immutable");
    }

    /// The Portal Theme Brand door: the Firm's own Admin sets it to a Brand
    /// the Firm owns and clears it; another Firm's Admin finds no Firm, and a
    /// Brand the Firm does not own is a conflict naming the rule.
    #[tokio::test]
    async fn a_firm_admin_sets_and_clears_the_portal_theme() {
        let db = store::test_support::mem_surreal().await;
        let admin = person(&db, "theme-api-admin@example.com", Role::Admin).await;
        let outsider = person(&db, "theme-api-outsider@example.com", Role::Admin).await;
        let themed = firm(&db, "Example Themed", admin).await;
        let elsewhere = firm(&db, "Example Elsewhere", outsider).await;
        worn_brand(&db, admin, themed, "example-house").await;
        worn_brand(&db, outsider, elsewhere, "example-other").await;
        let app = crate::router(crate::test_support::app_state(db.clone()).await);
        let admin = bearer(admin, Role::Admin);
        let outsider = bearer(outsider, Role::Admin);
        let route = format!("/app/api/firms/{themed}");
        let theme = |key: Option<&str>| Some(serde_json::json!({ "portal_theme_brand_key": key }));

        let (status, _) = send(
            &app,
            "PATCH",
            &route,
            &outsider,
            theme(Some("example-house")),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "another Firm's Admin");
        let (status, body) =
            send(&app, "PATCH", &route, &admin, theme(Some("example-other"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("one of this firm's own brands"), "{body}");

        let (status, body) =
            send(&app, "PATCH", &route, &admin, theme(Some("example-house"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let firm: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(firm["portal_theme_brand_key"], "example-house");
        assert_eq!(firm["name"], "Example Themed", "the name is untouched");

        let (status, body) = send(&app, "PATCH", &route, &admin, theme(None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(store::firms::find_by_id(&db, themed)
            .await
            .unwrap()
            .unwrap()
            .portal_theme_brand_key
            .is_none());

        let (status, _) = send(&app, "PATCH", &route, &admin, Some(serde_json::json!({}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "an edit naming nothing");
    }

    /// A Brand is deleted only once no Firm wears it.
    #[tokio::test]
    async fn a_brand_is_detached_before_it_is_deleted() {
        let db = store::test_support::mem_surreal().await;
        let admin = person(&db, "brand-api-admin@example.com", Role::Admin).await;
        let wearer = firm(&db, "Example Wearer", admin).await;
        worn_brand(&db, admin, wearer, "example-retired").await;
        let app = crate::router(crate::test_support::app_state(db.clone()).await);
        let admin = bearer(admin, Role::Admin);

        let (status, body) = send(
            &app,
            "DELETE",
            "/app/api/brands/example-retired",
            &admin,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "a worn Brand: {body}");
        let detach = format!("/app/api/firms/{wearer}/brands/example-retired");
        let (status, body) = send(&app, "DELETE", &detach, &admin, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (status, body) = send(
            &app,
            "DELETE",
            "/app/api/brands/example-retired",
            &admin,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(store::brands::find_by_key(&db, "example-retired")
            .await
            .unwrap()
            .is_none());
        let (status, _) = send(
            &app,
            "DELETE",
            "/app/api/brands/example-retired",
            &admin,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
