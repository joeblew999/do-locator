//! Scheduled refresh pipeline.
//!
//! Pulls fresh upstream data, merges + applies hysteresis vs the current
//! KV snapshot, writes the new snapshot back.
//!
//! Hysteresis: regional nearest-neighbour latencies often differ by only a
//! few ms for colos at region boundaries, so the raw upstream winner flaps
//! between refreshes. We accept a flip only if the new region is at least
//! `max(15ms, 20%)` faster than the previously-chosen one. Logic ported
//! from connyay/cf-colo-hint.

use std::collections::BTreeMap;

use serde::Deserialize;
use worker::send::IntoSendFuture;
use worker::{Env, Fetch, Method, Request, RequestInit};

use crate::snapshot::{ColoEntry, Snapshot};

const HYSTERESIS_MARGIN_MS: f64 = 15.0;
const HYSTERESIS_MARGIN_PCT: f64 = 0.20;

const COMPONENTS_URL: &str = "https://www.cloudflarestatus.com/api/v2/components.json";
const DO_DATA_URL: &str = "https://where.durableobjects.live/api/v3/data.json";

/// CF status page group_id → human region name (informational `cf_region`
/// field on each colo; the real DO hint comes from measured latency).
fn cf_group_name(id: &str) -> &'static str {
    match id {
        "00gpj4s37mz4" => "Africa",
        "77867vxkttgw" => "Asia",
        "zqxhg7y54vy8" => "Europe",
        "91blz4ztt7dm" => "LatinAmericaCaribbean",
        "m3639x4txd08" => "MiddleEast",
        "4l01sk5cdn5c" => "NorthAmerica",
        "q6qm6fvkst4h" => "Oceania",
        _ => "Unknown",
    }
}

#[derive(Debug, Deserialize)]
struct ComponentsResponse {
    components: Vec<ComponentItem>,
}

#[derive(Debug, Deserialize)]
struct ComponentItem {
    name: String,
    #[serde(default)]
    group: bool,
    group_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DoResponse {
    #[serde(default)]
    colos: BTreeMap<String, DoColoEntry>,
}

#[derive(Debug, Deserialize)]
struct DoColoEntry {
    #[serde(rename = "nearestRegion")]
    nearest_region: Option<String>,
    #[serde(default)]
    regions: BTreeMap<String, f64>,
}

pub async fn run(env: &Env) -> worker::Result<()> {
    let kv = env.kv("DO_LOCATOR_KV")?;
    let snapshot_key = env
        .var("SNAPSHOT_KEY")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "snapshot".to_string());

    let components: ComponentsResponse = fetch_json(COMPONENTS_URL).await?;
    let do_data: DoResponse = fetch_json(DO_DATA_URL).await?;

    let base = extract_from_components(&components);
    let merged = merge_do_data(base, &do_data);

    let previous: Option<Snapshot> = kv
        .get(&snapshot_key)
        .json::<Snapshot>()
        .into_send()
        .await
        .map_err(|e| worker::Error::RustError(format!("kv get: {e}")))?;

    let (final_colos, stats) = apply_hysteresis(merged, previous.as_ref());
    worker::console_log!(
        "hysteresis: flipped={} kept={} bootstrapped={}",
        stats.flipped,
        stats.kept,
        stats.bootstrapped,
    );

    let snapshot = Snapshot {
        version: today_yyyy_mm_dd(),
        colos: final_colos,
    };

    let body = serde_json::to_string(&snapshot)
        .map_err(|e| worker::Error::RustError(format!("snapshot encode: {e}")))?;
    kv.put(&snapshot_key, body)
        .map_err(|e| worker::Error::RustError(format!("kv put builder: {e}")))?
        .execute()
        .into_send()
        .await
        .map_err(|e| worker::Error::RustError(format!("kv put: {e}")))?;

    worker::console_log!(
        "refresh complete: {} colos, {} with hint",
        snapshot.colos.len(),
        snapshot.with_hint_count(),
    );
    Ok(())
}

async fn fetch_json<T: serde::de::DeserializeOwned>(url: &str) -> worker::Result<T> {
    let req = Request::new_with_init(
        url,
        RequestInit::new().with_method(Method::Get),
    )
    .map_err(|e| worker::Error::RustError(format!("build req {url}: {e}")))?;
    let mut resp = Fetch::Request(req)
        .send()
        .into_send()
        .await
        .map_err(|e| worker::Error::RustError(format!("fetch {url}: {e}")))?;
    let bytes = resp
        .bytes()
        .into_send()
        .await
        .map_err(|e| worker::Error::RustError(format!("read body {url}: {e}")))?;
    serde_json::from_slice::<T>(&bytes)
        .map_err(|e| worker::Error::RustError(format!("decode {url}: {e}")))
}

fn extract_from_components(c: &ComponentsResponse) -> Vec<ColoEntry> {
    c.components
        .iter()
        .filter(|item| !item.group)
        .filter_map(|item| {
            let group_id = item.group_id.as_deref()?;
            let (code, base_name) = parse_iata_suffix(&item.name)?;
            Some(ColoEntry {
                code: code.to_string(),
                name: base_name.trim().to_string(),
                cf_region: cf_group_name(group_id).to_string(),
                nearest_region: None,
                regions: BTreeMap::new(),
            })
        })
        .collect()
}

/// Pull the trailing `(XYZ)` IATA code off a CF status component name.
/// Returns (code, base_name_before_iata_suffix).
fn parse_iata_suffix(name: &str) -> Option<(&str, &str)> {
    let bytes = name.as_bytes();
    if bytes.len() < 5 || bytes[bytes.len() - 1] != b')' {
        return None;
    }
    let open = name.rfind('(')?;
    let close = name.len() - 1;
    let code = &name[open + 1..close];
    if code.len() != 3 || !code.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        return None;
    }
    // base name = everything before the last " - " (the IATA suffix marker)
    let base = match name.rfind(" - ") {
        Some(idx) => &name[..idx],
        None => name,
    };
    Some((code, base))
}

