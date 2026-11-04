//! Untracked dogfood harness for the Vesta `/world` page (not part of the
//! shipped crate surface: used to drive a real browser against the shipped
//! router, then deleted before the branch is committed).
//!
//! Mirrors `server/tests/world_page.rs`'s setup — memory store, the Vesta
//! local preview port, the public router — but binds a real TCP listener so
//! `agent-browser` can reach it:
//!
//! ```bash
//! cargo run -p server --example world_dogfood   # listens on 127.0.0.1:20641
//! WORLD_PORT=20642 cargo run -p server --example world_dogfood  # other port
//! ```
//!
//! Local preview admits registered brands independently of the public launch
//! gate, so `http://127.0.0.1:20641/world` resolves to Vesta's `/world`.

use std::path::Path;

use portal::{AppState, CanonicalHost};
use views::brand::BrandKey;

/// The same local preview port `vesta_home.rs` and the `/world` routing test
/// stand in for.
const PREVIEW_PORT: u16 = 20_641;

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("WORLD_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(PREVIEW_PORT);
    let state = AppState {
        canonical_host: CanonicalHost::new(None)
            .with_local_ports([(port, BrandKey::Vesta)].into_iter().collect()),
        ..portal::test_support::app_state(store::test_support::mem_surreal().await).await
    };
    let app = server::neon_router(state, Path::new(portal::DEFAULT_PUBLIC_DIR));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind the dogfood listener");
    println!("listening on http://127.0.0.1:{port}");
    axum::serve(listener, app)
        .await
        .expect("serve the dogfood app");
}
