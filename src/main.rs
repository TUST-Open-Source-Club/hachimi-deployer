mod config;
mod engine;
mod error;
mod server;

use std::{env, path::PathBuf, sync::Arc};

use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    config::AppConfig,
    engine::EngineClient,
    server::{AppState, build_router},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let config_path = resolve_config_path()?;
    let config_contents = tokio::fs::read_to_string(&config_path).await?;
    let config = AppConfig::from_toml(&config_contents)?;
    let listen_addr = config.server.listen;
    let engine_socket = config.engine.socket_path.clone();

    let state = AppState {
        config: Arc::new(config),
        engine: EngineClient::new(engine_socket),
    };

    let listener = TcpListener::bind(listen_addr).await?;
    info!(listen_addr = %listen_addr, config_path = %config_path.display(), "server listening");

    axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}

fn resolve_config_path() -> Result<PathBuf, std::io::Error> {
    match env::var_os("HACHIMI_CONFIG") {
        Some(path) => Ok(PathBuf::from(path)),
        None => Ok(PathBuf::from("config/deployer.toml")),
    }
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,hachimi_deployer=info")),
        )
        .with(fmt::layer().json())
        .init();
}
