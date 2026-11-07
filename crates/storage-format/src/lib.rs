//! Versioned, bounded metadata. Every immutable reference carries SHA-256 and size.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use storage_namespace::RelativeKey;

pub const FORMAT_VERSION: u32 = 1;
pub const LAYOUT_VERSION: u32 = 1;
pub const MAX_METADATA_BYTES: usize = 1024 * 1024;
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObjectRef {
    pub key: String,
    pub size: u64,
    pub sha256: String,
}
impl ObjectRef {
    pub fn relative_key(&self) -> Result<RelativeKey, storage_namespace::InvalidPath> {
        RelativeKey::new(&self.key)
    }
    pub fn validate(&self) -> bool {
        self.relative_key().is_ok()
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    }
    pub fn matches(&self, bytes: &[u8]) -> bool {
        self.validate() && self.size == bytes.len() as u64 && self.sha256 == digest(bytes)
    }
}
/// A compressed radix node: prefix is the full shared key prefix. Values are
/// small typed references, never whole page maps or embedded result payloads.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexNode {
    pub version: u32,
    pub prefix: String,
    pub value: Option<Value>,
    pub children: BTreeMap<char, ObjectRef>,
}
impl IndexNode {
    pub fn validate(&self) -> bool {
        self.version == FORMAT_VERSION
            && self.prefix.len() <= 64
            && self.prefix.bytes().all(|b| b.is_ascii_hexdigit())
            && self.children.len() <= 16
            && self
                .children
                .iter()
                .all(|(c, r)| c.is_ascii_hexdigit() && r.validate())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub page_size: usize,
    pub logical_size: u64,
    pub segment_size: usize,
    pub generation_id: String,
    pub base_root: Option<ObjectRef>,
    pub delta_root: Option<ObjectRef>,
    pub overlay_bytes: u64,
    pub overlay_transactions: u64,
}
impl Manifest {
    pub fn validate(&self) -> bool {
        self.version == FORMAT_VERSION
            && (512..=65536).contains(&self.page_size)
            && self.page_size.is_power_of_two()
            && self.logical_size <= i64::MAX as u64
            && self.logical_size.is_multiple_of(self.page_size as u64)
            && self.segment_size >= self.page_size
            && self.segment_size <= 1024 * 1024
            && self.segment_size.is_multiple_of(self.page_size)
            && self.base_root.as_ref().is_none_or(ObjectRef::validate)
            && self.delta_root.as_ref().is_none_or(ObjectRef::validate)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageLocation {
    pub object: ObjectRef,
    pub offset: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct History {
    pub snapshot_id: String,
    pub incarnation_id: String,
    #[serde(with = "decimal")]
    pub data_version: u64,
    pub manifest: ObjectRef,
    pub retired_at_upper_ms: Option<u64>,
    pub observed_at_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyPolicy {
    pub max_read_ms: u64,
    pub drain_ms: u64,
}
/// Online GC ownership. Progress is local to an invocation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GcState {
    pub owner_id: String,
    #[serde(with = "decimal")]
    pub cutoff_epoch: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub protocol_version: u32,
    pub storage_layout_version: u32,
    pub incarnation_id: String,
    #[serde(with = "decimal")]
    pub revision: u64,
    #[serde(with = "decimal")]
    pub data_version: u64,
    #[serde(with = "decimal")]
    pub metadata_version: u64,
    #[serde(with = "decimal")]
    pub epoch: u64,
    pub head_nonce: String,
    pub name: String,
    pub description: String,
    pub metadata: Value,
    pub status: String,
    pub manifest_ref: ObjectRef,
    pub snapshot_index_root: Option<ObjectRef>,
    pub snapshot_retention_days: u32,
    pub snapshot_count: u64,
    pub safety_policy: SafetyPolicy,
    pub gc: Option<GcState>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
impl Head {
    pub fn validate(&self, name: &str) -> bool {
        self.protocol_version == FORMAT_VERSION
            && self.storage_layout_version == LAYOUT_VERSION
            && self.epoch > 0
            && self
                .gc
                .as_ref()
                .is_none_or(|gc| !gc.owner_id.is_empty() && gc.cutoff_epoch == self.epoch)
            && self.name == name
            && self.manifest_ref.validate()
            && matches!(self.status.as_str(), "active" | "deleted")
            && self.snapshot_retention_days <= 36500
            && self.safety_policy.drain_ms >= self.safety_policy.max_read_ms
            && self.safety_policy.max_read_ms > 0
            && self.safety_policy.max_read_ms <= 60000
            && storage_namespace::DatabaseName::new(&self.name).is_ok()
            && storage_namespace::IncarnationId::new(&self.incarnation_id).is_ok()
            && self.description.len() <= 16384
    }
}
pub mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        if s.is_empty()
            || !s.bytes().all(|b| b.is_ascii_digit())
            || s.len() > 1 && s.starts_with('0')
        {
            return Err(serde::de::Error::custom("canonical decimal required"));
        }
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn version_counters_roundtrip_losslessly_and_reject_noncanonical_json() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Counter {
            #[serde(with = "decimal")]
            version: u64,
        }
        // Include values beyond JavaScript's exact integer range and u64's edge.
        for version in [0, 1, (1 << 53) + 1, u64::MAX] {
            let counter = Counter { version };
            let encoded = serde_json::to_string(&counter).unwrap();
            assert_eq!(encoded, format!(r#"{{"version":"{version}"}}"#));
            assert_eq!(serde_json::from_str::<Counter>(&encoded).unwrap(), counter);
        }
        for value in [
            json!(0),
            json!(1.5),
            json!(null),
            json!(true),
            json!([]),
            json!({}),
            json!(""),
            json!("00"),
            json!("01"),
            json!("+1"),
            json!("-1"),
            json!(" 1"),
            json!("1 "),
            json!("1.0"),
            json!("1e3"),
            json!("１"),
            json!("18446744073709551616"),
        ] {
            assert!(
                serde_json::from_value::<Counter>(serde_json::json!({"version": value})).is_err(),
                "accepted noncanonical counter: {value}"
            );
        }
    }
}
