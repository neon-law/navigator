//! The attorney's own email-signature form on the self-service `/app/profile`
//! page (LAW-280): a native `POST /app/profile/mail-signature` twin, the
//! shape `post_brand_letterhead` established — form, fields, save, redirect
//! back with a saved/failed banner.
//!
//! The subject is always the signed session's Person, resolved server-side;
//! the form carries no subject field at all, so there is nothing to forge —
//! and [`store::mail_signatures::save_for_actor`] repeats the gate on the
//! write itself, refusing any other actor with the same non-disclosing
//! `NotFound` an unknown subject gets. The redirect names only fixed flash
//! codes, never request text, and the profile page maps the codes to
//! sentences.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Extension;
use serde::Deserialize;

use crate::admin::AdminState;
use crate::session::SessionData;

/// The profile page's signature form posts here.
pub const PROFILE_MAIL_SIGNATURE_PATH: &str = "/app/profile/mail-signature";

/// Why a save did not stand. Each variant maps to one wire status, and the
/// wire status to one fixed flash code — never request text.
#[derive(Debug)]
pub(crate) enum SaveRefusal {
    /// The session carries no linked Person, so there is no subject to save
    /// for. Nothing was touched.
    NoPerson,
    /// The store's validation refused a field. The portal's flash is a fixed
    /// code — the store's message never reaches the redirect — and the
    /// profile page's sentence names every constraint the store enforces.
    Invalid,
    /// The store's non-disclosing refusal: an unknown subject, or an actor
    /// who is not the subject. The same answer either way.
    NotFound,
    /// PostgreSQL is unreachable on this deployment.
    Unavailable,
    /// Everything else, logged server-side.
    Failed,
}

