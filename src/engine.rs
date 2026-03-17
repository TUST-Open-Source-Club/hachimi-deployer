use std::{path::Path, sync::Arc};

use bytes::Bytes;
use http::{
    Method, Request, StatusCode,
    header::{CONTENT_TYPE, HeaderValue},
};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{fs::File, io::AsyncReadExt};
use tracing::info;

use crate::error::AppError;

#[derive(Clone)]
pub struct EngineClient {
    socket_path: Arc<std::path::PathBuf>,
    http: Client<hyperlocal::UnixConnector, Full<Bytes>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContainerReplaceOutcome {
    pub container_id: String,
    pub container_name: String,
    pub new_container_id: Option<String>,
    pub status: ReplaceStatus,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplaceStatus {
    Replaced,
    Failed,
}

#[derive(Debug, Deserialize)]
struct ImageLoadLine {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_detail: Option<Value>,
}

impl EngineClient {
    pub fn new(socket_path: impl Into<std::path::PathBuf>) -> Self {
        let connector = hyperlocal::UnixConnector;
        let http = Client::builder(TokioExecutor::new()).build(connector);
        Self {
            socket_path: Arc::new(socket_path.into()),
            http,
        }
    }

    pub async fn load_image_from_path(&self, tar_path: &Path) -> Result<(), AppError> {
        let mut file = File::open(tar_path).await?;
        let metadata = file.metadata().await?;
        let mut buffer = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut buffer).await?;

        let request = self.request_builder(
            Method::POST,
            "/images/load?quiet=1",
            Full::new(Bytes::from(buffer)),
            Some(HeaderValue::from_static("application/x-tar")),
        )?;
        let response = self.send(request).await?;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|err| AppError::EngineRequest(err.to_string()))?
            .to_bytes();

        if !status.is_success() {
            return Err(AppError::EngineResponse(format!(
                "image load failed with status {status}: {}",
                String::from_utf8_lossy(&body)
            )));
        }

        for line in String::from_utf8_lossy(&body).lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(parsed) = serde_json::from_str::<ImageLoadLine>(line) {
                if let Some(error) = parsed.error {
                    return Err(AppError::EngineResponse(error));
                }
                if let Some(details) = parsed.error_detail {
                    return Err(AppError::EngineResponse(details.to_string()));
                }
            }
        }

        Ok(())
    }

    pub async fn list_containers_by_image(
        &self,
        image_ref: &str,
    ) -> Result<Vec<ContainerSummary>, AppError> {
        let response = self
            .send(self.request_builder(
                Method::GET,
                "/containers/json?all=1",
                Full::new(Bytes::new()),
                None,
            )?)
            .await?;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|err| AppError::EngineRequest(err.to_string()))?
            .to_bytes();

        if !status.is_success() {
            return Err(AppError::EngineResponse(format!(
                "container list failed with status {status}: {}",
                String::from_utf8_lossy(&body)
            )));
        }

        let entries: Vec<Value> = serde_json::from_slice(&body)
            .map_err(|err| AppError::EngineResponse(err.to_string()))?;

        let containers = entries
            .into_iter()
            .filter_map(|entry| {
                let image = entry.get("Image")?.as_str()?;
                if image != image_ref {
                    return None;
                }
                let id = entry.get("Id")?.as_str()?.to_owned();
                let name = entry
                    .get("Names")
                    .and_then(Value::as_array)
                    .and_then(|names| names.first())
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim_start_matches('/')
                    .to_owned();
                Some(ContainerSummary { id, name })
            })
            .collect();

        Ok(containers)
    }

    pub async fn replace_container(
        &self,
        container: &ContainerSummary,
        image_ref: &str,
    ) -> ContainerReplaceOutcome {
        match self.replace_container_inner(container, image_ref).await {
            Ok(new_id) => ContainerReplaceOutcome {
                container_id: container.id.clone(),
                container_name: container.name.clone(),
                new_container_id: Some(new_id),
                status: ReplaceStatus::Replaced,
                message: "container replaced successfully".to_owned(),
            },
            Err(err) => ContainerReplaceOutcome {
                container_id: container.id.clone(),
                container_name: container.name.clone(),
                new_container_id: None,
                status: ReplaceStatus::Failed,
                message: err.to_string(),
            },
        }
    }

    async fn replace_container_inner(
        &self,
        container: &ContainerSummary,
        image_ref: &str,
    ) -> Result<String, AppError> {
        let inspect = self.inspect_container(&container.id).await?;
        let original_name = inspect
            .pointer("/Name")
            .and_then(Value::as_str)
            .unwrap_or(container.name.as_str())
            .trim_start_matches('/')
            .to_owned();
        let temp_name = format!("{original_name}-replacement");

        let create_payload = build_create_payload(&inspect, image_ref)?;
        let create_result = self.create_container(&temp_name, create_payload).await?;
        let new_id = create_result
            .get("Id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AppError::ContainerReplace("create response missing container id".to_owned())
            })?
            .to_owned();

        if let Err(err) = self.stop_container(&container.id).await {
            let _ = self.remove_container(&new_id, true).await;
            return Err(err);
        }

        if let Err(err) = self.remove_container(&container.id, false).await {
            let _ = self.start_container(&container.id).await;
            let _ = self.remove_container(&new_id, true).await;
            return Err(err);
        }

        if let Err(err) = self.rename_container(&new_id, &original_name).await {
            let _ = self.start_container(&new_id).await;
            return Err(err);
        }

        if let Err(err) = self.start_container(&new_id).await {
            return Err(err);
        }

        info!(
            old_container_id = %container.id,
            new_container_id = %new_id,
            container_name = %original_name,
            "replaced container"
        );

        Ok(new_id)
    }

    async fn inspect_container(&self, id: &str) -> Result<Value, AppError> {
        self.get_json(&format!("/containers/{id}/json")).await
    }

    async fn create_container(&self, name: &str, payload: Value) -> Result<Value, AppError> {
        self.post_json(
            &format!("/containers/create?name={}", urlencoding::encode(name)),
            payload,
        )
        .await
    }

    async fn stop_container(&self, id: &str) -> Result<(), AppError> {
        self.post_empty(&format!("/containers/{id}/stop?t=10"))
            .await
    }

    async fn start_container(&self, id: &str) -> Result<(), AppError> {
        self.post_empty(&format!("/containers/{id}/start")).await
    }

    async fn remove_container(&self, id: &str, force: bool) -> Result<(), AppError> {
        let uri = format!("/containers/{id}?force={}", if force { 1 } else { 0 });
        self.delete_empty(&uri).await
    }

    async fn rename_container(&self, id: &str, new_name: &str) -> Result<(), AppError> {
        self.post_empty(&format!(
            "/containers/{id}/rename?name={}",
            urlencoding::encode(new_name)
        ))
        .await
    }

    async fn get_json(&self, uri: &str) -> Result<Value, AppError> {
        let response = self
            .send(self.request_builder(Method::GET, uri, Full::new(Bytes::new()), None)?)
            .await?;
        parse_json_response(response).await
    }

    async fn post_json(&self, uri: &str, payload: Value) -> Result<Value, AppError> {
        let body =
            serde_json::to_vec(&payload).map_err(|err| AppError::EngineRequest(err.to_string()))?;
        let body = Full::new(Bytes::from(body));
        let response = self
            .send(self.request_builder(
                Method::POST,
                uri,
                body,
                Some(HeaderValue::from_static("application/json")),
            )?)
            .await?;
        parse_json_response(response).await
    }

    async fn post_empty(&self, uri: &str) -> Result<(), AppError> {
        let response = self
            .send(self.request_builder(Method::POST, uri, Full::new(Bytes::new()), None)?)
            .await?;
        parse_empty_response(response).await
    }

    async fn delete_empty(&self, uri: &str) -> Result<(), AppError> {
        let response = self
            .send(self.request_builder(Method::DELETE, uri, Full::new(Bytes::new()), None)?)
            .await?;
        parse_empty_response(response).await
    }

    fn request_builder(
        &self,
        method: Method,
        uri: &str,
        body: Full<Bytes>,
        content_type: Option<HeaderValue>,
    ) -> Result<Request<Full<Bytes>>, AppError> {
        let url: http::Uri = hyperlocal::Uri::new(&*self.socket_path, uri).into();
        let mut builder = Request::builder().method(method).uri(url);

        if let Some(content_type) = content_type {
            builder = builder.header(CONTENT_TYPE, content_type);
        }

        builder
            .body(body)
            .map_err(|err: http::Error| AppError::EngineRequest(err.to_string()))
    }

    async fn send(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<http::Response<Incoming>, AppError> {
        self.http
            .request(request)
            .await
            .map_err(|err| AppError::EngineRequest(err.to_string()))
    }
}

