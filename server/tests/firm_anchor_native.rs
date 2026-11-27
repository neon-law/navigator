//! Native HTTP Entity anchor create/delete/rename race invariants.
use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use tower::ServiceExt;
use uuid::Uuid;

fn redirect_location(response: &axum::response::Response) -> String {
    response
        .headers()
        .get(axum::http::header::LOCATION)
        .expect("a redirect carries a Location")
        .to_str()
        .unwrap()
        .to_string()
}

fn entity_write_outcome(response: &axum::response::Response) -> EntityWriteOutcome {
    if response.status() != StatusCode::SEE_OTHER {
        return EntityWriteOutcome::NotRedirected(response.status());
    }
    let location = redirect_location(response);
    if location == "/app/admin/entities" {
        return EntityWriteOutcome::Created;
    }
    let flash = location
        .split_once("?error=")
        .map_or_else(String::new, |(_, query)| {
            query.split('&').next().unwrap_or_default().to_string()
        });
    EntityWriteOutcome::Refused(flash)
}

fn flash_of(message: &str) -> String {
    message.replace(' ', "%20")
}

fn entity_write_succeeded(response: &axum::response::Response) -> bool {
    entity_write_outcome(response) == EntityWriteOutcome::Created
}

fn admin_session_cookie_and_csrf() -> (String, String) {
    let session = portal::SessionData::fresh("admin@neonlaw.com", store::persons::Role::Admin);
    let csrf = session.csrf_token.clone();
    let cookie = format!(
        "{}={}",
        portal::session::SESSION_COOKIE_NAME,
        portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY).encode(&session)
    );
    (cookie, csrf)
}

#[derive(Debug, PartialEq, Eq)]
enum EntityWriteOutcome {
    Created,
    Refused(String),
    /// Anything that is not a redirect at all — a lost delete/rename race
    /// answers `404`, and that is a third outcome, not a refusal.
    NotRedirected(StatusCode),
}

