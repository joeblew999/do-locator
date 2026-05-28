//! Canonical KV-stored snapshot shape, plus conversions to proto types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::proto::locator::v1::{Colo as ColoPb, LocationHint, Snapshot as SnapshotPb};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: String,
    pub colos: Vec<ColoEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColoEntry {
    pub code: String,
    pub name: String,
    pub cf_region: String,
    pub nearest_region: Option<String>,
    /// Per-region latency measurements (ms). Needed across refreshes so
    /// hysteresis can compare the proposed new region's latency against
    /// the current pick's. Not exposed in proto responses.
    #[serde(default)]
    pub regions: BTreeMap<String, f64>,
}

impl Snapshot {
    pub fn find(&self, colo: &str) -> Option<&ColoEntry> {
        self.colos.iter().find(|c| c.code == colo)
    }

    pub fn with_hint_count(&self) -> usize {
        self.colos
            .iter()
            .filter(|c| c.nearest_region.is_some())
            .count()
    }
}

pub fn hint_str_to_pb(s: Option<&str>) -> buffa::EnumValue<LocationHint> {
    use LocationHint::*;
    match s.unwrap_or("") {
        "wnam" => LOCATION_HINT_WNAM.into(),
        "enam" => LOCATION_HINT_ENAM.into(),
        "sam" => LOCATION_HINT_SAM.into(),
        "weur" => LOCATION_HINT_WEUR.into(),
        "eeur" => LOCATION_HINT_EEUR.into(),
        "apac" => LOCATION_HINT_APAC.into(),
        "oc" => LOCATION_HINT_OC.into(),
        "afr" => LOCATION_HINT_AFR.into(),
        "me" => LOCATION_HINT_ME.into(),
        _ => LOCATION_HINT_UNSPECIFIED.into(),
    }
}

pub fn entry_to_pb(e: &ColoEntry) -> ColoPb {
    ColoPb {
        code: e.code.clone(),
        name: e.name.clone(),
        cf_region: e.cf_region.clone(),
        hint: hint_str_to_pb(e.nearest_region.as_deref()),
        ..Default::default()
    }
}

pub fn snapshot_to_pb(s: &Snapshot) -> SnapshotPb {
    SnapshotPb {
        version: s.version.clone(),
        total_colos: s.colos.len() as i32,
        colos_with_hint: s.with_hint_count() as i32,
        ..Default::default()
    }
}
