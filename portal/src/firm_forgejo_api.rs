//! `POST /app/api/firms/{id}/forgejo-organization` — create or reconcile an
//! existing Firm's organization on the installation's Forgejo.
//!
//! The installation lets only a site administrator create organizations. This
//! door lets a Firm's Admin (or Owner) ask Navigator to do it with its own
//! provisioning identity, so no person holds site administration. The forge
//! work and the record are [`store::firms::forgejo_organization::provision`];
//! this module only authorizes, opens PostgreSQL, and maps the outcome.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use cloud::forgejo_organization::OrganizationError;
use store::firm_capability::FirmCapability;
use store::firms::forgejo_organization::{self as organization, ProvisionError};
use uuid::Uuid;

use crate::api::{AdminSession, ApiError, ApiState};

fn refused(status: StatusCode, error: &'static str, message: Option<&str>) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": error, "message": message })),
    )
        .into_response()
}

fn provision_error(error: &ProvisionError, firm_id: Uuid) -> Response {
    let (status, code) = match error {
        ProvisionError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        ProvisionError::Unavailable(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "forgejo_provisioning_unconfigured",
        ),
        ProvisionError::Organization(OrganizationError::NotPermitted) => {
            (StatusCode::BAD_GATEWAY, "forgejo_provisioner_not_permitted")
        }
        ProvisionError::Organization(OrganizationError::NameUnavailable { .. })
        | ProvisionError::RecordedElsewhere => {
            (StatusCode::CONFLICT, "forgejo_organization_unavailable")
        }
        ProvisionError::Organization(_) => (StatusCode::BAD_GATEWAY, "forgejo_unavailable"),
        ProvisionError::Db(_) | ProvisionError::Postgres(_) | ProvisionError::Mirror(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "store_error")
        }
    };
    tracing::warn!(
        event = "firm_forgejo_organization_refused",
        outcome = code,
        firm_id = %firm_id,
        %error
    );
    // Store faults stay in the log; every other message names a setting, a
    // forge status, or the organization, never a credential.
    let message = (status != StatusCode::INTERNAL_SERVER_ERROR).then(|| error.to_string());
    refused(status, code, message.as_deref())
}

/// Create or reconcile the Firm's Forgejo organization and record it.
pub(crate) async fn provision(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(firm_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if session.scope.is_some() {
        return Err(ApiError::Forbidden);
    }
    let allowed = FirmCapability::ManageMembership
        .resolve(&state.surreal, session.role, session.person_id, firm_id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    if !allowed.is_allowed() {
        return Err(ApiError::NotFound);
    }
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        return Ok(refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "forgejo_organization_requires_postgresql",
            None,
        ));
    };
    let kms = crate::integrations::runtime_kms_from_env().await;
    let lookup = |key: &str| std::env::var(key).ok();
    Ok(
        match organization::provision(&state.surreal, &pg, firm_id, kms.as_ref(), lookup).await {
            Ok((reconciled, recorded)) => {
                tracing::info!(
                    event = "firm_forgejo_organization_reconciled",
                    firm_id = %firm_id,
                    created = reconciled.created,
                    owner_added = reconciled.owner_added
                );
                Json(serde_json::json!({
                    "firm_id": recorded.firm_id,
                    "organization": recorded.organization,
                    "forgejo_id": recorded.forgejo_id,
                    "owner_login": recorded.owner_login,
                    "created": reconciled.created,
                    "made_private": reconciled.made_private,
                    "owner_added": reconciled.owner_added,
                    "provisioned_at": recorded.provisioned_at,
                    "reconciled_at": recorded.reconciled_at,
                }))
                .into_response()
            }
            Err(error) => provision_error(&error, firm_id),
        },
    )
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
                sub: "synthetic-forgejo-actor".into(),
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

    async fn send(app: &axum::Router, method: &str, uri: &str, auth: &str) -> (StatusCode, String) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn only_a_firms_administrator_reaches_provisioning_and_the_directory_names_the_slug() {
        let db = store::test_support::mem_surreal().await;
        let admin = store::persons::create(
            &db,
            &NewPerson::with_role(
                "Synthetic administrator",
                "forgejo-admin@example.com",
                Role::Admin,
            ),
        )
        .await
        .unwrap();
        let outsider = store::persons::create(
            &db,
            &NewPerson::with_role(
                "Synthetic unrelated administrator",
                "forgejo-outsider@example.com",
                Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm = store::firms::create(
            &db,
            &store::firms::NewFirm {
                name: "Example Law PC".into(),
                status: "active".into(),
                entity_id: store::test_support::seed_entity(&db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        let app = crate::router(crate::test_support::app_state(db).await);
        let route = format!("/app/api/firms/{}/forgejo-organization", firm.id);

        let (status, _) = send(&app, "POST", &route, &bearer(outsider.id, Role::Admin)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "another Firm's administrator"
        );
        let (status, _) = send(&app, "POST", &route, &bearer(outsider.id, Role::Client)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "a client");
        let unknown = format!("/app/api/firms/{}/forgejo-organization", Uuid::now_v7());
        let (status, _) = send(&app, "POST", &unknown, &bearer(admin.id, Role::Owner)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "an unknown Firm");

        // The Firm's own administrator is admitted; without PostgreSQL the
        // door refuses before any forge request is made.
        let (status, body) = send(&app, "POST", &route, &bearer(admin.id, Role::Admin)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(
            body.contains("forgejo_organization_requires_postgresql"),
            "{body}"
        );

        let (status, body) = send(
            &app,
            "GET",
            "/app/api/firms",
            &bearer(admin.id, Role::Admin),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let firms: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(firms[0]["slug"], "example-law-pc", "{body}");
    }
}