async fn fixture() -> (
    portal::AppState,
    store::application_store::ApplicationStore,
    store::entities::Entity,
) {
    let db = store::application_store::ApplicationStore::uninitialized();
    let suffix = Uuid::now_v7();
    let kind = store::entity_types::create(&db, &format!("Synthetic kind {suffix}"))
        .await
        .unwrap();
    let jurisdiction = store::jurisdictions::create(
        &db,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic jurisdiction {suffix}"),
            suffix.to_string(),
            "state",
        ),
    )
    .await
    .unwrap();
    let firm = store::entities::create(
        &db,
        &store::entities::NewEntity {
            name: store::seed::FIRM_ENTITY_NAME.into(),
            entity_type_id: kind.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: Some(store::seed::FIRM_ENTITY_NAME.to_lowercase()),
        },
    )
    .await
    .unwrap();
    (portal::test_support::app_state(db.clone()).await, db, firm)
}
async fn move_anchor_aside(
    _db: &store::application_store::ApplicationStore,
    firm: &store::entities::Entity,
    aside_name: &str,
) {
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    assert_eq!(
        sqlx::query("UPDATE entity SET name=$2, firm_anchor_key=NULL WHERE id=$1")
            .bind(firm.id)
            .bind(aside_name)
            .execute(pg.pool())
            .await
            .unwrap()
            .rows_affected(),
        1
    );
}
async fn native_router(state: portal::AppState) -> axum::Router {
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)).layer(
        axum::middleware::from_fn(
            move |request: Request<Body>, next: axum::middleware::Next| {
                let pg = pg.clone();
                async move {
                    store::postgres::PostgresDb::with_application_db(pg, next.run(request))
                        .await
                        .expect("each racer requires native admission")
                }
            },
        ),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_creates_cannot_fork_the_firm_anchor() {
    store::test_support::with_postgres(async {
    let (state, application_store, firm) = fixture().await;
    let (type_id, jur_id) = (firm.entity_type_id, firm.jurisdiction_id);

    // Stand in for the white-label window: the protected name is configured
    // but no row carries it yet, so the existence check can pass. Moved aside
    // directly, since the surface itself refuses to.
    // Clearing `firm_anchor_key` is what opens the window: the UNIQUE
    // `entity_firm_anchor` index — not the name — is what a create now
    // collides with, so a rename that left the key would make every racer
    // below fail rather than exactly one succeed.
    move_anchor_aside(&application_store, &firm, "Placeholder Holdings LLC").await;

    let app = native_router(state).await;
    let (cookie, csrf) = admin_session_cookie_and_csrf();

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let app = app.clone();
        let (cookie, csrf) = (cookie.clone(), csrf.clone());
        tasks.spawn(async move {
            let response = app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/app/admin/entities")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "name=Guieb%20%26%20Shook%20PC&entity_type_id={type_id}&jurisdiction_id={jur_id}&_csrf={csrf}"
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
            entity_write_outcome(&response)
        });
    }
    let mut created = 0;
    while let Some(outcome) = tasks.join_next().await {
        match outcome.unwrap() {
            EntityWriteOutcome::Created => created += 1,
            // Not merely "did not create" — refused *for the anchor*. A
            // racer that failed for any other reason is a broken guard
            // wearing a refusal, and must not be counted as one.
            EntityWriteOutcome::Refused(flash) => assert_eq!(
                flash,
                flash_of(store::entity_commands::FIRM_ANCHOR_EXISTS_MESSAGE),
                "a losing racer must be refused for the anchor, not for a fault",
            ),
            other @ EntityWriteOutcome::NotRedirected(_) => {
                panic!("a racer must redirect, got {other:?}")
            }
        }
    }
    assert_eq!(created, 1, "exactly one racer may create the anchor");

    let rows = store::entities::all(&application_store)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| {
            r.name
                .trim()
                .eq_ignore_ascii_case(store::seed::FIRM_ENTITY_NAME)
        })
        .count();
    assert_eq!(
        rows, 1,
        "concurrent creates must not fork the anchor into rows nothing can delete",
    );

    }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_delete_racing_a_rename_into_the_firm_name_never_removes_the_anchor() {
    store::test_support::with_postgres(async {
    // The dangerous interleaving — delete reads an ordinary row, a rename turns
    // it into the anchor, then the id-only delete removes the freshly protected
    // row — is timing-dependent and rarely wins against a fast local store.
    // The invariant this asserts holds regardless of who wins the race: a
    // rename that reports minting the anchor is never undone by the racing
    // delete. The owned fixture exercises the competing operations directly.
        let (state, application_store, firm) = fixture().await;
        let (type_id, jur_id) = (firm.entity_type_id, firm.jurisdiction_id);

        // Open the white-label window so a rename can mint the anchor: move the
        // seeded row aside directly, which the surface itself refuses to do.
        move_anchor_aside(
            &application_store,
            &firm,
            "Placeholder Holdings LLC",
        )
        .await;

        let victim = store::entities::create(
            &application_store,
            &store::entities::NewEntity {
                name: "Acme LLC".to_string(),
                entity_type_id: type_id,
                jurisdiction_id: jur_id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap();

        let app = native_router(state).await;
        let (cookie, csrf) = admin_session_cookie_and_csrf();
        let vid = victim.id;

        let delete = {
            let (app, cookie, csrf) = (app.clone(), cookie.clone(), csrf.clone());
            tokio::spawn(async move {
                app.oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/app/admin/entities/{vid}/delete"))
                        .header(header::COOKIE, &cookie)
                        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .body(Body::from(format!("_csrf={csrf}")))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            })
        };
        let rename = {
            let (app, cookie, csrf) = (app.clone(), cookie.clone(), csrf.clone());
            tokio::spawn(async move {
                let response = app.oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/app/admin/entities/{vid}"))
                        .header(header::COOKIE, &cookie)
                        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .body(Body::from(format!(
                            "name=Guieb%20%26%20Shook%20PC&entity_type_id={type_id}&jurisdiction_id={jur_id}&_csrf={csrf}"
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
                // Under post/redirect/get a refused rename is also a `303`, so
                // the redirect target — not the status — reports the outcome.
                entity_write_succeeded(&response)
            })
        };
        let (delete_status, renamed) = (delete.await.unwrap(), rename.await.unwrap());

        let anchor_rows: Vec<_> = store::entities::all(&application_store)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| row.name == store::seed::FIRM_ENTITY_NAME)
            .collect();
        if renamed {
            assert_eq!(
                anchor_rows.len(),
                1,
                "a rename that minted the anchor \
                 (delete={delete_status}) must not be undone by the racing delete",
            );
        } else {
            // The rename lost or errored, so no anchor should exist and the
            // victim was a plain row the delete could take.
            assert!(
                anchor_rows.is_empty(),
                "no anchor should exist when the rename did not succeed",
            );
        }
    }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_rename_racing_a_rename_into_the_firm_name_never_loses_the_anchor() {
    store::test_support::with_postgres(async {
    // Request A renames an ordinary row into the firm name (minting the anchor
    // in the white-label window); request B renames the same row to another
    // ordinary name off a read taken before A committed. If B's read were
    // outside the anchor lock it would see an ordinary source, skip every
    // guard, and rename the freshly protected firm away. The invariant holds
    // no matter who wins: a rename that reports minting the anchor is never
    // undone, and the firm name is never split into duplicates.
        let (state, application_store, firm) = fixture().await;
        let (type_id, jur_id) = (firm.entity_type_id, firm.jurisdiction_id);

        move_anchor_aside(
            &application_store,
            &firm,
            "Placeholder Holdings LLC",
        )
        .await;

        let victim = store::entities::create(
            &application_store,
            &store::entities::NewEntity {
                name: "Acme LLC".to_string(),
                entity_type_id: type_id,
                jurisdiction_id: jur_id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap();

        let app = native_router(state).await;
        let (cookie, csrf) = admin_session_cookie_and_csrf();
        let vid = victim.id;

        let into_firm = {
            let (app, cookie, csrf) = (app.clone(), cookie.clone(), csrf.clone());
            tokio::spawn(async move {
                let response = app.oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/app/admin/entities/{vid}"))
                        .header(header::COOKIE, &cookie)
                        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .body(Body::from(format!(
                            "name=Guieb%20%26%20Shook%20PC&entity_type_id={type_id}&jurisdiction_id={jur_id}&_csrf={csrf}"
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
                // Under post/redirect/get a refused rename is also a `303`, so
                // the redirect target — not the status — reports the outcome.
                entity_write_succeeded(&response)
            })
        };
        let to_ordinary = {
            let (app, cookie, csrf) = (app.clone(), cookie.clone(), csrf.clone());
            tokio::spawn(async move {
                app.oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/app/admin/entities/{vid}"))
                        .header(header::COOKIE, &cookie)
                        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .body(Body::from(format!(
                            "name=Acme%20Corp&entity_type_id={type_id}&jurisdiction_id={jur_id}&_csrf={csrf}"
                        )))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            })
        };
        let (minted_the_anchor, _to_ordinary_status) =
            (into_firm.await.unwrap(), to_ordinary.await.unwrap());

        let anchor_rows: Vec<_> = store::entities::all(&application_store)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| row.name == store::seed::FIRM_ENTITY_NAME)
            .collect();
        assert!(
            anchor_rows.len() <= 1,
            "the firm name must never split into duplicates",
        );
        if minted_the_anchor {
            assert_eq!(
                anchor_rows.len(),
                1,
                "a rename that minted the anchor must not be \
                 renamed away by the racing rename",
            );
        }
    }).await;
}
