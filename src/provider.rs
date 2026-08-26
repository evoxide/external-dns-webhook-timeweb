use crate::{
    domain_filter::DomainFilter,
    model::{Changes, Endpoint},
    timeweb::{DnsRecordChange, RemoteRecord, TimewebClient, TimewebError},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    net::IpAddr,
    sync::Arc,
};
use thiserror::Error;
use tokio::sync::RwLock;

const SUPPORTED_RECORD_TYPES: &[&str] = &["A", "AAAA", "TXT", "CNAME", "MX", "SRV"];

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("invalid ExternalDNS endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("ExternalDNS update lists have different lengths: {old} and {new}")]
    MismatchedUpdateLists { old: usize, new: usize },
    #[error("current DNS state does not contain record {record_type} {dns_name} {target}")]
    StateMismatch {
        dns_name: String,
        record_type: String,
        target: String,
    },
    #[error(transparent)]
    Timeweb(#[from] TimewebError),
}

impl ProviderError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidEndpoint(_) | Self::MismatchedUpdateLists { .. } => 400,
            Self::StateMismatch { .. } => 503,
            Self::Timeweb(error) => match error.status() {
                Some(status) if (400..500).contains(&status) && status != 429 => status,
                _ => 502,
            },
        }
    }
}

#[derive(Clone)]
pub struct Provider {
    client: TimewebClient,
    domain_filter: DomainFilter,
    discovered_zones: Arc<RwLock<BTreeSet<String>>>,
}

struct RemoteState {
    zones: Vec<String>,
    records: Vec<RemoteRecord>,
}

impl Provider {
    pub fn new(client: TimewebClient, domain_filter: DomainFilter) -> Self {
        Self {
            client,
            domain_filter,
            discovered_zones: Arc::new(RwLock::new(BTreeSet::new())),
        }
    }

    pub fn domain_filter(&self) -> &DomainFilter {
        &self.domain_filter
    }

    pub async fn records(&self) -> Result<Vec<Endpoint>, ProviderError> {
        let state = self.load_state().await?;
        Ok(group_records(state.records))
    }

    pub fn adjust_endpoints(&self, endpoints: Vec<Endpoint>) -> Vec<Endpoint> {
        endpoints
    }

    pub async fn apply_changes(&self, changes: Changes) -> Result<(), ProviderError> {
        self.validate_changes(&changes)?;
        if changes.is_empty() {
            return Ok(());
        }

        let mut state = self.load_state().await?;
        self.discover_change_zones(&changes, &mut state).await?;
        let mut deleted_ids = HashSet::new();

        for endpoint in &changes.delete {
            self.apply_delete(endpoint, &state, &mut deleted_ids)
                .await?;
        }

        for (old, new) in changes.update_old.iter().zip(changes.update_new.iter()) {
            self.apply_update(old, new, &state, &mut deleted_ids)
                .await?;
        }

        for endpoint in &changes.create {
            self.apply_create(endpoint, &state).await?;
        }

        Ok(())
    }

    async fn apply_create(
        &self,
        endpoint: &Endpoint,
        state: &RemoteState,
    ) -> Result<(), ProviderError> {
        let zone = find_record_zone(&state.zones, &endpoint.dns_name).ok_or_else(|| {
            ProviderError::InvalidEndpoint(format!(
                "no Timeweb Cloud domain contains {}",
                endpoint.dns_name
            ))
        })?;
        for target in &endpoint.targets {
            if state.records.iter().any(|record| {
                same_record_identity(&record.endpoint, endpoint)
                    && record.endpoint.targets.first().is_some_and(|current| {
                        canonical_target(&endpoint.record_type, current)
                            == canonical_target(&endpoint.record_type, target)
                    })
            }) {
                continue;
            }
            let change = change_from_endpoint(endpoint, target, zone)?;
            self.client.create_record(zone, &change).await?;
        }
        Ok(())
    }

