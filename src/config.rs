use crate::{domain_filter::DomainFilter, error::ConfigError};
use std::{env, net::SocketAddr, time::Duration};
use url::Url;

const DEFAULT_API_URL: &str = "https://api.timeweb.cloud";
const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8888";
const DEFAULT_METRICS_ADDR: &str = "0.0.0.0:8080";
const DEFAULT_HTTP_TIMEOUT: &str = "10s";

#[derive(Clone)]
pub struct Config {
    api_url: Url,
    token: String,
    listen_addr: SocketAddr,
    metrics_addr: SocketAddr,
    http_timeout: Duration,
    domain_filter: DomainFilter,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let token = required_env("TIMEWEB_CLOUD_TOKEN")?;
        let api_url = parse_url("TIMEWEB_CLOUD_API_URL", DEFAULT_API_URL)?;
        let listen_addr = parse_socket_addr("TIMEWEB_CLOUD_LISTEN_ADDR", DEFAULT_LISTEN_ADDR)?;
        let metrics_addr = parse_socket_addr("TIMEWEB_CLOUD_METRICS_ADDR", DEFAULT_METRICS_ADDR)?;
        let http_timeout = parse_duration("TIMEWEB_CLOUD_HTTP_TIMEOUT", DEFAULT_HTTP_TIMEOUT)?;
        if http_timeout.is_zero() {
            return Err(ConfigError::InvalidEnvironmentVariable {
                name: "TIMEWEB_CLOUD_HTTP_TIMEOUT",
                value: "zero duration".to_owned(),
            });
        }

        let domain_filter = DomainFilter::from_environment(
            optional_env("DOMAIN_FILTER")?,
            optional_env("EXCLUDE_DOMAIN_FILTER")?,
            optional_env("REGEXP_DOMAIN_FILTER")?,
            optional_env("REGEXP_DOMAIN_FILTER_EXCLUSION")?,
        )?;

        Ok(Self {
            api_url,
            token,
            listen_addr,
            metrics_addr,
            http_timeout,
            domain_filter,
        })
    }

    pub fn api_url(&self) -> &Url {
        &self.api_url
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    pub fn metrics_addr(&self) -> SocketAddr {
        self.metrics_addr
    }

    pub fn http_timeout(&self) -> Duration {
        self.http_timeout
    }

    pub fn domain_filter(&self) -> &DomainFilter {
        &self.domain_filter
    }
}

fn required_env(name: &'static str) -> Result<String, ConfigError> {
    let value = match env::var(name) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => {
            return Err(ConfigError::MissingEnvironmentVariable { name });
        }
        Err(env::VarError::NotUnicode(_)) => {
            return Err(ConfigError::InvalidEnvironmentVariable {
                name,
                value: "value is not valid UTF-8".to_owned(),
            });
        }
    };
    if value.trim().is_empty() {
        return Err(ConfigError::EmptyEnvironmentVariable { name });
    }
    Ok(value)
}

fn parse_url(name: &'static str, default: &str) -> Result<Url, ConfigError> {
    let value = env_or_default(name, default)?;
    let url =
        Url::parse(value.trim()).map_err(|source| ConfigError::InvalidUrl { name, source })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError::InvalidEnvironmentVariable { name, value });
    }
    Ok(url)
}

fn parse_socket_addr(name: &'static str, default: &str) -> Result<SocketAddr, ConfigError> {
    let value = env_or_default(name, default)?;
    value
        .parse()
        .map_err(|_| ConfigError::InvalidEnvironmentVariable { name, value })
}

fn parse_duration(name: &'static str, default: &str) -> Result<Duration, ConfigError> {
    let value = env_or_default(name, default)?;
    humantime::parse_duration(value.trim())
        .map_err(|_| ConfigError::InvalidEnvironmentVariable { name, value })
}

fn optional_env(name: &'static str) -> Result<String, ConfigError> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Ok(String::new()),
        Err(env::VarError::NotUnicode(_)) => Err(ConfigError::InvalidEnvironmentVariable {
            name,
            value: "value is not valid UTF-8".to_owned(),
        }),
    }
}

fn env_or_default(name: &'static str, default: &str) -> Result<String, ConfigError> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Ok(default.to_owned()),
        Err(env::VarError::NotUnicode(_)) => Err(ConfigError::InvalidEnvironmentVariable {
            name,
            value: "value is not valid UTF-8".to_owned(),
        }),
    }
}
