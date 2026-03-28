use axum::{
    routing::get,
    Router,
};
use tokio::net::TcpListener;

/// A simple handler for the health check endpoint.
async fn health_check() -> &'static str {
    "OK - Creator Command Center (CCC) API is running."
}

/// A simple handler for the version endpoint.
async fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Create and configure the main API router.
pub fn create_router() -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/version", get(version))
}

/// Run the server, binding to the specified address.
///
/// Example address: `"0.0.0.0:3000"`
pub async fn serve(addr: &str) -> Result<(), std::io::Error> {
    let app = create_router();

    let listener = TcpListener::bind(addr).await?;
    println!("Server running on {}", addr);

    axum::serve(listener, app).await
}