    async fn apply_delete(
        &self,
        endpoint: &Endpoint,
        state: &RemoteState,
        deleted_ids: &mut HashSet<u64>,
    ) -> Result<(), ProviderError> {
        let targets = endpoint
            .targets
            .iter()
            .map(|target| canonical_target(&endpoint.record_type, target))
            .collect::<HashSet<_>>();
        let records = state.records.iter().filter(|record| {
            same_record_identity(&record.endpoint, endpoint)
                && (targets.is_empty()
                    || record.endpoint.targets.first().is_some_and(|target| {
                        targets.contains(&canonical_target(&endpoint.record_type, target))
                    }))
        });

        for record in records {
            if deleted_ids.insert(record.id) {
                self.client.delete_record(&record.zone, record.id).await?;
            }
        }
        Ok(())
    }

    async fn apply_update(
        &self,
        old: &Endpoint,
        new: &Endpoint,
        state: &RemoteState,
        deleted_ids: &mut HashSet<u64>,
    ) -> Result<(), ProviderError> {
        if !same_record_identity(old, new) {
            return Err(ProviderError::InvalidEndpoint(
                "updateOld and updateNew must have the same DNS name, type and set identifier"
                    .to_owned(),
            ));
        }
        let zone = find_record_zone(&state.zones, &old.dns_name).ok_or_else(|| {
            ProviderError::InvalidEndpoint(format!(
                "no Timeweb Cloud domain contains {}",
                old.dns_name
            ))
        })?;

        let mut old_records = Vec::new();
        let mut used_ids = HashSet::new();
        for old_target in &old.targets {
            let canonical_old_target = canonical_target(&old.record_type, old_target);
            let record = state
                .records
                .iter()
                .find(|record| {
                    !used_ids.contains(&record.id)
                        && !deleted_ids.contains(&record.id)
                        && same_record_identity(&record.endpoint, old)
                        && record.endpoint.targets.first().is_some_and(|target| {
                            canonical_target(&old.record_type, target) == canonical_old_target
                        })
                })
                .ok_or_else(|| ProviderError::StateMismatch {
                    dns_name: old.dns_name.clone(),
                    record_type: old.record_type.clone(),
                    target: old_target.clone(),
                })?;
            used_ids.insert(record.id);
            old_records.push(record.clone());
        }

        let mut new_targets = new.targets.clone();
        let mut retained = Vec::new();
        let mut changed = Vec::new();
        for old_record in old_records {
            let old_target = old_record
                .endpoint
                .targets
                .first()
                .cloned()
                .ok_or_else(|| ProviderError::StateMismatch {
                    dns_name: old.dns_name.clone(),
                    record_type: old.record_type.clone(),
                    target: String::new(),
                })?;
            let old_key = canonical_target(&old.record_type, &old_target);
            if let Some(position) = new_targets
                .iter()
                .position(|target| canonical_target(&new.record_type, target) == old_key)
            {
                let target = new_targets.remove(position);
                retained.push((old_record, target));
            } else {
                changed.push(old_record);
            }
        }

        for (record, target) in retained {
            if ttl_changed(&record.endpoint, new) {
                let change = change_from_endpoint(new, &target, zone)?;
                self.client
                    .update_record(&record.zone, record.id, &change)
                    .await?;
            }
        }

        let changed_count = changed.len().min(new_targets.len());
        for index in 0..changed_count {
            let record = &changed[index];
            let target = &new_targets[index];
            let change = change_from_endpoint(new, target, zone)?;
            self.client
                .update_record(&record.zone, record.id, &change)
                .await?;
        }

        if changed_count < new_targets.len() {
            for target in new_targets.iter().skip(changed_count) {
                let change = change_from_endpoint(new, target, zone)?;
                self.client.create_record(zone, &change).await?;
            }
        }

        if changed_count < changed.len() {
            for record in changed.iter().skip(changed_count) {
                if deleted_ids.insert(record.id) {
                    self.client.delete_record(&record.zone, record.id).await?;
                }
            }
        }

        Ok(())
    }

