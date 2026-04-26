use axum::{routing::get, Router};
use ccc_core::{AppContext, PluginManager};
use tokio::net::TcpListener;

/// Health check endpoint.
async fn health_check() -> &'static str {
    "OK - Creator Command Center (CCC) API is running."
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

/// Build the full application router: base routes + every enabled plugin's routes.
///
/// Plugins that did not opt into HTTP (default `routes()` returning `None`)
/// contribute nothing — they pay no cost in the route tree.
pub fn app_router(plugins: &PluginManager, ctx: &AppContext) -> Router {
    base_router().merge(plugins.merged_router(ctx))
}

/// Run the server with a fully built `Router`.
///
/// The caller is responsible for assembling the router (typically via
/// `app_router(&manager, &ctx)`); `ccc-api` does not own plugin lifecycle.
pub async fn serve(addr: &str, app: Router) -> Result<(), std::io::Error> {
    let listener = TcpListener::bind(addr).await?;
    println!("Server running on {}", addr);
    axum::serve(listener, app).await
}
