use std::{collections::HashMap, net::SocketAddr, path::PathBuf};

use serde::Deserialize;

use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub engine: EngineConfig,
    pub image_policies: HashMap<String, ImagePolicy>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub listen: SocketAddr,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EngineConfig {
    pub socket_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ImagePolicy {
    pub image_ref: String,
    pub bearer_token: String,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    server: ServerConfig,
    engine: EngineConfig,
    #[serde(default)]
    images: Vec<RawImagePolicy>,
}

#[derive(Debug, Deserialize)]
struct RawImagePolicy {
    image_ref: String,
    bearer_token: String,
}

impl AppConfig {
    pub fn from_toml(input: &str) -> Result<Self, AppError> {
        let raw: RawConfig = toml::from_str(input).map_err(AppError::TomlParse)?;
        let mut image_policies = HashMap::with_capacity(raw.images.len());

        for image in raw.images {
            let image_ref = image.image_ref.trim().to_owned();
            let bearer_token = image.bearer_token.trim().to_owned();

            if image_ref.is_empty() {
                return Err(AppError::Config(
                    "image_ref entries must not be empty".to_owned(),
                ));
            }
            if bearer_token.is_empty() {
                return Err(AppError::Config(format!(
                    "bearer_token for image '{image_ref}' must not be empty"
                )));
            }
            if image_policies.contains_key(&image_ref) {
                return Err(AppError::Config(format!(
                    "duplicate image_ref '{image_ref}' in configuration"
                )));
            }

            image_policies.insert(
                image_ref.clone(),
                ImagePolicy {
                    image_ref,
                    bearer_token,
                },
            );
        }

        if image_policies.is_empty() {
            return Err(AppError::Config(
                "at least one [[images]] entry is required".to_owned(),
            ));
        }

        Ok(Self {
            server: raw.server,
            engine: raw.engine,
            image_policies,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::AppConfig;

    #[test]
    fn parses_valid_config() {
        let config = AppConfig::from_toml(
            r#"
            [server]
            listen = "127.0.0.1:3000"

            [engine]
            socket_path = "/var/run/docker.sock"

            [[images]]
            image_ref = "ghcr.io/acme/app:prod"
            bearer_token = "token-a"
            "#,
        )
        .unwrap();

        assert_eq!(config.image_policies.len(), 1);
        assert!(config.image_policies.contains_key("ghcr.io/acme/app:prod"));
    }

    #[test]
    fn rejects_duplicate_images() {
        let err = AppConfig::from_toml(
            r#"
            [server]
            listen = "127.0.0.1:3000"

            [engine]
            socket_path = "/var/run/docker.sock"

            [[images]]
            image_ref = "ghcr.io/acme/app:prod"
            bearer_token = "token-a"

            [[images]]
            image_ref = "ghcr.io/acme/app:prod"
            bearer_token = "token-b"
            "#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("duplicate image_ref"));
    }
}