    async fn load_state(&self) -> Result<RemoteState, ProviderError> {
        let mut domain_names = self
            .client
            .list_domains()
            .await?
            .into_iter()
            .map(|domain| domain.fqdn)
            .collect::<BTreeSet<_>>();
        domain_names.extend(self.discovered_zones.read().await.iter().cloned());

        let mut zones = Vec::new();
        let mut records = Vec::new();
        for domain_name in domain_names {
            if !self.domain_filter.zone_may_contain(&domain_name) {
                continue;
            }
            zones.push(domain_name.clone());
            for record in self.client.list_zone_records(&domain_name).await? {
                if self.domain_filter.matches(&record.endpoint.dns_name) {
                    records.push(record);
                }
            }
        }
        Ok(RemoteState { zones, records })
    }

    async fn discover_change_zones(
        &self,
        changes: &Changes,
        state: &mut RemoteState,
    ) -> Result<(), ProviderError> {
        let endpoints = changes
            .create
            .iter()
            .chain(&changes.delete)
            .chain(&changes.update_old)
            .chain(&changes.update_new);
        let names = endpoints
            .map(|endpoint| normalize_name(&endpoint.dns_name))
            .filter(|dns_name| {
                !dns_name.is_empty()
                    && self.domain_filter.matches(dns_name)
                    && find_zone(&state.zones, dns_name).is_some()
            })
            .collect::<BTreeSet<_>>();

        for dns_name in names {
            if state
                .zones
                .iter()
                .any(|zone| zone.eq_ignore_ascii_case(&dns_name))
            {
                continue;
            }

            let Some(records) = self.client.list_zone_records_if_exists(&dns_name).await? else {
                continue;
            };

            state.zones.push(dns_name.clone());
            state.records.extend(
                records
                    .into_iter()
                    .filter(|record| self.domain_filter.matches(&record.endpoint.dns_name)),
            );
            self.discovered_zones.write().await.insert(dns_name);
        }
        Ok(())
    }

    fn validate_changes(&self, changes: &Changes) -> Result<(), ProviderError> {
        if changes.update_old.len() != changes.update_new.len() {
            return Err(ProviderError::MismatchedUpdateLists {
                old: changes.update_old.len(),
                new: changes.update_new.len(),
            });
        }
        for endpoint in &changes.create {
            validate_endpoint(endpoint, true)?;
        }
        for endpoint in &changes.delete {
            validate_endpoint(endpoint, false)?;
        }
        for endpoint in &changes.update_old {
            validate_endpoint(endpoint, true)?;
        }
        for endpoint in &changes.update_new {
            validate_endpoint(endpoint, true)?;
        }
        Ok(())
    }
}

fn group_records(records: Vec<RemoteRecord>) -> Vec<Endpoint> {
    let mut grouped: BTreeMap<(String, String), Endpoint> = BTreeMap::new();
    for record in records {
        let key = (
            record.endpoint.dns_name.clone(),
            record.endpoint.record_type.clone(),
        );
        if let Some(endpoint) = grouped.get_mut(&key) {
            if let Some(target) = record.endpoint.targets.first()
                && !endpoint.targets.iter().any(|current| current == target)
            {
                endpoint.targets.push(target.clone());
            }
            if endpoint.record_ttl != record.endpoint.record_ttl {
                endpoint.record_ttl = 0;
            }
        } else {
            grouped.insert(key, record.endpoint);
        }
    }
    let mut endpoints = grouped.into_values().collect::<Vec<_>>();
    for endpoint in &mut endpoints {
        endpoint.targets.sort_unstable();
    }
    endpoints
}

