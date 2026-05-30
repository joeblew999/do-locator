//! Reusable consumer pattern: call ListColos once at process / isolate
//! startup, cache the result, do O(1) local lookups per request.
//!
//! ConnectRPC speaks plain HTTP + JSON, so this skips codegen and hits
//! the endpoints directly with `ureq`. In a real workers-rs consumer
//! you'd swap `ureq` for `worker::Fetch`; the cache + funnel shape is
//! identical.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

// ─────────────────────────────────────────────────────────────────────
// Proto type mirrors (no codegen needed — JSON + serde does the job).
// ─────────────────────────────────────────────────────────────────────

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
    /// Proto3 JSON omits default-valued enums, so colos with no mapping
    /// arrive without this field. `""` means UNSPECIFIED.
    #[serde(default)]
    pub hint: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListColosResponse {
    pub snapshot: Snapshot,
    pub colos: Vec<Colo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetSnapshotResponse {
    pub snapshot: Snapshot,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLocationHintResponse {
    #[serde(default)]
    pub hint: String,
    pub known: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetColoInfoResponse {
    #[serde(default)]
    pub colo: Option<Colo>,
    pub known: bool,
}

// ─────────────────────────────────────────────────────────────────────
// Low-level RPC client — one method per service RPC. Useful for tools
// and observability; the per-request hot path goes through `LocatorCache`.
// ─────────────────────────────────────────────────────────────────────

pub struct Locator {
    base_url: String,
}

impl Locator {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    fn post<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: &str,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let url = format!("{}/locator.v1.LocatorService/{}", self.base_url, method);
        let resp = ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_string(body)?;
        Ok(serde_json::from_reader(resp.into_reader())?)
    }

    pub fn get_snapshot(&self) -> Result<Snapshot, Box<dyn std::error::Error>> {
        let r: GetSnapshotResponse = self.post("GetSnapshot", "{}")?;
        Ok(r.snapshot)
    }
    pub fn list_colos(&self) -> Result<ListColosResponse, Box<dyn std::error::Error>> {
        self.post("ListColos", "{}")
    }
    pub fn get_location_hint(
        &self,
        colo: &str,
    ) -> Result<GetLocationHintResponse, Box<dyn std::error::Error>> {
        self.post("GetLocationHint", &format!(r#"{{"colo":"{colo}"}}"#))
    }
    pub fn get_colo_info(
        &self,
        colo: &str,
    ) -> Result<GetColoInfoResponse, Box<dyn std::error::Error>> {
        self.post("GetColoInfo", &format!(r#"{{"colo":"{colo}"}}"#))
    }
}

// ─────────────────────────────────────────────────────────────────────
// Cache — construct once at process / isolate boot, reuse everywhere.
// ─────────────────────────────────────────────────────────────────────

/// `"LOCATION_HINT_OC"` → `Some("oc")`. `""`, `"LOCATION_HINT_UNSPECIFIED"` → `None`.
pub fn hint_code(h: &str) -> Option<String> {
    if h.is_empty() || h == "LOCATION_HINT_UNSPECIFIED" {
        return None;
    }
    Some(h.trim_start_matches("LOCATION_HINT_").to_ascii_lowercase())
}

pub struct LocatorCache {
    rpc: Locator,
    hints: HashMap<String, String>,
    snapshot: Option<Snapshot>,
}

impl LocatorCache {
    pub fn new(base_url: impl Into<String>) -> Self {
        let url = base_url.into();
        Self {
            rpc: Locator::new(url),
            hints: HashMap::new(),
            snapshot: None,
        }
    }

    pub fn load(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let r = self.rpc.list_colos()?;
        self.snapshot = Some(r.snapshot);
        self.hints = r.colos.into_iter().map(|c| (c.code, c.hint)).collect();
        Ok(())
    }

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

// ─────────────────────────────────────────────────────────────────────
// Funnel pattern — every DO creation in a consumer Worker should go
// through one of these helpers. Policy varies per DO type; the
// observability log is mandatory.
// ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum DoType {
    User,
    Tenant,
    Session,
}

impl DoType {
    pub fn as_str(&self) -> &'static str {
        match self {
            DoType::User => "user",
            DoType::Tenant => "tenant",
            DoType::Session => "session",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreationLog {
    pub colo: String, // request.cf.colo OR "_explicit_" for tenant case
    pub hint: Option<String>,
    pub do_type: DoType,
    pub do_id: String,
    pub ts_ms: u128,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Stand-in for `env.X_DO.new_unique_id_with_options({ locationHint })`.
/// A real consumer calls the workers-rs DO namespace here; we synthesise
/// an id so the demo runs as a plain `cargo run`.
fn fake_new_unique_id(hint: Option<&str>) -> String {
    let region = hint.unwrap_or("auto");
    let rand: u32 = now_ms() as u32 ^ 0xC0FFEE;
    format!("do-{region}-{rand:08x}")
}

/// Per-user DO. Policy: request.cf.colo at signup.
pub fn create_user_do<F: FnOnce(CreationLog)>(
    cache: &LocatorCache,
    req_colo: &str,
    _user_id: &str,
    log: F,
) -> String {
    let hint = cache.hint_for(req_colo);
    let do_id = fake_new_unique_id(hint.as_deref());
    log(CreationLog {
        colo: req_colo.to_string(),
        hint,
        do_type: DoType::User,
        do_id: do_id.clone(),
        ts_ms: now_ms(),
    });
    // Real: env.user_do().new_unique_id_with_options(...) etc.
    do_id
}

/// Per-tenant DO. Policy: EXPLICIT region from the org owner, NOT request.cf.colo.
pub fn create_tenant_do<F: FnOnce(CreationLog)>(
    owner_region: &str,
    _tenant_id: &str,
    log: F,
) -> String {
    let do_id = fake_new_unique_id(Some(owner_region));
    log(CreationLog {
        colo: "_explicit_".to_string(),
        hint: Some(owner_region.to_string()),
        do_type: DoType::Tenant,
        do_id: do_id.clone(),
        ts_ms: now_ms(),
    });
    do_id
}

/// Per-session DO. Policy: request.cf.colo of the creator.
pub fn create_session_do<F: FnOnce(CreationLog)>(
    cache: &LocatorCache,
    req_colo: &str,
    _session_id: &str,
    log: F,
) -> String {
    let hint = cache.hint_for(req_colo);
    let do_id = fake_new_unique_id(hint.as_deref());
    log(CreationLog {
        colo: req_colo.to_string(),
        hint,
        do_type: DoType::Session,
        do_id: do_id.clone(),
        ts_ms: now_ms(),
    });
    do_id
}