fn build_create_payload(inspect: &Value, image_ref: &str) -> Result<Value, AppError> {
    let mut config = inspect
        .get("Config")
        .cloned()
        .ok_or_else(|| AppError::ContainerReplace("container inspect missing Config".to_owned()))?;

    if let Some(config_obj) = config.as_object_mut() {
        config_obj.remove("HostnamePath");
        config_obj.remove("HostsPath");
        config_obj.remove("Image");
    }

    let host_config = inspect
        .get("HostConfig")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let networking_config = inspect
        .get("NetworkSettings")
        .and_then(|settings| settings.get("Networks"))
        .cloned()
        .map(|networks| json!({ "EndpointsConfig": networks }))
        .unwrap_or_else(|| json!({}));

    let config_obj = config.as_object().ok_or_else(|| {
        AppError::ContainerReplace("container Config is not an object".to_owned())
    })?;

    let mut payload = serde_json::Map::new();
    for key in [
        "Cmd",
        "Entrypoint",
        "Env",
        "ExposedPorts",
        "Labels",
        "OpenStdin",
        "StdinOnce",
        "Tty",
        "User",
        "WorkingDir",
        "StopSignal",
        "StopTimeout",
        "AttachStderr",
        "AttachStdin",
        "AttachStdout",
    ] {
        if let Some(value) = config_obj.get(key) {
            payload.insert(key.to_owned(), value.clone());
        }
    }
    payload.insert("Image".to_owned(), Value::String(image_ref.to_owned()));
    payload.insert("HostConfig".to_owned(), host_config);
    payload.insert("NetworkingConfig".to_owned(), networking_config);

    Ok(Value::Object(payload))
}

async fn parse_json_response(response: http::Response<Incoming>) -> Result<Value, AppError> {
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|err| AppError::EngineRequest(err.to_string()))?
        .to_bytes();

    if status == StatusCode::NO_CONTENT {
        return Ok(Value::Null);
    }

    if !status.is_success() {
        return Err(AppError::EngineResponse(format!(
            "status {status}: {}",
            String::from_utf8_lossy(&body)
        )));
    }

    serde_json::from_slice(&body).map_err(|err| AppError::EngineResponse(err.to_string()))
}

async fn parse_empty_response(response: http::Response<Incoming>) -> Result<(), AppError> {
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|err| AppError::EngineRequest(err.to_string()))?
        .to_bytes();

    if status.is_success() || status == StatusCode::NOT_MODIFIED {
        return Ok(());
    }

    Err(AppError::EngineResponse(format!(
        "status {status}: {}",
        String::from_utf8_lossy(&body)
    )))
}
