//! App initialization shared by the project-notation HTTP contract tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use portal::{AppState, SessionStore};
use store::seed;
use store::test_support::mem_surreal;
use workflows::{InMemoryRuntime, StateMachineRuntime};

pub(super) async fn build_app(
    session_key: &str,
    storage_path: PathBuf,
) -> (axum::Router, store::surreal::SurrealDb) {
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> =
        Arc::new(cloud::FsStorage::new(storage_path).await.unwrap());
    seed::seed_canonical(&surreal, &storage).await.unwrap();
    let runtime: Arc<dyn StateMachineRuntime> = Arc::new(InMemoryRuntime::new());
    let state = AppState {
        sessions: SessionStore::new(session_key),
        storage: storage.clone(),
        workflow_runtime: runtime.clone(),
        questionnaire_runtime: runtime,
        ..portal::test_support::app_state(surreal.clone()).await
    };
    (
        server::neon_router(state, Path::new(portal::DEFAULT_PUBLIC_DIR)),
        surreal,
    )
}