fn validate_endpoint(endpoint: &Endpoint, require_targets: bool) -> Result<(), ProviderError> {
    if endpoint.dns_name.trim().is_empty() {
        return Err(ProviderError::InvalidEndpoint(
            "dnsName must not be empty".to_owned(),
        ));
    }
    let record_type = endpoint.record_type.trim().to_ascii_uppercase();
    if !SUPPORTED_RECORD_TYPES.contains(&record_type.as_str()) {
        return Err(ProviderError::InvalidEndpoint(format!(
            "record type {record_type} is not supported by Timeweb Cloud"
        )));
    }
    if !endpoint.set_identifier.is_empty() {
        return Err(ProviderError::InvalidEndpoint(
            "setIdentifier is not supported by Timeweb Cloud".to_owned(),
        ));
    }
    if endpoint.record_ttl < 0 {
        return Err(ProviderError::InvalidEndpoint(
            "recordTTL must not be negative".to_owned(),
        ));
    }
    if require_targets && endpoint.targets.is_empty() {
        return Err(ProviderError::InvalidEndpoint(
            "targets must not be empty".to_owned(),
        ));
    }
    if endpoint.targets.iter().any(|target| target.is_empty()) {
        return Err(ProviderError::InvalidEndpoint(
            "targets must not contain empty values".to_owned(),
        ));
    }
    Ok(())
}

fn same_record_identity(left: &Endpoint, right: &Endpoint) -> bool {
    left.dns_name.eq_ignore_ascii_case(&right.dns_name)
        && left.record_type.eq_ignore_ascii_case(&right.record_type)
        && left.set_identifier == right.set_identifier
}

fn ttl_changed(current: &Endpoint, desired: &Endpoint) -> bool {
    desired.record_ttl > 0 && current.record_ttl != desired.record_ttl
}

fn find_zone<'a>(zones: &'a [String], dns_name: &str) -> Option<&'a str> {
    let dns_name = normalize_name(dns_name);
    zones
        .iter()
        .filter(|zone| {
            let zone = normalize_name(zone);
            dns_name == zone || dns_name.ends_with(&format!(".{zone}"))
        })
        .max_by_key(|zone| zone.len())
        .map(String::as_str)
}

fn find_record_zone<'a>(zones: &'a [String], dns_name: &str) -> Option<&'a str> {
    find_zone(zones, dns_name)
}

fn change_from_endpoint(
    endpoint: &Endpoint,
    target: &str,
    zone: &str,
) -> Result<DnsRecordChange, ProviderError> {
    let record_type = endpoint.record_type.trim().to_ascii_uppercase();
    let ttl = u64::try_from(endpoint.record_ttl)
        .ok()
        .filter(|ttl| *ttl > 0);
    let subdomain = subdomain_for(&endpoint.dns_name, zone);
    let change = match record_type.as_str() {
        "A" => {
            let address = target.parse::<IpAddr>().map_err(|_| {
                ProviderError::InvalidEndpoint(format!(
                    "A record target is not an IP address: {target}"
                ))
            })?;
            if !address.is_ipv4() {
                return Err(ProviderError::InvalidEndpoint(format!(
                    "A record target is not an IPv4 address: {target}"
                )));
            }
            DnsRecordChange {
                record_type,
                subdomain: subdomain.clone(),
                value: target.to_owned(),
                ttl,
                priority: None,
            }
        }
        "AAAA" => {
            let address = target.parse::<IpAddr>().map_err(|_| {
                ProviderError::InvalidEndpoint(format!(
                    "AAAA record target is not an IP address: {target}"
                ))
            })?;
            if !address.is_ipv6() {
                return Err(ProviderError::InvalidEndpoint(format!(
                    "AAAA record target is not an IPv6 address: {target}"
                )));
            }
            DnsRecordChange {
                record_type,
                subdomain: subdomain.clone(),
                value: target.to_owned(),
                ttl,
                priority: None,
            }
        }
        "TXT" | "CNAME" => {
            let value = if record_type == "TXT" {
                target.to_owned()
            } else {
                target.trim_end_matches('.').to_owned()
            };
            DnsRecordChange {
                record_type,
                subdomain: subdomain.clone(),
                value,
                ttl,
                priority: None,
            }
        }
        "MX" => {
            let (priority, host) = parse_mx_target(target)?;
            DnsRecordChange {
                record_type,
                subdomain: subdomain.clone(),
                value: host,
                ttl,
                priority: Some(priority),
            }
        }
        "SRV" => {
            let (priority, weight, port, host) = parse_srv_target(target)?;
            validate_srv_owner(&endpoint.dns_name, zone)?;
            DnsRecordChange {
                record_type,
                subdomain,
                value: format!("{weight} {port} {host}"),
                ttl,
                priority: Some(priority),
            }
        }
        _ => {
            return Err(ProviderError::InvalidEndpoint(format!(
                "record type {record_type} is not supported by Timeweb Cloud"
            )));
        }
    };
    Ok(change)
}

