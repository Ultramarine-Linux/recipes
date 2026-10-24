//! HTTP endpoints:
//!
//! - `POST /hooks/github`: GitHub webhook; pushes to the branch queue a build
//! - `GET /hooks/status`: live commit, queue and last error, as JSON
//! - `GET /healthz`: liveness

use std::{
    sync::{Arc, Mutex, PoisonError},
    time::SystemTime,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::map_response,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    config::Config,
    queue::BuildQueue,
    release,
    webhook::{route_event, verify_signature},
};

/// Push payloads with many commits get big, but not this big.
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub queue: BuildQueue,
    pub errors: ErrorLog,
}

/// The most recent failure, shown on the status page until a build succeeds.
#[derive(Clone, Default)]
pub struct ErrorLog(Arc<Mutex<Option<LastError>>>);

#[derive(Debug, Clone, Serialize)]
pub struct LastError {
    /// `None` when resolving the branch head failed.
    pub sha: Option<String>,
    pub at: String,
    pub message: String,
}

impl ErrorLog {
    pub fn record(&self, sha: Option<&str>, error: &anyhow::Error) {
        let message = format!("{error:#}");
        if let Some(sha) = sha {
            tracing::error!("build {sha} failed: {message}");
        } else {
            tracing::error!("{message}");
        }
        // Check output can be long; the status page only needs the gist.
        let message = message.chars().take(8000).collect();
        *self.lock() = Some(LastError {
            sha: sha.map(str::to_owned),
            at: timestamp(SystemTime::now()),
            message,
        });
    }

    pub fn clear(&self) {
        *self.lock() = None;
    }

    #[must_use]
    pub fn last(&self) -> Option<LastError> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<LastError>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/hooks/github", post(webhook))
        .route("/hooks/status", get(status))
        .route("/healthz", get(|| async { "ok\n" }))
        .fallback(|| async { (StatusCode::NOT_FOUND, "not found\n") })
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(map_response(|mut response: Response| async move {
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }))
        .with_state(state)
}

async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    if let Some(secret) = &state.config.webhook_secret
        && !verify_signature(secret.as_bytes(), &body, header("x-hub-signature-256"))
    {
        return (StatusCode::UNAUTHORIZED, "bad signature\n").into_response();
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            "expected a JSON payload (content type application/json)\n",
        )
            .into_response();
    };
    let event = header("x-github-event").unwrap_or_default();
    let route = route_event(event, &payload, &state.config.repo, &state.config.branch);
    tracing::info!(
        "webhook {}: {}",
        header("x-github-delivery").unwrap_or("-"),
        route.message
    );
    if let Some(sha) = route.build {
        state.queue.request(sha);
    }
    let status = StatusCode::from_u16(route.status).unwrap_or(StatusCode::OK);
    if status == StatusCode::NO_CONTENT {
        status.into_response()
    } else {
        (status, format!("{}\n", route.message)).into_response()
    }
}

#[derive(Serialize)]
struct Status<'a> {
    repo: &'a str,
    branch: &'a str,
    /// The live commit.
    current: Option<String>,
    built_at: Option<String>,
    building: Option<String>,
    pending: Option<String>,
    last_error: Option<LastError>,
}

async fn status(State(state): State<AppState>) -> Json<Value> {
    let live = release::current(&state.config.data_dir);
    let slots = state.queue.slots();
    let status = Status {
        repo: &state.config.repo,
        branch: &state.config.branch,
        built_at: live.as_ref().map(|live| timestamp(live.built_at)),
        current: live.map(|live| live.sha),
        building: slots.running,
        pending: slots.pending,
        last_error: state.errors.last(),
    };
    Json(serde_json::to_value(status).unwrap_or_default())
}

fn timestamp(time: SystemTime) -> String {
    humantime::format_rfc3339_seconds(time).to_string()
}

#[cfg(test)]
mod tests {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    use tokio::sync::mpsc;

    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    async fn serve(
        secret: Option<&str>,
    ) -> (String, mpsc::UnboundedReceiver<String>, tempfile::TempDir) {
        let data = tempfile::tempdir().unwrap();
        let config = Config::from_env(|name| match name {
            "DATA_DIR" => Some(data.path().to_str().unwrap().to_owned()),
            "GITHUB_WEBHOOK_SECRET" => secret.map(str::to_owned),
            "ALLOW_UNSIGNED_WEBHOOKS" => Some("1".into()),
            _ => None,
        })
        .unwrap();
        let (built, receiver) = mpsc::unbounded_channel();
        let queue = BuildQueue::new(move |sha| {
            let built = built.clone();
            async move { built.send(sha).unwrap() }
        });
        let state = AppState {
            config: Arc::new(config),
            queue,
            errors: ErrorLog::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await });
        (url, receiver, data)
    }

    fn push() -> String {
        serde_json::json!({
            "ref": "refs/heads/main", "after": SHA, "deleted": false,
            "repository": { "full_name": "Ultramarine-Server/recipes" }
        })
        .to_string()
    }

    async fn deliver(
        url: &str,
        event: &str,
        body: String,
        signature: Option<String>,
    ) -> reqwest::Response {
        let mut request = reqwest::Client::new()
            .post(format!("{url}/hooks/github"))
            .header("x-github-event", event)
            .header("content-type", "application/json")
            .body(body);
        if let Some(signature) = signature {
            request = request.header("x-hub-signature-256", signature);
        }
        request.send().await.unwrap()
    }

    fn sign(body: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(b"hunter2").unwrap();
        mac.update(body.as_bytes());
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[tokio::test]
    async fn signed_pushes_queue_builds() {
        let (url, mut built, _data) = serve(Some("hunter2")).await;

        let unsigned = deliver(&url, "push", push(), None).await;
        assert_eq!(unsigned.status(), 401);
        let forged = deliver(&url, "push", push(), Some(sign("{}"))).await;
        assert_eq!(forged.status(), 401);

        let ping = deliver(&url, "ping", "{}".into(), Some(sign("{}"))).await;
        assert_eq!(ping.status(), 200);
        let other = deliver(&url, "star", "{}".into(), Some(sign("{}"))).await;
        assert_eq!(other.status(), 204);

        let accepted = deliver(&url, "push", push(), Some(sign(&push()))).await;
        assert_eq!(accepted.status(), 202);
        assert_eq!(accepted.headers()["cache-control"], "no-store");
        assert_eq!(built.recv().await.unwrap(), SHA);

        let too_big = "x".repeat(MAX_BODY_BYTES + 1);
        let rejected = deliver(&url, "push", too_big.clone(), Some(sign(&too_big))).await;
        assert_eq!(rejected.status(), 413);
    }

    #[tokio::test]
    async fn reports_status_and_health() {
        let (url, _built, _data) = serve(None).await;
        let accepted = deliver(&url, "push", push(), None).await;
        assert_eq!(accepted.status(), 202);

        let client = reqwest::Client::new();
        let health = client.get(format!("{url}/healthz")).send().await.unwrap();
        assert_eq!(health.text().await.unwrap(), "ok\n");
        let status: Value = serde_json::from_str(
            &client
                .get(format!("{url}/hooks/status"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(status["repo"], "Ultramarine-Server/recipes");
        assert_eq!(status["current"], Value::Null);
        assert_eq!(status["last_error"], Value::Null);
        let missing = client.get(format!("{url}/nope")).send().await.unwrap();
        assert_eq!(missing.status(), 404);
        let wrong_method = client
            .get(format!("{url}/hooks/github"))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_method.status(), 405);
    }
}
