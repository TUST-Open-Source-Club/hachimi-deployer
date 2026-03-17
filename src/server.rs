use std::{path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::header::AUTHORIZATION,
    routing::put,
};
use futures::StreamExt;
use percent_encoding::percent_decode_str;
use serde::Serialize;
use tokio::{fs::File, io::AsyncWriteExt};
use tower_http::trace::TraceLayer;
use tracing::{Instrument, info, info_span, warn};
use uuid::Uuid;

use crate::{
    config::AppConfig,
    engine::{ContainerReplaceOutcome, EngineClient, ReplaceStatus},
    error::AppError,
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub engine: EngineClient,
}

#[derive(Debug, Serialize)]
pub struct DeployResponse {
    pub trace_id: Uuid,
    pub image_ref: String,
    pub bytes_received: u64,
    pub replaced: usize,
    pub failed: usize,
    pub containers: Vec<ContainerReplaceOutcome>,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/deploy/{image_ref}", put(deploy_image))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn deploy_image(
    State(state): State<AppState>,
    Path(encoded_image_ref): Path<String>,
    request: Request,
) -> Result<Json<DeployResponse>, AppError> {
    let image_ref = decode_image_ref(&encoded_image_ref)?;
    let policy = state
        .config
        .image_policies
        .get(&image_ref)
        .ok_or(AppError::UnknownImage)?;

    let token = extract_bearer_token(request.headers().get(AUTHORIZATION))?;
    if token != policy.bearer_token {
        return Err(AppError::Unauthorized);
    }

    let trace_id = Uuid::new_v4();
    let remote_addr = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.to_string())
        .unwrap_or_else(|| "unknown".to_owned());

    let span = info_span!(
        "deploy_request",
        %trace_id,
        remote_addr = %remote_addr,
        image_ref = %policy.image_ref
    );

    deploy_image_inner(state, image_ref, request, trace_id)
        .instrument(span)
        .await
        .map(Json)
}

async fn deploy_image_inner(
    state: AppState,
    image_ref: String,
    request: Request,
    trace_id: Uuid,
) -> Result<DeployResponse, AppError> {
    let (temp_path, bytes_received) = write_body_to_tempfile(request).await?;
    info!(%image_ref, bytes_received, temp_path = %temp_path.display(), "image upload persisted");

    let load_result = state.engine.load_image_from_path(&temp_path).await;
    let cleanup_result = tokio::fs::remove_file(&temp_path).await;
    if let Err(err) = cleanup_result {
        warn!(path = %temp_path.display(), error = %err, "failed to remove temporary image tar");
    }
    load_result?;

    let containers = state.engine.list_containers_by_image(&image_ref).await?;
    info!(%image_ref, container_count = containers.len(), "matched containers for replacement");

    let mut outcomes = Vec::with_capacity(containers.len());
    for container in containers {
        let outcome = state.engine.replace_container(&container, &image_ref).await;
        match outcome.status {
            ReplaceStatus::Replaced => info!(
                trace_id = %trace_id,
                container_id = %outcome.container_id,
                container_name = %outcome.container_name,
                new_container_id = %outcome.new_container_id.as_deref().unwrap_or_default(),
                "container replacement succeeded"
            ),
            ReplaceStatus::Failed => warn!(
                trace_id = %trace_id,
                container_id = %outcome.container_id,
                container_name = %outcome.container_name,
                error = %outcome.message,
                "container replacement failed"
            ),
        }
        outcomes.push(outcome);
    }

    let replaced = outcomes
        .iter()
        .filter(|outcome| matches!(outcome.status, ReplaceStatus::Replaced))
        .count();
    let failed = outcomes.len().saturating_sub(replaced);

    Ok(DeployResponse {
        trace_id,
        image_ref,
        bytes_received,
        replaced,
        failed,
        containers: outcomes,
    })
}

async fn write_body_to_tempfile(request: Request) -> Result<(PathBuf, u64), AppError> {
    let temp_dir = tempfile::tempdir().map_err(AppError::ConfigRead)?;
    let temp_path = temp_dir
        .path()
        .join(format!("upload-{}.tar", Uuid::new_v4()));
    let mut file = File::create(&temp_path).await?;
    let mut body = request.into_body().into_data_stream();
    let mut bytes_received = 0_u64;

    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|_| AppError::InvalidBody)?;
        bytes_received += chunk.len() as u64;
        file.write_all(&chunk).await?;
    }

    file.flush().await?;
    let persisted = temp_dir.keep();
    let final_path = persisted.join(
        temp_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(AppError::InvalidBody)?,
    );

    Ok((final_path, bytes_received))
}

fn decode_image_ref(encoded_image_ref: &str) -> Result<String, AppError> {
    let decoded = percent_decode_str(encoded_image_ref)
        .decode_utf8()
        .map_err(|_| AppError::InvalidImageReference)?;
    if decoded.is_empty() {
        return Err(AppError::InvalidImageReference);
    }
    Ok(decoded.into_owned())
}

fn extract_bearer_token(header: Option<&axum::http::HeaderValue>) -> Result<&str, AppError> {
    let header = header.ok_or(AppError::MissingBearerToken)?;
    let value = header.to_str().map_err(|_| AppError::MissingBearerToken)?;
    value
        .strip_prefix("Bearer ")
        .ok_or(AppError::MissingBearerToken)
}

#[cfg(test)]
mod tests {
    use super::{decode_image_ref, extract_bearer_token};
    use axum::http::HeaderValue;

    #[test]
    fn decodes_image_ref() {
        let decoded = decode_image_ref("ghcr.io%2Facme%2Fapp%3Aprod").unwrap();
        assert_eq!(decoded, "ghcr.io/acme/app:prod");
    }

    #[test]
    fn extracts_bearer_token() {
        let header = HeaderValue::from_static("Bearer abc");
        let token = extract_bearer_token(Some(&header)).unwrap();
        assert_eq!(token, "abc");
    }
}