fn subdomain_for(dns_name: &str, zone: &str) -> Option<String> {
    let dns_name = normalize_name(dns_name);
    let zone = normalize_name(zone);
    if dns_name == zone {
        return None;
    }
    dns_name
        .strip_suffix(&format!(".{zone}"))
        .filter(|subdomain| !subdomain.is_empty())
        .map(str::to_owned)
        .or(Some(dns_name))
}

fn parse_mx_target(target: &str) -> Result<(u16, String), ProviderError> {
    let mut parts = target.split_whitespace();
    let priority = parts
        .next()
        .ok_or_else(|| ProviderError::InvalidEndpoint(format!("invalid MX target: {target}")))?
        .parse::<u16>()
        .map_err(|_| {
            ProviderError::InvalidEndpoint(format!("invalid MX priority in target: {target}"))
        })?;
    let host = parts
        .next()
        .ok_or_else(|| ProviderError::InvalidEndpoint(format!("invalid MX target: {target}")))?;
    if parts.next().is_some() {
        return Err(ProviderError::InvalidEndpoint(format!(
            "invalid MX target: {target}"
        )));
    }
    Ok((priority, host.trim_end_matches('.').to_owned()))
}

fn parse_srv_target(target: &str) -> Result<(u16, u16, u16, String), ProviderError> {
    let mut parts = target.split_whitespace();
    let priority = parse_srv_number(parts.next(), target, "priority")?;
    let weight = parse_srv_number(parts.next(), target, "weight")?;
    let port = parse_srv_number(parts.next(), target, "port")?;
    let host = parts
        .next()
        .ok_or_else(|| ProviderError::InvalidEndpoint(format!("invalid SRV target: {target}")))?;
    if parts.next().is_some() {
        return Err(ProviderError::InvalidEndpoint(format!(
            "invalid SRV target: {target}"
        )));
    }
    Ok((
        priority,
        weight,
        port,
        host.trim_end_matches('.').to_owned(),
    ))
}

fn parse_srv_number(value: Option<&str>, target: &str, field: &str) -> Result<u16, ProviderError> {
    value
        .ok_or_else(|| ProviderError::InvalidEndpoint(format!("invalid SRV target: {target}")))?
        .parse::<u16>()
        .map_err(|_| {
            ProviderError::InvalidEndpoint(format!("invalid SRV {field} in target: {target}"))
        })
}

fn validate_srv_owner(dns_name: &str, zone: &str) -> Result<(), ProviderError> {
    let dns_name = normalize_name(dns_name);
    let zone = normalize_name(zone);
    let relative = match dns_name.strip_suffix(&format!(".{zone}")) {
        Some(relative) => relative,
        None if dns_name == zone => "",
        None => &dns_name,
    };
    let mut labels = relative.split('.');
    let service = labels.next().map_or("", |value| value);
    let protocol = labels.next().map_or("", |value| value);
    if !service.starts_with('_') || !protocol.starts_with('_') {
        return Err(ProviderError::InvalidEndpoint(format!(
            "SRV DNS name must start with service and protocol labels: {dns_name}"
        )));
    }
    Ok(())
}

fn canonical_target(record_type: &str, target: &str) -> String {
    let record_type = record_type.to_ascii_uppercase();
    if record_type == "TXT" {
        target.to_owned()
    } else if record_type == "MX" {
        let mut parts = target.split_whitespace();
        let priority = parts.next().map_or("", |value| value);
        let host = parts.next().map_or("", |value| value);
        format!("{priority} {}", host.trim_end_matches('.')).to_ascii_lowercase()
    } else if record_type == "SRV" {
        let mut parts = target.split_whitespace();
        let priority = parts.next().map_or("", |value| value);
        let weight = parts.next().map_or("", |value| value);
        let port = parts.next().map_or("", |value| value);
        let host = parts.next().map_or("", |value| value);
        format!("{priority} {weight} {port} {}", host.trim_end_matches('.')).to_ascii_lowercase()
    } else {
        target.trim_end_matches('.').to_ascii_lowercase()
    }
}

