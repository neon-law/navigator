//! Real HTTP previews of prepared brands, independent of deployment launch gates.

use std::{future::Future, pin::Pin};

use futures::FutureExt as _;
use store::postgres::PostgresDb;
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};
use views::brand::BrandKey;

pub struct PracticePreview {
    pub sites: Vec<(BrandKey, String)>,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    _storage: tempfile::TempDir,
}

impl PracticePreview {
    /// Keep the fresh canonical database alive until every HTTP task has drained,
    /// including when a browser assertion panics. Never reuse the tier's database.
    pub async fn run(
        test: impl for<'a> FnOnce(&'a Self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    ) {
        Self::run_for_brands(
            &[BrandKey::Aegis, BrandKey::Groundwork, BrandKey::Vesta],
            test,
        )
        .await;
    }

    pub async fn run_for_brands(
        brands: &[BrandKey],
        test: impl for<'a> FnOnce(&'a Self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    ) {
        store::test_support::with_postgres(async {
            let mut preview = Self::start(brands).await;
            let result = std::panic::AssertUnwindSafe(test(&preview))
                .catch_unwind()
                .await;
            let closed = preview.close().await;
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
            closed.expect("drain preview HTTP tasks");
        })
        .await;
    }

    async fn start(brands: &[BrandKey]) -> Self {
        let pg = PostgresDb::application()
            .await
            .expect("admitted preview fixture");
        let mut state =
            portal::test_support::app_state(store::surreal::SurrealDb::uninitialized()).await;
        // The public signup form mounts with the OAuth routes. Rendering it uses no provider.
        state.oauth = Some(portal::OAuthConfig::new(
            "preview",
            "synthetic",
            "https://primary.example/auth/callback",
            "https://idp.example/authorize",
            "https://idp.example/token",
        ));
        let storage = tempfile::tempdir().expect("owned preview storage");
        let blobs = std::sync::Arc::new(
            cloud::FsStorage::new(storage.path())
                .await
                .expect("preview storage"),
        );
        state.storage = blobs.clone();
        state.assets_storage = blobs;
        store::seed::seed_canonical(&state.surreal, &state.assets_storage)
            .await
            .expect("seed canonical Firms and Brands before their public Posts");
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
        let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR))
            .layer(axum::middleware::from_fn(
                move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                    let pg = pg.clone();
                    async move {
                        request.extensions_mut().insert(pg.clone());
                        PostgresDb::with_application_db(pg, next.run(request))
                            .await
                            .unwrap_or_else(|_| {
                                axum::response::IntoResponse::into_response(
                                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                                )
                            })
                    }
                },
            ));
        let (stop, stopped) = watch::channel(false);
        let tasks = listeners
            .into_iter()
            .map(|listener| {
                let app = app.clone();
                let mut stopped = stopped.clone();
                tokio::spawn(async move {
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async move {
                            let _ = stopped.changed().await;
                        })
                        .await
                        .expect("serve practice preview");
                })
            })
            .collect();
        Self {
            sites,
            stop,
            tasks,
            _storage: storage,
        }
    }

    async fn close(&mut self) -> Result<(), tokio::task::JoinError> {
        self.stop.send_replace(true);
        let mut failure = None;
        for task in &mut self.tasks {
            if let Err(error) = task.await {
                failure = Some(error);
            }
        }
        self.tasks.clear();
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for PracticePreview {
    fn drop(&mut self) {
        // Fallback for cancellation during setup/close. Normal and assertion-panic
        // paths await graceful shutdown before the surrounding fixture is closed.
        self.stop.send_replace(true);
        for task in &self.tasks {
            task.abort();
        }
    }
}
