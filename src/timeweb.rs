use crate::model::Endpoint;
use futures_util::StreamExt;
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{collections::BTreeSet, fmt, time::Duration};
use thiserror::Error;
use url::Url;

const API_BODY_LIMIT: usize = 16 * 1024 * 1024;
const PAGE_SIZE: u64 = 100;

#[derive(Debug, Error)]
pub enum TimewebError {
    #[error("request to Timeweb Cloud failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Timeweb Cloud returned HTTP {status}: {message}")]
    HttpStatus { status: u16, message: String },
    #[error("failed to decode Timeweb Cloud response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("Timeweb Cloud response exceeded {API_BODY_LIMIT} bytes")]
    BodyTooLarge,
    #[error("failed to construct Timeweb Cloud URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("Timeweb Cloud response is invalid: {0}")]
    InvalidResponse(String),
}

impl TimewebError {
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Request(error) => error.is_timeout() || error.is_connect() || error.is_request(),
            Self::HttpStatus { status, .. } => *status == 429 || *status >= 500,
            Self::Decode(_) | Self::BodyTooLarge | Self::Url(_) | Self::InvalidResponse(_) => true,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Self::HttpStatus { status, .. } => Some(*status),
            Self::Request(_)
            | Self::Decode(_)
            | Self::BodyTooLarge
            | Self::Url(_)
            | Self::InvalidResponse(_) => None,
        }
    }
}

