use axum::{http::StatusCode, response::IntoResponse, routing::get, Extension, Json, Router};
use ccc_core::{AppContext, PluginLifecycle, PluginManager};
use serde::Serialize;
use std::future::Future;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Health check endpoint.
async fn health_check() -> &'static str {
    "OK - Nuntius (CCC) API is running."
}

/// API version (the `ccc-api` crate version).
async fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Build the *base* router — only the always-on host endpoints.
///
/// This is what the host always exposes, even if no plugins are enabled.
pub fn base_router() -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/version", get(version))
}

/// Aggregate plugin health for load balancers and monitoring.
///
/// Returns 200 OK when every loaded plugin reports `Configured` or
/// `Live`; 503 Service Unavailable when any plugin is in the
/// `Unconfigured` state. The body is the same in both cases — the
/// status code carries the binary signal.
async fn health_plugins(
    axum::extract::State(manager): axum::extract::State<Arc<PluginManager>>,
    Extension(ctx): Extension<AppContext>,
) -> impl IntoResponse {
    let rows = manager.rail_rows(&ctx);
    let unconfigured: Vec<&str> = rows
        .iter()
        .filter(|r| matches!(r.state, PluginLifecycle::Unconfigured))
        .map(|r| r.id.as_str())
        .collect();
    let ready = unconfigured.is_empty();
    let body = HealthPlugins {
        ready,
        unconfigured: unconfigured.into_iter().map(String::from).collect(),
        total: rows.len(),
    };
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body))
}

#[derive(Serialize)]
struct HealthPlugins {
    ready: bool,
    unconfigured: Vec<String>,
    total: usize,
}

/// Build the full application router: base routes + every enabled plugin's routes.
///
/// Plugins that did not opt into HTTP (default `routes()` returning `None`)
/// contribute nothing — they pay no cost in the route tree.
pub fn app_router(plugins: Arc<PluginManager>, ctx: &AppContext) -> Router {
    // The health-plugins route needs the PluginManager, so we build it
    // separately with the right state type and merge it in.
    let health_route: Router = axum::Router::new()
        .route("/health/plugins", get(health_plugins))
        .with_state(plugins.clone())
        .layer(axum::Extension(ctx.clone()));
    base_router()
        .merge(health_route)
        .merge(plugins.merged_router(ctx))
}

/// Run the server with a fully built `Router`.
///
/// The caller is responsible for assembling the router (typically via
/// `app_router(manager_arc, &ctx)`); `ccc-api` does not own plugin lifecycle.
///
/// `shutdown` is a future that resolves when the server should stop
/// accepting new connections and finish in-flight requests. The common
/// pattern is `async { tokio::signal::ctrl_c().await.unwrap_or(()) }`.
///
/// Pass `std::future::pending()` to never shut down from this path
/// (useful in tests).
pub async fn serve(
    addr: &str,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), std::io::Error> {
    let listener = TcpListener::bind(addr).await?;
    println!("Server running on {}", addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}
