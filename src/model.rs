use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Endpoint {
    #[serde(rename = "dnsName", skip_serializing_if = "String::is_empty")]
    pub dns_name: String,
    #[serde(
        default,
        deserialize_with = "deserialize_vec_or_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub targets: Vec<String>,
    #[serde(
        rename = "recordType",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub record_type: String,
    #[serde(
        rename = "setIdentifier",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub set_identifier: String,
    #[serde(rename = "recordTTL", default, skip_serializing_if = "is_zero")]
    pub record_ttl: i64,
    #[serde(
        default,
        deserialize_with = "deserialize_map_or_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub labels: BTreeMap<String, String>,
    #[serde(
        default,
        deserialize_with = "deserialize_vec_or_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub provider_specific: Vec<ProviderSpecificProperty>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderSpecificProperty {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct Changes {
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub create: Vec<Endpoint>,
    #[serde(
        rename = "updateOld",
        default,
        deserialize_with = "deserialize_vec_or_default"
    )]
    pub update_old: Vec<Endpoint>,
    #[serde(
        rename = "updateNew",
        default,
        deserialize_with = "deserialize_vec_or_default"
    )]
    pub update_new: Vec<Endpoint>,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub delete: Vec<Endpoint>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.create.is_empty()
            && self.update_old.is_empty()
            && self.update_new.is_empty()
            && self.delete.is_empty()
    }
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

fn deserialize_vec_or_default<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    match Option::<Vec<T>>::deserialize(deserializer)? {
        Some(values) => Ok(values),
        None => Ok(Vec::new()),
    }
}

fn deserialize_map_or_default<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
{
    match Option::<BTreeMap<K, V>>::deserialize(deserializer)? {
        Some(values) => Ok(values),
        None => Ok(BTreeMap::new()),
    }
}