impl SaveRefusal {
    fn status(&self) -> StatusCode {
        match self {
            Self::NoPerson => StatusCode::FORBIDDEN,
            Self::Invalid => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Failed => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// A short, stable flash code for the profile page; the page maps it to
    /// a sentence. The status class is enough — the store already validated,
    /// and nothing from the request may reach the redirect.
    fn flash_code(&self) -> &'static str {
        match self {
            Self::NoPerson | Self::NotFound => "not_found",
            Self::Invalid => "invalid",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }
}

/// Back to the profile page's signature card with a fixed flash code.
fn back_to_profile(flash: &str, code: &str) -> Response {
    Redirect::to(&format!(
        "{}?{flash}={code}#profile-signature",
        webapp::profile::PROFILE_PATH,
    ))
    .into_response()
}

/// Save the signed-in session's mail signature. The session's Person is the
/// actor and the only subject this seam can ever name; the store repeats the
/// self-edit gate on the write itself. Validation runs before any connection,
/// so a refused body never reaches for a store.
async fn save_for_session(
    surreal: &store::surreal::SurrealDb,
    pg: Option<&store::postgres::PostgresDb>,
    session: &SessionData,
    body: &str,
) -> Result<store::mail_signatures::MailSignature, SaveRefusal> {
    let Some(person_id) = session.person_id else {
        return Err(SaveRefusal::NoPerson);
    };
    let fields = store::mail_signatures::validate(&store::mail_signatures::MailSignatureFields {
        body: body.to_string(),
    })
    .map_err(|error| match error {
        store::mail_signatures::MailSignatureError::Invalid(_) => SaveRefusal::Invalid,
        other => {
            tracing::error!(error = %other, "mail signature validate returned an unexpected error");
            SaveRefusal::Failed
        }
    })?;
    // The handle is only needed for the write; a deployment whose PostgreSQL
    // is unreachable cannot store a signature at all.
    let Some(pg) = pg else {
        return Err(SaveRefusal::Unavailable);
    };
    store::mail_signatures::save_for_actor(surreal, pg, person_id, person_id, &fields)
        .await
        .map_err(|error| match error {
            store::mail_signatures::MailSignatureError::NotFound => SaveRefusal::NotFound,
            store::mail_signatures::MailSignatureError::Invalid(_) => SaveRefusal::Invalid,
            other => {
                tracing::error!(error = %other, "mail signature save failed");
                SaveRefusal::Failed
            }
        })
}

/// The profile page's signature form: one `body` field, nothing else — in
/// particular no subject field, so nothing on the wire can name a Person to
/// sign as. (The urlencoded extractor ignores undeclared keys rather than
/// rejecting them, so safety here is structural: there is no field to
/// deserialize a subject into. The session decides the subject, always.)
#[derive(Debug, Deserialize)]
pub(crate) struct MailSignatureForm {
    #[serde(default)]
    body: String,
}

/// `POST /app/profile/mail-signature` — the profile page's signature form.
/// The signed session's Person is the subject and the only accepted subject;
/// the redirect names a fixed flash code only.
async fn profile_mail_signature(
    State(state): State<AdminState>,
    session: Option<Extension<SessionData>>,
    axum::Form(form): axum::Form<MailSignatureForm>,
) -> Response {
    let Some(Extension(session)) = session else {
        return StatusCode::FORBIDDEN.into_response();
    };
    // The production PG handle comes from the deployment's environment, the
    // established portal pattern (see `post_brand_letterhead`); the None
    // mapping is the same refusal the unreachable-store path returns.
    let pg = store::postgres::PostgresDb::from_env().await.ok();
    match save_for_session(&state.surreal, pg.as_ref(), &session, &form.body).await {
        Ok(_) => back_to_profile("signature", "saved"),
        Err(refusal) => {
            if matches!(refusal, SaveRefusal::Failed) {
                return refusal.status().into_response();
            }
            back_to_profile("signature_error", refusal.flash_code())
        }
    }
}

/// The profile page's native-form route, merged into `admin::routes`.
/// `/app/profile/**` is admitted for every authenticated tier by the route
/// policy; the handler decides the rest.
pub fn routes() -> axum::Router<AdminState> {
    axum::Router::new().route(
        PROFILE_MAIL_SIGNATURE_PATH,
        axum::routing::post(profile_mail_signature),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::persons::{NewPerson, Role};
    use store::postgres::PostgresDb;
    use store::test_support::mem_surreal;
    use uuid::Uuid;

    fn session(person_id: Option<Uuid>) -> SessionData {
        SessionData {
            sub: "synthetic-signature-holder".into(),
            email: None,
            person_id,
            exp: crate::session::now_unix_secs() + 60,
            role: Role::Lawyer,
            csrf_token: "synthetic-csrf".into(),
            source: crate::session::SessionSource::Cli,
            provider: None,
            viewing_as_dri: None,
            scope: None,
        }
    }

    fn body(text: &str) -> String {
        text.to_string()
    }

    #[test]
    fn refusals_map_to_stable_statuses_and_flash_codes() {
        assert_eq!(SaveRefusal::NoPerson.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            SaveRefusal::Invalid.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(SaveRefusal::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            SaveRefusal::Unavailable.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            SaveRefusal::Failed.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(SaveRefusal::NoPerson.flash_code(), "not_found");
        assert_eq!(SaveRefusal::Invalid.flash_code(), "invalid");
        assert_eq!(SaveRefusal::NotFound.flash_code(), "not_found");
        assert_eq!(SaveRefusal::Unavailable.flash_code(), "unavailable");
        assert_eq!(SaveRefusal::Failed.flash_code(), "failed");
    }

    #[test]
    fn the_redirect_carries_only_a_fixed_flash_code() {
        let response = back_to_profile("signature", "saved");
        let location = response.headers()[axum::http::header::LOCATION]
            .to_str()
            .unwrap();
        assert!(
            location.ends_with("?signature=saved#profile-signature"),
            "{location}"
        );
        let response = back_to_profile("signature_error", "invalid");
        let location = response.headers()[axum::http::header::LOCATION]
            .to_str()
            .unwrap();
        assert!(
            location.ends_with("?signature_error=invalid#profile-signature"),
            "{location}"
        );
    }

    #[tokio::test]
    async fn a_posted_subject_id_is_ignored_not_honored() {
        async fn parse(body: &str) -> MailSignatureForm {
            use axum::extract::FromRequest;
            let request = axum::http::Request::builder()
                .method(axum::http::Method::POST)
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(axum::body::Body::from(body.to_string()))
                .unwrap();
            axum::Form::<MailSignatureForm>::from_request(request, &())
                .await
                .map(|axum::Form(form)| form)
                .expect("the form parses")
        }
        // The extractor ignores keys the struct does not declare — the
        // struct has no subject field at all, so nothing on the wire can
        // name a Person. Which subject is written is decided by the
        // session, and only the session; the store-write test pins that.
        let plain = parse("body=Example Attorney").await;
        assert_eq!(plain.body, "Example Attorney");
        let forged =
            parse("body=Example Attorney&person_id=00000000-0000-0000-0000-000000000001").await;
        assert_eq!(forged.body, "Example Attorney");
        assert_eq!(
            plain.body, forged.body,
            "a posted subject id changes nothing"
        );
    }

    #[tokio::test]
    async fn a_session_without_a_linked_person_is_refused_before_any_store() {
        let surreal = mem_surreal().await;
        let refused = save_for_session(&surreal, None, &session(None), &body("Example Attorney"))
            .await
            .unwrap_err();
        assert!(matches!(refused, SaveRefusal::NoPerson), "{refused:?}");
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_invalid_body_is_refused_before_any_connection() {
        let surreal = mem_surreal().await;
        let session = session(Some(Uuid::now_v7()));
        for (text, why) in [
            ("   ", "blank"),
            ("Example Attorney\n--\nConfidential.", "delimiter"),
            (&"x".repeat(2001), "too long"),
        ] {
            let refused = save_for_session(&surreal, None, &session, &body(text))
                .await
                .unwrap_err();
            assert!(
                matches!(refused, SaveRefusal::Invalid),
                "{why}: {refused:?}"
            );
            assert_eq!(refused.flash_code(), "invalid", "{why}");
        }
    }

    /// The round trip against a real PostgreSQL, where the lane exists: the
    /// session's Person is the subject written, the store's self-edit gate
    /// still refuses an unknown one, and one Person's row never answers for
    /// another.
    #[tokio::test]
    async fn the_session_person_is_the_subject_the_store_writes() {
        let Some(pg) = pg_lane().await else {
            return;
        };
        let surreal = mem_surreal().await;
        let attorney = store::persons::create(
            &surreal,
            &NewPerson {
                name: "Synthetic attorney".to_string(),
                email: "signature-attorney@example.com".to_string(),
                ..NewPerson::default()
            },
        )
        .await
        .unwrap()
        .id;
        let colleague = store::persons::create(
            &surreal,
            &NewPerson {
                name: "Synthetic colleague".to_string(),
                email: "signature-colleague@example.com".to_string(),
                ..NewPerson::default()
            },
        )
        .await
        .unwrap()
        .id;

        let saved = save_for_session(
            &surreal,
            Some(&pg),
            &session(Some(attorney)),
            &body("Example Attorney\nConfidential: intended recipient only."),
        )
        .await
        .unwrap();
        assert_eq!(saved.person_id, attorney);
        let stored = store::mail_signatures::for_person(&surreal, &pg, attorney)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.body, saved.body);
        assert_eq!(
            store::mail_signatures::find(&pg, colleague).await.unwrap(),
            None,
            "one Person's row is keyed to that Person"
        );

        // A session whose Person the directory does not know: the store's
        // non-disclosing refusal, and no row written.
        let stranger = session(Some(Uuid::now_v7()));
        let refused = save_for_session(&surreal, Some(&pg), &stranger, &body("Example Attorney"))
            .await
            .unwrap_err();
        assert!(matches!(refused, SaveRefusal::NotFound), "{refused:?}");
        assert_eq!(
            store::mail_signatures::find(&pg, stranger.person_id.unwrap())
                .await
                .unwrap(),
            None,
            "a refused save writes nothing"
        );
    }

    /// Arm the PostgreSQL test lane the way the store's own integration tests
    /// do: an isolated database per test, `None` (skip) where no lane runs.
    async fn pg_lane() -> Option<PostgresDb> {
        let port = if let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") {
            port.parse::<u16>().unwrap()
        } else {
            assert!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
                "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
            );
            return None;
        };
        let options = sqlx::postgres::PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("synthetic")
            .database(
                &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                    .unwrap_or_else(|_| "smoke".into()),
            )
            .ssl_mode(sqlx::postgres::PgSslMode::Disable);
        let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
        let database = format!("mail_signature_portal_{}", Uuid::now_v7().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
            .execute(&admin)
            .await
            .unwrap();
        let pg = PostgresDb::connect(options.database(&database))
            .await
            .unwrap();
        pg.migrate_embedded(None).await.unwrap();
        Some(pg)
    }
}
