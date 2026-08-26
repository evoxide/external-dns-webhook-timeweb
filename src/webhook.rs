use crate::{
    metrics::Metrics,
    model::{Changes, Endpoint},
    provider::{Provider, ProviderError},
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use std::sync::Arc;

pub const MEDIA_TYPE: &str = "application/external.dns.webhook+json;version=1";

#[derive(Clone)]
pub struct AppState {
    pub provider: Arc<Provider>,
    pub metrics: Arc<Metrics>,
}

impl AppState {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider: Arc::new(provider),
            metrics: Arc::new(Metrics::default()),
        }
    }
}

#[derive(Clone)]
struct HealthState {
    metrics: Arc<Metrics>,
}

pub fn provider_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(negotiate))
        .route("/records", get(records).post(apply_changes))
        .route("/adjustendpoints", post(adjust_endpoints))
        .route("/healthz", get(healthz))
        .with_state(state)
}

pub fn health_router(metrics: Arc<Metrics>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics_handler))
        .with_state(HealthState { metrics })
}

async fn negotiate(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, WebhookError> {
    ensure_accept(&headers)?;
    Ok(json_response(state.provider.domain_filter()))
}

async fn records(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, WebhookError> {
    state.metrics.inc_records_requests();
    if let Err(error) = ensure_accept(&headers) {
        state.metrics.inc_records_errors();
        return Err(error);
    }
    match state.provider.records().await {
        Ok(records) => Ok(json_response(records)),
        Err(error) => {
            state.metrics.inc_records_errors();
            Err(WebhookError::from_provider(error))
        }
    }
}

async fn apply_changes(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, WebhookError> {
    state.metrics.inc_changes_requests();
    if let Err(error) = ensure_content_type(&headers) {
        state.metrics.inc_changes_errors();
        return Err(error);
    }
    let changes = match serde_json::from_slice::<Changes>(&body) {
        Ok(changes) => changes,
        Err(error) => {
            state.metrics.inc_changes_errors();
            return Err(WebhookError::bad_request(format!(
                "invalid changes payload: {error}"
            )));
        }
    };
    match state.provider.apply_changes(changes).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(error) => {
            state.metrics.inc_changes_errors();
            tracing::error!(error = %error, "failed to apply DNS changes");
            Err(WebhookError::from_provider(error))
        }
    }
}

async fn adjust_endpoints(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, WebhookError> {
    state.metrics.inc_adjust_requests();
    if let Err(error) = ensure_accept(&headers) {
        state.metrics.inc_adjust_errors();
        return Err(error);
    }
    if let Err(error) = ensure_content_type(&headers) {
        state.metrics.inc_adjust_errors();
        return Err(error);
    }
    let endpoints = match serde_json::from_slice::<Vec<Endpoint>>(&body) {
        Ok(endpoints) => endpoints,
        Err(error) => {
            state.metrics.inc_adjust_errors();
            return Err(WebhookError::bad_request(format!(
                "invalid endpoints payload: {error}"
            )));
        }
    };
    let adjusted = state.provider.adjust_endpoints(endpoints);
    Ok(json_response(adjusted))
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn metrics_handler(State(state): State<HealthState>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        )],
        state.metrics.render(),
    )
}

fn ensure_accept(headers: &HeaderMap) -> Result<(), WebhookError> {
    let Some(value) = headers.get(header::ACCEPT) else {
        return Ok(());
    };
    let value = value
        .to_str()
        .map_err(|_| WebhookError::not_acceptable("invalid Accept header"))?;
    if value == MEDIA_TYPE || value.split(',').any(|part| part.trim() == MEDIA_TYPE) {
        Ok(())
    } else {
        Err(WebhookError::not_acceptable(format!(
            "unsupported Accept media type: {value}"
        )))
    }
}

fn ensure_content_type(headers: &HeaderMap) -> Result<(), WebhookError> {
    let Some(value) = headers.get(header::CONTENT_TYPE) else {
        return Ok(());
    };
    let value = value
        .to_str()
        .map_err(|_| WebhookError::unsupported_media_type("invalid Content-Type header"))?;
    if value == MEDIA_TYPE || value.starts_with(&format!("{MEDIA_TYPE};")) {
        Ok(())
    } else {
        Err(WebhookError::unsupported_media_type(format!(
            "unsupported Content-Type media type: {value}"
        )))
    }
}

fn json_response<T>(value: T) -> Response
where
    T: Serialize,
{
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE));
    response
}

#[derive(Debug)]
struct WebhookError {
    status: StatusCode,
    message: String,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl WebhookError {
    fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }

    fn not_acceptable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_ACCEPTABLE,
            message: message.into(),
        }
    }

    fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
            message: message.into(),
        }
    }

    fn from_provider(error: ProviderError) -> Self {
        let status = match StatusCode::from_u16(error.http_status()) {
            Ok(status) => status,
            Err(_) => StatusCode::BAD_GATEWAY,
        };
        Self {
            status,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{AppState, MEDIA_TYPE, provider_router};
    use crate::{
        domain_filter::DomainFilter, model::Endpoint, provider::Provider, timeweb::TimewebClient,
    };
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;
    use url::Url;

    #[tokio::test]
    async fn negotiates_external_dns_media_type() -> Result<(), Box<dyn std::error::Error>> {
        let client = TimewebClient::new(
            Url::parse("http://127.0.0.1:1")?,
            "token",
            std::time::Duration::from_secs(1),
        )?;
        let provider = Provider::new(
            client,
            DomainFilter::new(Vec::new(), Vec::new(), None, None),
        );
        let response = provider_router(AppState::new(provider))
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("Accept", MEDIA_TYPE)
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], MEDIA_TYPE);
        let body = response.into_body().collect().await?.to_bytes();
        assert_eq!(body, &b"{}"[..]);
        Ok(())
    }

    #[tokio::test]
    async fn echoes_adjust_endpoints_without_calling_timeweb()
    -> Result<(), Box<dyn std::error::Error>> {
        let client = TimewebClient::new(
            Url::parse("http://127.0.0.1:1")?,
            "token",
            std::time::Duration::from_secs(1),
        )?;
        let provider = Provider::new(
            client,
            DomainFilter::new(Vec::new(), Vec::new(), None, None),
        );
        let state = AppState::new(provider);
        let endpoint = Endpoint {
            dns_name: "www.example.com".to_owned(),
            targets: vec!["192.0.2.1".to_owned()],
            record_type: "A".to_owned(),
            set_identifier: String::new(),
            record_ttl: 60,
            labels: Default::default(),
            provider_specific: Vec::new(),
        };
        let body = serde_json::to_vec(&vec![endpoint])?;
        let response = provider_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/adjustendpoints")
                    .header("Content-Type", MEDIA_TYPE)
                    .body(Body::from(body))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }
}
