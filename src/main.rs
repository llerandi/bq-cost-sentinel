mod config;
mod auth;
mod server;
mod google_client;
mod pricing;

use config::AppConfig;
use std::{
    net::SocketAddr, 
    sync::Arc
};
use tracing::{
    info,
    Level
};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .json()
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("Error initializing the logging (tracing) component");

    let config = AppConfig::load_from_env();

    let google_client = google_client::BqClient::new();

    let shared_state = Arc::new(server::AppState {
        config: config.clone(),
        google_client
    });

    info!(
        max_cost = config.max_cost_per_query,
        price_per_tib = config.price_per_tib,
        enforce_mode = config.enforce_mode,
        "Starting BigQuery Cost Sentinel in proxy mode..."
    );

    let app = server::build_router(shared_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!("Server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}