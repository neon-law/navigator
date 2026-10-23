#![allow(clippy::unused_async)]

use std::sync::Arc;

use cucumber::{given, then, when, World};
use features::{app_state, fs_storage};
use portal::{policy::PolicyClient, SessionStore};
use workflows::InMemoryRuntime;

#[derive(Default, World)]
#[world(init = Self::default)]
struct TrustAcceptanceWorld {
    app: Option<axum::Router>,
    sessions: Option<SessionStore>,
    last_status: Option<u16>,
    last_body: String,
}

impl std::fmt::Debug for TrustAcceptanceWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustAcceptanceWorld")
            .field("last_status", &self.last_status)
            .finish_non_exhaustive()
    }
}

impl TrustAcceptanceWorld {
    fn app(&self) -> axum::Router {
        self.app.as_ref().expect("app is built").clone()
    }

    fn sessions(&self) -> &SessionStore {
        self.sessions.as_ref().expect("sessions are built")
    }
}

#[given("the Neon Law Navigator app is running")]
async fn build_app(world: &mut TrustAcceptanceWorld) {
    let sessions = SessionStore::new("test-trust-acceptance-session-key");
    let state = app_state(
        Arc::new(InMemoryRuntime::new()),
        fs_storage("portfolio-acceptance").await,
        PolicyClient::passthrough(),
        None,
        sessions.clone(),
    )
    .await;
    world.sessions = Some(sessions);
    world.app = Some(features::neon_router(
        state,
        std::path::Path::new(portal::DEFAULT_PUBLIC_DIR),
    ));
}

#[given("the versioned synthetic portfolio is applied twice")]
async fn apply_portfolio_twice(_world: &mut TrustAcceptanceWorld) {
    let counts = features::portfolio_acceptance::prepare_test_fixture().await;
    assert_eq!(counts.second_created, 0);
    assert!(counts.second_unchanged > 0);
}

#[when(regex = r#"^shared acceptance scenario "([^"]+)" is requested$"#)]
async fn request_shared_scenario(world: &mut TrustAcceptanceWorld, id: String) {
    let result = features::portfolio_acceptance::request_api(&world.app(), world.sessions(), &id)
        .await
        .expect("shared portfolio trust scenario executes");
    world.last_status = Some(result.status);
    world.last_body = result.body;
}

#[then(regex = r#"^shared acceptance scenario "([^"]+)" passes$"#)]
async fn shared_scenario_passes(world: &mut TrustAcceptanceWorld, id: String) {
    let status = world.last_status.expect("shared API response is captured");
    features::portfolio_acceptance::assert_result(&id, status, &world.last_body)
        .unwrap_or_else(|error| panic!("{error}"));
}

#[tokio::main]
async fn main() {
    TrustAcceptanceWorld::cucumber()
        .run_and_exit("tests/features/portal_trust_position_acceptance.feature")
        .await;
}
