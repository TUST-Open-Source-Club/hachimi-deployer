use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;
use tracing::error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("failed to read config file: {0}")]
    ConfigRead(#[from] std::io::Error),
    #[error("failed to parse config file: {0}")]
    TomlParse(#[from] toml::de::Error),
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("unsupported image reference")]
    UnknownImage,
    #[error("invalid image reference encoding")]
    InvalidImageReference,
    #[error("missing bearer token")]
    MissingBearerToken,
    #[error("request body too large or malformed")]
    InvalidBody,
    #[error("engine request failed: {0}")]
    EngineRequest(String),
    #[error("engine returned an error: {0}")]
    EngineResponse(String),
    #[error("container replacement failed: {0}")]
    ContainerReplace(String),
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

impl AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized | Self::MissingBearerToken => StatusCode::UNAUTHORIZED,
            Self::UnknownImage => StatusCode::NOT_FOUND,
            Self::InvalidImageReference | Self::InvalidBody | Self::Config(_) => {
                StatusCode::BAD_REQUEST
            }
            Self::ConfigRead(_) | Self::TomlParse(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::EngineRequest(_) | Self::EngineResponse(_) | Self::ContainerReplace(_) => {
                StatusCode::BAD_GATEWAY
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        if status.is_server_error() {
            error!(error = %self, "request failed");
        }
        (
            status,
            Json(ErrorBody {
                error: &self.to_string(),
            }),
        )
            .into_response()
    }
}
