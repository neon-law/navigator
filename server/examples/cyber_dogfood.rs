//! THROWAWAY dogfood server for local page inspection. Never committed.
//! Serves the composed neon router on 127.0.0.1:3099 with an in-memory store.

use std::path::Path;

#[tokio::main]
async fn main() {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    let app = server::neon_router(state, Path::new(portal::DEFAULT_PUBLIC_DIR));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3099")
        .await
        .expect("bind 3099");
    eprintln!("dogfood server on http://127.0.0.1:3099 (Host: www.cyberinjurylaw.com)");
    axum::serve(listener, app).await.expect("serve");
}
