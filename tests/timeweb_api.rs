use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch},
};
use external_dns_webhook_timeweb::{
    domain_filter::DomainFilter,
    model::{Changes, Endpoint},
    provider::Provider,
    timeweb::TimewebClient,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::TcpListener;
use url::Url;

#[derive(Clone, Default)]
struct MockState {
    requests: Arc<Mutex<Vec<MockRequest>>>,
}

#[derive(Clone, Debug)]
struct MockRequest {
    method: Method,
    path: String,
    authorization: String,
    body: Option<Value>,
}

#[tokio::test]
async fn records_and_changes_use_timeweb_api_contract() -> Result<(), Box<dyn std::error::Error>> {
    let state = MockState::default();
    let app = Router::new()
        .route("/api/v1/domains", get(list_domains))
        .route(
            "/api/v1/domains/{zone}/dns-records",
            get(list_records).post(create_record),
        )
        .route(
            "/api/v1/domains/{owner}/dns-records/{id}",
            patch(update_record).delete(delete_record),
        )
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let client = TimewebClient::new(
        Url::parse(&format!("http://{address}"))?,
        "test-token",
        Duration::from_secs(2),
    )?;
    let provider = Provider::new(
        client,
        DomainFilter::new(Vec::new(), Vec::new(), None, None),
    );

    let records = provider.records().await?;
    assert_eq!(records.len(), 2);
    assert!(records.iter().any(|endpoint| {
        endpoint.dns_name == "www.example.com"
            && endpoint.record_type == "A"
            && endpoint.targets == ["192.0.2.1"]
    }));
    assert!(records.iter().any(|endpoint| {
        endpoint.dns_name == "mail.example.com"
            && endpoint.record_type == "MX"
            && endpoint.targets == ["10 mx.example.com"]
    }));

    let changes = Changes {
        create: vec![endpoint("_acme.example.com", "TXT", "token-value", 60)],
        update_old: vec![endpoint("www.example.com", "A", "192.0.2.1", 300)],
        update_new: vec![endpoint("www.example.com", "A", "192.0.2.2", 60)],
        delete: vec![endpoint("mail.example.com", "MX", "10 mx.example.com", 300)],
    };
    provider.apply_changes(changes).await?;

    let requests = state
        .requests
        .lock()
        .map_err(|_| "mock request lock was poisoned")?
        .clone();
    assert!(
        requests
            .iter()
            .all(|request| request.authorization == "Bearer test-token")
    );

    assert!(requests.iter().any(|request| {
        request.method == Method::DELETE
            && request.path == "/api/v1/domains/example.com/dns-records/2"
    }));
    assert!(requests.iter().any(|request| {
        request.method == Method::PATCH
            && request.path == "/api/v1/domains/example.com/dns-records/1"
            && request.body.as_ref().is_some_and(|body| {
                body == &json!({
                    "type":"A",
                    "subdomain":"www.example.com",
                    "value":"192.0.2.2",
                    "ttl":60
                })
            })
    }));
    assert!(requests.iter().any(|request| {
        request.method == Method::POST
            && request.path == "/api/v1/domains/example.com/dns-records"
            && request.body.as_ref().is_some_and(|body| {
                body == &json!({
                    "type":"TXT",
                    "subdomain":"_acme.example.com",
                    "value":"token-value",
                    "ttl":60
                })
            })
    }));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn records_include_records_from_timeweb_subdomains() -> Result<(), Box<dyn std::error::Error>>
{
    let state = MockState::default();
    let app = Router::new()
        .route("/api/v1/domains", get(list_domains_with_subdomain))
        .route(
            "/api/v1/domains/{zone}/dns-records",
            get(list_records_with_subdomain),
        )
        .route(
            "/api/v1/domains/{owner}/dns-records/{id}",
            patch(update_record).delete(delete_record),
        )
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let client = TimewebClient::new(
        Url::parse(&format!("http://{address}"))?,
        "test-token",
        Duration::from_secs(2),
    )?;
    let provider = Provider::new(
        client,
        DomainFilter::new(Vec::new(), Vec::new(), None, None),
    );

    let records = provider.records().await?;
    assert!(records.iter().any(|endpoint| {
        endpoint.dns_name == "grafana.example.com"
            && endpoint.record_type == "A"
            && endpoint.targets == ["192.0.2.1"]
    }));

    provider
        .apply_changes(Changes {
            create: Vec::new(),
            update_old: vec![endpoint("grafana.example.com", "A", "192.0.2.1", 600)],
            update_new: vec![endpoint("grafana.example.com", "A", "192.0.2.2", 300)],
            delete: Vec::new(),
        })
        .await?;

    let requests = state
        .requests
        .lock()
        .map_err(|_| "mock request lock was poisoned")?
        .clone();
    assert!(requests.iter().any(|request| {
        request.method == Method::GET
            && request.path == "/api/v1/domains/grafana.example.com/dns-records"
    }));
    assert!(requests.iter().any(|request| {
        request.method == Method::PATCH
            && request.path == "/api/v1/domains/grafana.example.com/dns-records/3"
            && request.body.as_ref().is_some_and(|body| {
                body == &json!({
                    "type":"A",
                    "value":"192.0.2.2",
                    "ttl":300
                })
            })
    }));

    server.abort();
    Ok(())
}

fn endpoint(dns_name: &str, record_type: &str, target: &str, ttl: i64) -> Endpoint {
    Endpoint {
        dns_name: dns_name.to_owned(),
        targets: vec![target.to_owned()],
        record_type: record_type.to_owned(),
        set_identifier: String::new(),
        record_ttl: ttl,
        labels: Default::default(),
        provider_specific: Vec::new(),
    }
}

async fn list_domains(State(state): State<MockState>, request: axum::extract::Request) -> Response {
    capture(&state, request).await;
    json_response(json!({
        "domains": [{"fqdn":"example.com"}],
        "meta": {"total": 1}
    }))
}

async fn list_domains_with_subdomain(
    State(state): State<MockState>,
    request: axum::extract::Request,
) -> Response {
    capture(&state, request).await;
    json_response(json!({
        "domains": [{
            "fqdn":"example.com",
            "subdomains":[{"fqdn":"grafana.example.com"}]
        }],
        "meta": {"total": 1}
    }))
}

async fn list_records(
    State(state): State<MockState>,
    Path(zone): Path<String>,
    request: axum::extract::Request,
) -> Response {
    capture(&state, request).await;
    if zone != "example.com" {
        return StatusCode::NOT_FOUND.into_response();
    }
    json_response(json!({
        "dns_records": [
            {"id":1,"type":"A","data":{"subdomain":"www","value":"192.0.2.1"},"ttl":300},
            {"id":2,"type":"MX","data":{"subdomain":"mail","value":"mx.example.com","priority":10},"ttl":300}
        ],
        "meta": {"total": 2}
    }))
}

async fn list_records_with_subdomain(
    State(state): State<MockState>,
    Path(zone): Path<String>,
    request: axum::extract::Request,
) -> Response {
    capture(&state, request).await;
    match zone.as_str() {
        "example.com" => json_response(json!({
            "dns_records": [
                {"id":1,"type":"A","data":{"value":"192.0.2.10"},"ttl":300}
            ],
            "meta": {"total": 1}
        })),
        "grafana.example.com" => json_response(json!({
            "dns_records": [
                {
                    "id":3,
                    "type":"A",
                    "data":{"value":"192.0.2.1"},
                    "fqdn":"grafana.example.com",
                    "ttl":600
                }
            ],
            "meta": {"total": 1}
        })),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn create_record(
    State(state): State<MockState>,
    Path(_owner): Path<String>,
    request: axum::extract::Request,
) -> Response {
    capture(&state, request).await;
    json_response(json!({"dns_record":{"id":3}}))
}

async fn update_record(
    State(state): State<MockState>,
    Path((_owner, _id)): Path<(String, String)>,
    request: axum::extract::Request,
) -> Response {
    capture(&state, request).await;
    json_response(json!({"dns_record":{"id":1}}))
}

async fn delete_record(
    State(state): State<MockState>,
    Path((_owner, _id)): Path<(String, String)>,
    request: axum::extract::Request,
) -> Response {
    capture(&state, request).await;
    StatusCode::NO_CONTENT.into_response()
}

async fn capture(state: &MockState, request: axum::extract::Request) {
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => Bytes::new(),
    };
    let body = if bytes.is_empty() {
        None
    } else {
        serde_json::from_slice(&bytes).ok()
    };
    if let Ok(mut requests) = state.requests.lock() {
        let authorization = match parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
        {
            Some(value) => value.to_owned(),
            None => String::new(),
        };
        requests.push(MockRequest {
            method: parts.method,
            path: parts.uri.path().to_owned(),
            authorization,
            body,
        });
    }
}

fn json_response(value: Value) -> Response {
    Json(value).into_response()
}
