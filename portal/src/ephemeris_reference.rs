//! Administrative reference-source discovery, independent of private charts.

use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};

/// Returns only the raw source eligible for authenticated local acquisition.
pub(crate) async fn sources(
    _session: crate::api::AdminSession,
) -> Result<impl IntoResponse, StatusCode> {
    let mut manifest: ephemeris::EphemerisManifest =
        serde_json::from_str(include_str!("../../ephemeris/manifest.json"))
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    // Generated Horizons kernels have a separate, unresolved redistribution gate.
    manifest.files.retain(|file| file.name == "de440.bsp");
    manifest
        .validate()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if manifest.files.len() != 1 {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(manifest),
    ))
}

#[cfg(test)]
mod tests {
    use crate::session::{SessionData, SessionSource};
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use store::persons::Role;
    use tower::ServiceExt;

    #[tokio::test]
    async fn administrator_receives_only_the_eligible_pinned_raw_source() {
        let db = store::surreal::test_support::mem().await;
        let mut app = crate::test_support::app_state(db).await;
        app.policy = crate::policy::PolicyClient::embedded().unwrap();
        let router = crate::router(app.clone());
        for role in [Role::Owner, Role::Admin] {
            let mut session = SessionData::fresh("reference-download", role);
            session.source = SessionSource::Cli;
            let token = app.sessions.encode(&session);
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/app/api/ephemeris/sources")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "the authenticated source catalog must exist"
            );
            assert_eq!(response.headers()["cache-control"], "private, no-store");
            let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
            let manifest: ephemeris::EphemerisManifest = serde_json::from_slice(&bytes).unwrap();
            manifest.validate().unwrap();
            assert_eq!(
                manifest.files.len(),
                1,
                "unresolved raw sources cannot enter the download catalog"
            );
            assert_eq!(manifest.files[0].name, "de440.bsp");
            assert!(!String::from_utf8(bytes.to_vec())
                .unwrap()
                .to_lowercase()
                .contains("chiron"));
        }
    }
    #[tokio::test]
    async fn canonical_session_boundary_refuses_ineligible_sessions_before_catalogue() {
        use crate::session::{DocumentScope, SessionScope};
        let db = store::surreal::test_support::mem().await;
        let mut app = crate::test_support::app_state(db).await;
        app.policy = crate::policy::PolicyClient::embedded().unwrap();
        let sessions = app.sessions.clone();
        let router = crate::router(app);
        let mut cases = vec![(None, StatusCode::UNAUTHORIZED)];
        for role in [Role::Lawyer, Role::Clerk, Role::Client] {
            cases.push((
                Some(SessionData::fresh("lower-role", role)),
                StatusCode::FORBIDDEN,
            ));
        }
        let mut expired = SessionData::fresh("expired-admin", Role::Admin);
        expired.exp = 0;
        cases.push((Some(expired), StatusCode::UNAUTHORIZED));
        let mut ci = SessionData::fresh("scoped-owner", Role::Owner);
        ci.source = SessionSource::Ci;
        ci.scope = Some(SessionScope::Document(DocumentScope {
            project_id: uuid::Uuid::new_v4(),
            project_code: "synthetic-reference".into(),
        }));
        cases.push((Some(ci), StatusCode::FORBIDDEN));
        for (session, expected) in cases {
            let mut request = Request::builder().uri("/app/api/ephemeris/sources");
            if let Some(session) = session {
                request = request.header(
                    "authorization",
                    format!("Bearer {}", sessions.encode(&session)),
                );
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("de440.bsp"));
        }
    }
}
