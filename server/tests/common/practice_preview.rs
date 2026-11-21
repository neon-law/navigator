//! Real HTTP previews of prepared brands, independent of deployment launch gates.

use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use views::brand::BrandKey;

pub struct PracticePreview {
    pub sites: Vec<(BrandKey, String)>,
    tasks: Vec<JoinHandle<()>>,
}

impl PracticePreview {
    pub async fn start() -> Self {
        Self::for_brands(&[BrandKey::Aegis, BrandKey::Groundwork, BrandKey::Vesta]).await
    }

    pub async fn for_brands(brands: &[BrandKey]) -> Self {
        // PostgreSQL carries every record the preview reads; the Surreal handle
        // is an unconnected placeholder, as in the other browser fixtures.
        store::postgres::PostgresDb::application()
            .await
            .expect("require PostgreSQL browser fixture history");
        let mut state =
            portal::test_support::app_state(store::surreal::SurrealDb::uninitialized()).await;
        store::seed::seed_brand_blog(&state.surreal, store::seed::BrandSeed::Neon)
            .await
            .expect("seed public Brand Posts for previews");
        let mut listeners = Vec::new();
        let mut ports = std::collections::BTreeMap::new();
        let mut sites = Vec::new();
        for &key in brands {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("preview listener");
            let port = listener.local_addr().expect("preview address").port();
            ports.insert(port, key);
            sites.push((key, format!("http://localhost:{port}")));
            listeners.push(listener);
        }
        state.canonical_host = portal::CanonicalHost::new(None).with_local_ports(ports);
        let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
        let tasks = listeners
            .into_iter()
            .map(|listener| {
                let app = app.clone();
                tokio::spawn(async move {
                    axum::serve(listener, app)
                        .await
                        .expect("serve practice preview");
                })
            })
            .collect();
        Self { sites, tasks }
    }
}

impl Drop for PracticePreview {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
