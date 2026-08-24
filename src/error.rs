use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("environment variable {name} is required")]
    MissingEnvironmentVariable { name: &'static str },
    #[error("environment variable {name} must not be empty")]
    EmptyEnvironmentVariable { name: &'static str },
    #[error("invalid value for environment variable {name}: {value}")]
    InvalidEnvironmentVariable { name: &'static str, value: String },
    #[error("invalid URL in environment variable {name}: {source}")]
    InvalidUrl {
        name: &'static str,
        #[source]
        source: url::ParseError,
    },
    #[error("invalid regular expression in environment variable {name}: {source}")]
    InvalidRegex {
        name: &'static str,
        #[source]
        source: regex::Error,
    },
}
