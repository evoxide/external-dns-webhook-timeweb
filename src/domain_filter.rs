use crate::error::ConfigError;
use regex::Regex;
use serde::Serialize;

#[derive(Clone, Debug)]
pub struct DomainFilter {
    include: Vec<String>,
    exclude: Vec<String>,
    regex_include: Option<Regex>,
    regex_exclude: Option<Regex>,
}

#[derive(Serialize)]
struct DomainFilterPayload {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    include: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    exclude: Vec<String>,
    #[serde(rename = "regexInclude", skip_serializing_if = "Option::is_none")]
    regex_include: Option<String>,
    #[serde(rename = "regexExclude", skip_serializing_if = "Option::is_none")]
    regex_exclude: Option<String>,
}

impl DomainFilter {
    pub fn from_environment(
        include: String,
        exclude: String,
        regex_include: String,
        regex_exclude: String,
    ) -> Result<Self, ConfigError> {
        let regex_include = compile_regex("REGEXP_DOMAIN_FILTER", regex_include)?;
        let regex_exclude = compile_regex("REGEXP_DOMAIN_FILTER_EXCLUSION", regex_exclude)?;

        Ok(Self::new(
            split_values(&include),
            split_values(&exclude),
            regex_include,
            regex_exclude,
        ))
    }

    pub fn new(
        include: Vec<String>,
        exclude: Vec<String>,
        regex_include: Option<Regex>,
        regex_exclude: Option<Regex>,
    ) -> Self {
        Self {
            include: normalize_patterns(include),
            exclude: normalize_patterns(exclude),
            regex_include,
            regex_exclude,
        }
    }

    pub fn matches(&self, domain: &str) -> bool {
        let domain = normalize_domain(domain);
        if self.regex_include.is_some() || self.regex_exclude.is_some() {
            if self
                .regex_exclude
                .as_ref()
                .is_some_and(|regex| regex.is_match(&domain))
            {
                return false;
            }
            return match &self.regex_include {
                Some(regex) => regex.is_match(&domain),
                None => true,
            };
        }
        if self
            .exclude
            .iter()
            .any(|pattern| pattern_matches(pattern, &domain))
        {
            return false;
        }
        self.include.is_empty()
            || self
                .include
                .iter()
                .any(|pattern| pattern_matches(pattern, &domain))
    }

    pub fn zone_may_contain(&self, zone: &str) -> bool {
        if self.regex_include.is_some() || self.include.is_empty() {
            return true;
        }

        let zone = normalize_domain(zone);
        self.include.iter().any(|pattern| {
            let candidate = pattern.trim_start_matches('.');
            pattern_matches(pattern, &zone)
                || candidate == zone
                || candidate.ends_with(&format!(".{zone}"))
        })
    }

    pub fn is_unrestricted(&self) -> bool {
        self.include.is_empty()
            && self.exclude.is_empty()
            && self.regex_include.is_none()
            && self.regex_exclude.is_none()
    }
}

impl Serialize for DomainFilter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut include = self.include.clone();
        let mut exclude = self.exclude.clone();
        include.sort_unstable();
        exclude.sort_unstable();
        if self.regex_include.is_some() || self.regex_exclude.is_some() {
            include.clear();
            exclude.clear();
        }

        DomainFilterPayload {
            include,
            exclude,
            regex_include: self
                .regex_include
                .as_ref()
                .map(|regex| regex.as_str().to_owned()),
            regex_exclude: self
                .regex_exclude
                .as_ref()
                .map(|regex| regex.as_str().to_owned()),
        }
        .serialize(serializer)
    }
}

fn compile_regex(name: &'static str, value: String) -> Result<Option<Regex>, ConfigError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    Regex::new(value)
        .map(Some)
        .map_err(|source| ConfigError::InvalidRegex { name, source })
}

fn split_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn normalize_patterns(patterns: Vec<String>) -> Vec<String> {
    patterns
        .into_iter()
        .map(|pattern| {
            let trimmed = pattern.trim();
            trimmed.strip_prefix('.').map_or_else(
                || normalize_domain(trimmed),
                |suffix| format!(".{}", normalize_domain(suffix)),
            )
        })
        .filter(|pattern| !pattern.is_empty() && pattern != ".")
        .collect()
}

fn normalize_domain(domain: &str) -> String {
    domain.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn pattern_matches(pattern: &str, domain: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix('.') {
        return domain.ends_with(pattern) && domain != suffix;
    }
    domain == pattern || domain.ends_with(&format!(".{pattern}"))
}

#[cfg(test)]
mod tests {
    use super::DomainFilter;
    use regex::Regex;

    #[test]
    fn matches_include_exclude_and_regex_filters() -> Result<(), Box<dyn std::error::Error>> {
        let filter = DomainFilter::new(
            vec!["example.com".to_owned()],
            vec!["private.example.com".to_owned()],
            None,
            None,
        );

        assert!(filter.matches("www.example.com."));
        assert!(!filter.matches("private.example.com"));
        assert!(!filter.matches("other.example.org"));

        let regex_filter = DomainFilter::new(
            Vec::new(),
            Vec::new(),
            Some(Regex::new(r"^api\.example\.com$")?),
            None,
        );
        assert!(regex_filter.matches("api.example.com"));
        assert!(!regex_filter.matches("www.example.com"));
        Ok(())
    }

    #[test]
    fn serializes_using_external_dns_domain_filter_keys() -> Result<(), Box<dyn std::error::Error>>
    {
        let filter = DomainFilter::new(
            vec!["example.com".to_owned()],
            vec!["private.example.com".to_owned()],
            None,
            None,
        );

        let value = serde_json::to_value(filter)?;
        assert_eq!(value["include"][0], "example.com");
        assert_eq!(value["exclude"][0], "private.example.com");
        assert!(value.get("filters").is_none());
        Ok(())
    }

    #[test]
    fn regex_filter_overrides_domain_lists() -> Result<(), Box<dyn std::error::Error>> {
        let filter = DomainFilter::new(
            vec!["example.com".to_owned()],
            vec!["blocked.example.com".to_owned()],
            Some(Regex::new(r"^api\.example\.net$")?),
            None,
        );

        assert!(filter.matches("api.example.net"));
        assert!(!filter.matches("www.example.com"));
        let value = serde_json::to_value(filter)?;
        assert!(value.get("include").is_none());
        assert_eq!(value["regexInclude"], r"^api\.example\.net$");
        Ok(())
    }
}