fn normalize_name(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_target, change_from_endpoint, find_record_zone, group_records, parse_mx_target,
        parse_srv_target,
    };
    use crate::{model::Endpoint, timeweb::RemoteRecord};

    fn endpoint(record_type: &str, dns_name: &str, targets: &[&str]) -> Endpoint {
        Endpoint {
            dns_name: dns_name.to_owned(),
            targets: targets.iter().map(|target| (*target).to_owned()).collect(),
            record_type: record_type.to_owned(),
            set_identifier: String::new(),
            record_ttl: 300,
            labels: Default::default(),
            provider_specific: Vec::new(),
        }
    }

    #[test]
    fn groups_remote_records_without_losing_distinct_targets() {
        let first = endpoint("A", "www.example.com", &["192.0.2.1"]);
        let second = endpoint("A", "www.example.com", &["192.0.2.2"]);
        let records = group_records(vec![
            RemoteRecord {
                id: 1,
                zone: "example.com".to_owned(),
                endpoint: first,
            },
            RemoteRecord {
                id: 2,
                zone: "example.com".to_owned(),
                endpoint: second,
            },
        ]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].targets, ["192.0.2.1", "192.0.2.2"]);
    }

    #[test]
    fn parses_timeweb_record_target_formats() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            parse_mx_target("10 mail.example.com.")?,
            (10, "mail.example.com".to_owned())
        );
        assert_eq!(
            parse_srv_target("10 0 443 service.example.com.")?,
            (10, 0, 443, "service.example.com".to_owned())
        );
        assert_eq!(
            canonical_target("CNAME", "target.example.com."),
            "target.example.com"
        );
        Ok(())
    }

    #[test]
    fn creates_timeweb_mx_payload() -> Result<(), Box<dyn std::error::Error>> {
        let endpoint = endpoint("MX", "example.com", &["10 mail.example.com"]);
        let change = change_from_endpoint(&endpoint, &endpoint.targets[0], "example.com")?;
        let json = serde_json::to_value(change)?;
        assert_eq!(json["type"], "MX");
        assert_eq!(json["priority"], 10);
        assert_eq!(json["value"], "mail.example.com");
        assert!(json.get("subdomain").is_none());
        Ok(())
    }

    #[test]
    fn creates_relative_subdomain_payload() -> Result<(), Box<dyn std::error::Error>> {
        let endpoint = endpoint("A", "www.example.com", &["192.0.2.1"]);
        let change = change_from_endpoint(&endpoint, &endpoint.targets[0], "example.com")?;
        let json = serde_json::to_value(change)?;
        assert_eq!(json["subdomain"], "www");
        Ok(())
    }

    #[test]
    fn selects_the_most_specific_domain_for_records() {
        let zones = vec!["evoxide.ru".to_owned(), "grafana.evoxide.ru".to_owned()];
        assert_eq!(
            find_record_zone(&zones, "grafana.evoxide.ru"),
            Some("grafana.evoxide.ru")
        );
        assert_eq!(
            find_record_zone(&zones, "api.evoxide.ru"),
            Some("evoxide.ru")
        );
    }

    #[test]
    fn creates_timeweb_srv_payload() -> Result<(), Box<dyn std::error::Error>> {
        let endpoint = endpoint(
            "SRV",
            "_sip._tcp.example.com",
            &["10 5 443 service.example.com"],
        );
        let change = change_from_endpoint(&endpoint, &endpoint.targets[0], "example.com")?;
        let json = serde_json::to_value(change)?;
        assert_eq!(json["type"], "SRV");
        assert_eq!(json["subdomain"], "_sip._tcp");
        assert_eq!(json["priority"], 10);
        assert_eq!(json["value"], "5 443 service.example.com");
        Ok(())
    }
}
