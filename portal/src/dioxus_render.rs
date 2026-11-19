//! Shared Dioxus execution and SSR renderers with per-route configuration.

use dioxus_server::{FullstackState, ServeConfig};

#[derive(Clone)]
struct RouteRoot(std::sync::Arc<dyn Fn() -> dioxus_core::Element + Send + Sync>);

fn route_root() -> dioxus_core::Element {
    let root = dioxus_core::consume_context::<RouteRoot>();
    (root.0)()
}

// Every page retains its own configuration and root context; only the
// execution pool and stateless SSR renderers are shared across page routes.
// These page configurations do not enable incremental rendering: its cache
// belongs to the pool and cannot vary through FullstackState::with_config.
pub(crate) fn state<M: 'static>(
    config: ServeConfig,
    root: impl dioxus_core::ComponentFunction<(), M> + Send + Sync + 'static,
) -> FullstackState {
    static STATE: std::sync::OnceLock<FullstackState> = std::sync::OnceLock::new();
    let root = RouteRoot(std::sync::Arc::new(move || root.rebuild(())));
    STATE
        .get_or_init(|| FullstackState::new(ServeConfig::new(), route_root))
        .clone()
        .with_config(config.context_provider(move || root.clone()))
}

#[cfg(test)]
mod shared_pool_tests {
    use super::*;
    use axum::{body::Body, extract::Request, http::StatusCode, routing::get, Router};
    use dioxus_core::{DynamicNode, Template, TemplateNode, VNode, VText};
    use dioxus_server::render_handler;
    use tower::ServiceExt;

    #[derive(Clone)]
    struct RouteLabel(&'static str);

    fn text(prefix: &str) -> VNode {
        let label = dioxus_core::consume_context::<RouteLabel>();
        VNode::new(
            None,
            Template {
                roots: &[TemplateNode::Dynamic { id: 0 }],
                node_paths: &[&[0]],
                attr_paths: &[],
            },
            Box::new([DynamicNode::Text(VText::new(format!(
                "{prefix}:{}",
                label.0
            )))]),
            Box::new([]),
        )
    }

    async fn page(app: Router) -> String {
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        String::from_utf8(
            axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn shared_render_pool_keeps_concurrent_roots_and_route_contexts_isolated() {
        let first = Router::<FullstackState>::new()
            .route("/", get(render_handler))
            .with_state(state(
                ServeConfig::new().context_provider(|| RouteLabel("alpha")),
                || Ok(text("first")),
            ));
        let second = Router::<FullstackState>::new()
            .route("/", get(render_handler))
            .with_state(state(
                ServeConfig::new().context_provider(|| RouteLabel("beta")),
                || Ok(text("second")),
            ));
        for _ in 0..8 {
            let (left, right) = tokio::join!(page(first.clone()), page(second.clone()));
            assert!(left.contains("first:alpha"), "{left}");
            assert!(
                !left.contains("beta") && !left.contains("second:"),
                "{left}"
            );
            assert!(right.contains("second:beta"), "{right}");
            assert!(
                !right.contains("alpha") && !right.contains("first:"),
                "{right}"
            );
        }
    }
}
