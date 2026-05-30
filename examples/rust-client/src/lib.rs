//! The reusable consumer pattern: call `ListColos` once at startup, cache
//! the result in memory, do O(1) local lookups per request.
//!
//! ConnectRPC speaks plain HTTP + JSON, so this demo skips the codegen
//! pipeline and hits the endpoint directly with `ureq`. In a real
//! workers-rs consumer you'd swap `ureq` for `worker::Fetch` — the
//! cache + lookup shape is identical.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListColosResponse {
    snapshot: Snapshot,
    colos: Vec<Colo>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: String,
    pub total_colos: i32,
    pub colos_with_hint: i32,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Colo {
    pub code: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cf_region: String,
    // Proto3 JSON omits default-valued enums, so colos with no mapping
    // arrive without this field.
    #[serde(default)]
    pub hint: String, // e.g. "LOCATION_HINT_OC" or "" (unspecified)
}

/// Strip the `LOCATION_HINT_` prefix; returns `None` for the unspecified case.
pub fn hint_code(h: &str) -> Option<String> {
    if h == "LOCATION_HINT_UNSPECIFIED" || h.is_empty() {
        return None;
    }
    Some(h.trim_start_matches("LOCATION_HINT_").to_ascii_lowercase())
}

/// Consumer cache. Construct once at process / isolate start; reuse across
/// every request thereafter.
pub struct LocatorCache {
    base_url: String,
    hints: HashMap<String, String>,
    snapshot: Option<Snapshot>,
}

impl LocatorCache {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            hints: HashMap::new(),
            snapshot: None,
        }
    }

    /// Fetch the full table once. Idempotent — safe to re-call later.
    pub fn load(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let url = format!("{}/locator.v1.LocatorService/ListColos", self.base_url);
        let resp = ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_string("{}")?;
        let body: ListColosResponse = serde_json::from_reader(resp.into_reader())?;
        self.snapshot = Some(body.snapshot);
        self.hints = body
            .colos
            .into_iter()
            .map(|c| (c.code, c.hint))
            .collect();
        Ok(())
    }

    /// Return the hint code (e.g. `"oc"`) for an edge code, or `None`.
    pub fn hint_for(&self, colo: &str) -> Option<String> {
        self.hints.get(colo).and_then(|h| hint_code(h))
    }

    pub fn version(&self) -> &str {
        self.snapshot
            .as_ref()
            .map(|s| s.version.as_str())
            .unwrap_or("(not loaded)")
    }

    pub fn size(&self) -> usize {
        self.hints.len()
    }
}