#[derive(Clone)]
pub struct TimewebClient {
    client: Client,
    base_url: Url,
    token: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DnsRecordChange {
    #[serde(rename = "type")]
    pub record_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdomain: Option<String>,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct TimewebDomain {
    pub fqdn: String,
}

#[derive(Clone, Debug)]
pub struct RemoteRecord {
    pub id: u64,
    pub zone: String,
    pub endpoint: Endpoint,
}

impl TimewebClient {
    pub fn new(base_url: Url, token: &str, timeout: Duration) -> Result<Self, TimewebError> {
        let client = Client::builder()
            .connect_timeout(timeout)
            .timeout(timeout)
            .user_agent("external-dns-webhook-timeweb")
            .build()?;

        Ok(Self {
            client,
            base_url,
            token: token.to_owned(),
        })
    }

    pub async fn list_domains(&self) -> Result<Vec<TimewebDomain>, TimewebError> {
        let mut offset = 0_u64;
        let mut domains = BTreeSet::new();

        loop {
            let url = self.api_url("/api/v1/domains")?;
            let url = with_pagination(url, offset);
            let response: DomainsResponse = self.get_json(url).await?;
            let page_len = response.domains.len() as u64;
            for domain in response.domains {
                let fqdn = normalize_name(&domain.fqdn);
                if fqdn.is_empty() {
                    continue;
                }
                domains.insert(fqdn.clone());
                for subdomain in domain.subdomains.unwrap_or_default() {
                    let subdomain_fqdn = normalize_name(&subdomain.fqdn);
                    if subdomain_fqdn.is_empty() {
                        continue;
                    }
                    if subdomain_fqdn != fqdn && !subdomain_fqdn.ends_with(&format!(".{fqdn}")) {
                        return Err(TimewebError::InvalidResponse(format!(
                            "subdomain {subdomain_fqdn} does not belong to domain {fqdn}"
                        )));
                    }
                    domains.insert(subdomain_fqdn);
                }
            }

            if should_stop_pagination(
                page_len,
                offset,
                response.meta.as_ref().and_then(|meta| meta.total),
            ) {
                break;
            }
            offset = offset.checked_add(page_len).ok_or_else(|| {
                TimewebError::InvalidResponse("domain pagination overflow".to_owned())
            })?;
        }

        Ok(domains
            .into_iter()
            .map(|fqdn| TimewebDomain { fqdn })
            .collect())
    }

    pub async fn list_zone_records(&self, zone: &str) -> Result<Vec<RemoteRecord>, TimewebError> {
        let zone = normalize_name(zone);
        if zone.is_empty() {
            return Err(TimewebError::InvalidResponse(
                "an empty zone cannot be queried".to_owned(),
            ));
        }

        let mut offset = 0_u64;
        let mut records = Vec::new();
        loop {
            let url = self.api_url(&format!("/api/v1/domains/{zone}/dns-records"))?;
            let url = with_pagination(url, offset);
            let response: DnsRecordsResponse = self.get_json(url).await?;
            let page_len = response.dns_records.len() as u64;
            for record in response.dns_records {
                records.push(record.into_remote_record(&zone)?);
            }

            if should_stop_pagination(
                page_len,
                offset,
                response.meta.as_ref().and_then(|meta| meta.total),
            ) {
                break;
            }
            offset = offset.checked_add(page_len).ok_or_else(|| {
                TimewebError::InvalidResponse("DNS record pagination overflow".to_owned())
            })?;
        }
        Ok(records)
    }

    pub async fn list_zone_records_if_exists(
        &self,
        zone: &str,
    ) -> Result<Option<Vec<RemoteRecord>>, TimewebError> {
        match self.list_zone_records(zone).await {
            Ok(records) => Ok(Some(records)),
            Err(error) if error.status() == Some(404) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn create_record(
        &self,
        owner_fqdn: &str,
        change: &DnsRecordChange,
    ) -> Result<(), TimewebError> {
        let url = self.dns_record_url("POST", owner_fqdn, None)?;
        let _: Value = self.send_json(Method::POST, url, Some(change)).await?;
        Ok(())
    }

    pub async fn update_record(
        &self,
        owner_fqdn: &str,
        id: u64,
        change: &DnsRecordChange,
    ) -> Result<(), TimewebError> {
        let url = self.dns_record_url("PATCH", owner_fqdn, Some(id))?;
        let _: Value = self.send_json(Method::PATCH, url, Some(change)).await?;
        Ok(())
    }

    pub async fn delete_record(&self, owner_fqdn: &str, id: u64) -> Result<(), TimewebError> {
        let url = self.dns_record_url("DELETE", owner_fqdn, Some(id))?;
        self.send_empty(Method::DELETE, url).await
    }

    async fn get_json<T>(&self, url: Url) -> Result<T, TimewebError>
    where
        T: DeserializeOwned,
    {
        self.send_json(Method::GET, url, Option::<&Value>::None)
            .await
    }

    async fn send_json<T, B>(
        &self,
        method: Method,
        url: Url,
        body: Option<&B>,
    ) -> Result<T, TimewebError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let request = self.request(method, url, body);
        let response = request.send().await?;
        let status = response.status();
        let body = read_body(response).await?;
        if !status.is_success() {
            return Err(api_status_error(status, &body));
        }
        serde_json::from_slice(&body).map_err(TimewebError::Decode)
    }

    async fn send_empty(&self, method: Method, url: Url) -> Result<(), TimewebError> {
        let response = self.request::<Value>(method, url, None).send().await?;
        let status = response.status();
        let body = read_body(response).await?;
        if !status.is_success() {
            return Err(api_status_error(status, &body));
        }
        Ok(())
    }

    fn request<B>(&self, method: Method, url: Url, body: Option<&B>) -> RequestBuilder
    where
        B: Serialize + ?Sized,
    {
        let request = self
            .client
            .request(method, url)
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        match body {
            Some(body) => request.json(body),
            None => request,
        }
    }

    fn api_url(&self, path: &str) -> Result<Url, TimewebError> {
        let mut url = self.base_url.clone();
        let base_path = url.path().trim_end_matches('/');
        url.set_path(&format!("{base_path}{path}"));
        url.set_query(None);
        url.set_fragment(None);
        Ok(url)
    }

    fn dns_record_url(
        &self,
        method: &str,
        owner_fqdn: &str,
        record_id: Option<u64>,
    ) -> Result<Url, TimewebError> {
        let owner_fqdn = normalize_name(owner_fqdn);
        if owner_fqdn.is_empty() {
            return Err(TimewebError::InvalidResponse(format!(
                "cannot use an empty owner FQDN for {method}"
            )));
        }
        let path = match record_id {
            Some(id) => format!("/api/v1/domains/{owner_fqdn}/dns-records/{id}"),
            None => format!("/api/v1/domains/{owner_fqdn}/dns-records"),
        };
        self.api_url(&path)
    }
}

#[derive(Debug, Deserialize)]
struct DomainsResponse {
    #[serde(default)]
    domains: Vec<DomainResponse>,
    meta: Option<MetaResponse>,
}

#[derive(Debug, Deserialize)]
struct DomainResponse {
    fqdn: String,
    subdomains: Option<Vec<SubdomainResponse>>,
}

#[derive(Debug, Deserialize)]
struct SubdomainResponse {
    fqdn: String,
}

#[derive(Debug, Deserialize)]
struct DnsRecordsResponse {
    #[serde(default)]
    dns_records: Vec<DnsRecordResponse>,
    meta: Option<MetaResponse>,
}

#[derive(Debug, Deserialize)]
struct MetaResponse {
    total: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DnsRecordResponse {
    id: Option<u64>,
    #[serde(rename = "type")]
    record_type: String,
    fqdn: Option<String>,
    #[serde(default)]
    data: DnsRecordData,
    ttl: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct DnsRecordData {
    priority: Option<u16>,
    subdomain: Option<String>,
    value: Option<String>,
}

impl DnsRecordResponse {
    fn into_remote_record(self, zone: &str) -> Result<RemoteRecord, TimewebError> {
        let id = self.id.filter(|id| *id > 0).ok_or_else(|| {
            TimewebError::InvalidResponse("DNS record has no valid ID".to_owned())
        })?;
        let record_type = self.record_type.trim().to_ascii_uppercase();
        let target = response_target(&record_type, &self.data)?;
        let dns_name = self
            .fqdn
            .as_deref()
            .map(|fqdn| owner_name(zone, Some(fqdn)))
            .unwrap_or_else(|| owner_name(zone, self.data.subdomain.as_deref()));
        let record_ttl = match self.ttl.map(i64::try_from).transpose() {
            Ok(Some(ttl)) => ttl,
            Ok(None) => 0,
            Err(_) => {
                return Err(TimewebError::InvalidResponse(
                    "DNS record TTL does not fit in int64".to_owned(),
                ));
            }
        };
        let endpoint = Endpoint {
            dns_name,
            targets: vec![target],
            record_type,
            set_identifier: String::new(),
            record_ttl,
            labels: Default::default(),
            provider_specific: Vec::new(),
        };
        Ok(RemoteRecord {
            id,
            zone: zone.to_owned(),
            endpoint,
        })
    }
}

fn response_target(record_type: &str, data: &DnsRecordData) -> Result<String, TimewebError> {
    match record_type {
        "A" | "AAAA" | "TXT" | "CNAME" => data
            .value
            .as_deref()
            .map(|value| normalize_target(record_type, value))
            .ok_or_else(|| missing_record_field(record_type, "value")),
        "MX" => {
            let priority = data
                .priority
                .ok_or_else(|| missing_record_field(record_type, "priority"))?;
            let value = data
                .value
                .as_deref()
                .ok_or_else(|| missing_record_field(record_type, "value"))?;
            Ok(format!(
                "{priority} {}",
                normalize_target(record_type, value)
            ))
        }
        "SRV" => {
            let priority = data
                .priority
                .ok_or_else(|| missing_record_field(record_type, "priority"))?;
            let value = data
                .value
                .as_deref()
                .ok_or_else(|| missing_record_field(record_type, "value"))?;
            let (weight, port, host) = parse_srv_value(value)?;
            Ok(format!(
                "{priority} {weight} {port} {}",
                normalize_target(record_type, &host)
            ))
        }
        other => Err(TimewebError::InvalidResponse(format!(
            "unsupported DNS record type {other}"
        ))),
    }
}

fn parse_srv_value(value: &str) -> Result<(u16, u16, String), TimewebError> {
    let mut parts = value.split_whitespace();
    let weight = parts
        .next()
        .ok_or_else(|| missing_record_field("SRV", "weight"))?
        .parse::<u16>()
        .map_err(|_| {
            TimewebError::InvalidResponse("SRV DNS record has invalid weight".to_owned())
        })?;
    let port = parts
        .next()
        .ok_or_else(|| missing_record_field("SRV", "port"))?
        .parse::<u16>()
        .map_err(|_| TimewebError::InvalidResponse("SRV DNS record has invalid port".to_owned()))?;
    let host = parts
        .next()
        .ok_or_else(|| missing_record_field("SRV", "host"))?;
    if parts.next().is_some() {
        return Err(TimewebError::InvalidResponse(
            "SRV DNS record value has extra fields".to_owned(),
        ));
    }
    Ok((weight, port, host.trim_end_matches('.').to_owned()))
}

fn missing_record_field(record_type: &str, field: &str) -> TimewebError {
    TimewebError::InvalidResponse(format!("{record_type} DNS record has no {field} field"))
}

fn owner_name(zone: &str, subdomain: Option<&str>) -> String {
    let zone = normalize_name(zone);
    let Some(subdomain) = subdomain.map(str::trim).filter(|value| !value.is_empty()) else {
        return zone;
    };
    if subdomain == "@" {
        return zone;
    }
    let subdomain = normalize_name(subdomain);
    if subdomain == zone || subdomain.ends_with(&format!(".{zone}")) {
        subdomain
    } else {
        format!("{subdomain}.{zone}")
    }
}

fn normalize_name(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn normalize_target(record_type: &str, value: &str) -> String {
    if record_type == "TXT" {
        value.to_owned()
    } else {
        value.trim_end_matches('.').to_owned()
    }
}

fn with_pagination(mut url: Url, offset: u64) -> Url {
    url.query_pairs_mut()
        .append_pair("limit", &PAGE_SIZE.to_string())
        .append_pair("offset", &offset.to_string());
    url
}

fn should_stop_pagination(page_len: u64, offset: u64, total: Option<u64>) -> bool {
    if page_len == 0 || page_len < PAGE_SIZE {
        return true;
    }
    total.is_some_and(|total| offset.saturating_add(page_len) >= total)
}

async fn read_body(response: Response) -> Result<Vec<u8>, TimewebError> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > API_BODY_LIMIT {
            return Err(TimewebError::BodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn api_status_error(status: StatusCode, body: &[u8]) -> TimewebError {
    let message = match serde_json::from_slice::<ApiErrorResponse>(body)
        .ok()
        .and_then(|error| error.message)
    {
        Some(message) if !message.to_string().is_empty() => message.to_string(),
        Some(_) | None => {
            let text = String::from_utf8_lossy(body).trim().to_owned();
            if text.is_empty() {
                "empty response body".to_owned()
            } else {
                truncate_message(&text)
            }
        }
    };
    TimewebError::HttpStatus {
        status: status.as_u16(),
        message,
    }
}

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    message: Option<ApiErrorMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ApiErrorMessage {
    Text(String),
    List(Vec<String>),
}

impl fmt::Display for ApiErrorMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(message) => formatter.write_str(&truncate_message(message)),
            Self::List(messages) => formatter.write_str(&truncate_message(&messages.join("; "))),
        }
    }
}

fn truncate_message(message: &str) -> String {
    message.chars().take(1024).collect()
}

#[cfg(test)]
mod tests {
    use super::{DnsRecordData, DnsRecordResponse, owner_name, response_target};

    #[test]
    fn builds_full_owner_name_from_relative_and_absolute_subdomains() {
        assert_eq!(owner_name("example.com", None), "example.com");
        assert_eq!(owner_name("example.com", Some("www")), "www.example.com");
        assert_eq!(
            owner_name("example.com", Some("www.example.com.")),
            "www.example.com"
        );
    }

    #[test]
    fn converts_mx_response_to_external_dns_target() -> Result<(), Box<dyn std::error::Error>> {
        let data = DnsRecordData {
            priority: Some(10),
            value: Some("mail.example.com.".to_owned()),
            ..Default::default()
        };
        assert_eq!(response_target("MX", &data)?, "10 mail.example.com");
        Ok(())
    }

    #[test]
    fn restores_srv_owner_name_from_v1_response() -> Result<(), Box<dyn std::error::Error>> {
        let response = DnsRecordResponse {
            id: Some(7),
            record_type: "SRV".to_owned(),
            fqdn: None,
            data: DnsRecordData {
                subdomain: Some("_sip._tcp.sub.example.com".to_owned()),
                priority: Some(10),
                value: Some("0 993 mail.example.com.".to_owned()),
            },
            ttl: Some(300),
        };

        let record = response.into_remote_record("example.com")?;
        assert_eq!(record.endpoint.dns_name, "_sip._tcp.sub.example.com");
        assert_eq!(record.endpoint.targets, ["10 0 993 mail.example.com"]);
        Ok(())
    }

    #[test]
    fn restores_owner_from_subdomain_response_fqdn() -> Result<(), Box<dyn std::error::Error>> {
        let response = DnsRecordResponse {
            id: Some(91579517),
            record_type: "A".to_owned(),
            fqdn: Some("grafana.example.com".to_owned()),
            data: DnsRecordData {
                value: Some("192.0.2.1".to_owned()),
                ..Default::default()
            },
            ttl: Some(600),
        };

        let record = response.into_remote_record("grafana.example.com")?;
        assert_eq!(record.endpoint.dns_name, "grafana.example.com");
        assert_eq!(record.endpoint.targets, ["192.0.2.1"]);
        assert_eq!(record.zone, "grafana.example.com");
        Ok(())
    }
}