fn merge_do_data(mut base: Vec<ColoEntry>, do_data: &DoResponse) -> Vec<ColoEntry> {
    // Update existing entries; collect codes we've seen.
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (i, c) in base.iter().enumerate() {
        seen.insert(c.code.clone(), i);
    }
    for (code, do_info) in &do_data.colos {
        if let Some(&idx) = seen.get(code) {
            base[idx].nearest_region = do_info.nearest_region.clone();
            base[idx].regions = do_info.regions.clone();
        } else {
            base.push(ColoEntry {
                code: code.clone(),
                name: code.clone(),
                cf_region: "Unknown".to_string(),
                nearest_region: do_info.nearest_region.clone(),
                regions: do_info.regions.clone(),
            });
        }
    }
    base.sort_by(|a, b| a.code.cmp(&b.code));
    base
}

#[derive(Debug, Default)]
pub struct HysteresisStats {
    pub flipped: usize,
    pub kept: usize,
    pub bootstrapped: usize,
}

fn apply_hysteresis(
    mut colos: Vec<ColoEntry>,
    previous: Option<&Snapshot>,
) -> (Vec<ColoEntry>, HysteresisStats) {
    let prev_map: BTreeMap<&str, Option<&str>> = previous
        .map(|s| {
            s.colos
                .iter()
                .map(|c| (c.code.as_str(), c.nearest_region.as_deref()))
                .collect()
        })
        .unwrap_or_default();

    let mut stats = HysteresisStats::default();

    for c in colos.iter_mut() {
        let prev = match prev_map.get(c.code.as_str()) {
            Some(Some(p)) => *p,
            // Either the colo isn't in prev_map at all, or its prev hint
            // was None. In both cases we have nothing to dampen against.
            _ => {
                stats.bootstrapped += 1;
                continue;
            }
        };
        let new = match c.nearest_region.as_deref() {
            Some(n) => n,
            None => continue,
        };
        if new == prev {
            continue;
        }
        let new_lat = c.regions.get(new).copied();
        let prev_lat = c.regions.get(prev).copied();
        let (Some(new_lat), Some(prev_lat)) = (new_lat, prev_lat) else {
            // Latency missing for either — refuse to flip.
            c.nearest_region = Some(prev.to_string());
            stats.kept += 1;
            continue;
        };
        let margin = HYSTERESIS_MARGIN_MS.max(prev_lat * HYSTERESIS_MARGIN_PCT);
        if prev_lat - new_lat >= margin {
            stats.flipped += 1;
        } else {
            c.nearest_region = Some(prev.to_string());
            stats.kept += 1;
        }
    }
    (colos, stats)
}

/// Day stamp used to version the snapshot. Workers don't have `chrono`
/// bundled by default and this is the only place we need a date string.
fn today_yyyy_mm_dd() -> String {
    let ms = worker::Date::now().as_millis();
    let secs = ms / 1000;
    let days = secs / 86_400;
    // Days since Unix epoch (1970-01-01) → Gregorian Y-M-D using the
    // standard civil-from-days algorithm (Howard Hinnant, public domain).
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iata_suffix() {
        let (code, base) = parse_iata_suffix("Sydney, NSW, Australia - (SYD)").unwrap();
        assert_eq!(code, "SYD");
        assert_eq!(base, "Sydney, NSW, Australia");
    }

    #[test]
    fn rejects_non_iata() {
        assert!(parse_iata_suffix("Some Service (running)").is_none());
        assert!(parse_iata_suffix("Bare name").is_none());
    }

    #[test]
    fn hysteresis_keeps_marginal_flips() {
        let colos = vec![ColoEntry {
            code: "TST".into(),
            name: "Test".into(),
            cf_region: "X".into(),
            nearest_region: Some("enam".into()),
            regions: [("enam".to_string(), 40.0), ("sam".to_string(), 38.0)]
                .into_iter()
                .collect(),
        }];
        let prev = Snapshot {
            version: "2026-01-01".into(),
            colos: vec![ColoEntry {
                code: "TST".into(),
                name: "Test".into(),
                cf_region: "X".into(),
                nearest_region: Some("sam".into()),
                regions: BTreeMap::new(),
            }],
        };
        // new=enam (40ms) vs prev=sam (38ms): new is SLOWER, must keep sam.
        let (result, _) = apply_hysteresis(colos, Some(&prev));
        assert_eq!(result[0].nearest_region.as_deref(), Some("sam"));
    }

    #[test]
    fn hysteresis_accepts_clear_wins() {
        let colos = vec![ColoEntry {
            code: "TST".into(),
            name: "Test".into(),
            cf_region: "X".into(),
            nearest_region: Some("enam".into()),
            regions: [("enam".to_string(), 20.0), ("sam".to_string(), 100.0)]
                .into_iter()
                .collect(),
        }];
        let prev = Snapshot {
            version: "2026-01-01".into(),
            colos: vec![ColoEntry {
                code: "TST".into(),
                name: "Test".into(),
                cf_region: "X".into(),
                nearest_region: Some("sam".into()),
                regions: BTreeMap::new(),
            }],
        };
        // 80ms faster, well past the max(15ms, 20%) margin.
        let (result, _) = apply_hysteresis(colos, Some(&prev));
        assert_eq!(result[0].nearest_region.as_deref(), Some("enam"));
    }
}
