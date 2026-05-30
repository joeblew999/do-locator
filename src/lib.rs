//! Cloudflare colo → Durable Objects locationHint lookup service.
//!
//! Two entry points:
//!
//! - `fetch(req, env, ctx)` — ConnectRPC service handler; reads the
//!   snapshot from KV and answers `LocatorService` RPCs.
//! - `scheduled(event, env, ctx)` — cron handler; pulls fresh upstream
//!   data, applies hysteresis vs the current snapshot, writes back to KV.
//!
//! KV is the source of truth. The snapshot blob is JSON-encoded under a
//! single key (default: `snapshot`). See `refresh::run` for the refresh
//! pipeline and `service::LocatorServer` for the read path.

#![allow(refining_impl_trait)]

use std::sync::Arc;

use connectrpc::{ConnectRpcBody, ConnectRpcService, Router as RpcRouter};
use http_body_util::Full;
use tower::Service;
use worker::{Context, Env, HttpRequest, ScheduleContext, ScheduledEvent, event};

pub(crate) mod proto {
    connectrpc::include_generated!();
}

mod refresh;
mod service;
mod snapshot;
mod state;

use crate::proto::locator::v1::LocatorServiceExt;
use crate::service::LocatorServer;
use crate::state::{AppState, SharedState};

#[event(fetch, respond_with_errors)]
async fn fetch(
    req: HttpRequest,
    env: Env,
    _ctx: Context,
) -> worker::Result<http::Response<ConnectRpcBody>> {
    // Interactive landing page so browser visitors get a real demo
    // instead of `{"code":"unimplemented","message":"method not found: "}`.
    // Server-side renders the visitor's own colo (from the cf-ray header)
    // so the first paint shows them "where they are" — client-side JS
    // does the rest via the public RPCs.
    if req.uri().path() == "/" {
        // Try Cf extension first (canonical workers-rs path), fall back to
        // parsing the cf-ray header (`<ray_id>-<COLO>`). Either should
        // work in prod; one or the other might be empty in dev.
        let colo_from_cf = req
            .extensions()
            .get::<worker::Cf>()
            .map(|cf| cf.colo());
        let colo_from_ray = req
            .headers()
            .get("cf-ray")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.rsplit_once('-'))
            .map(|(_, c)| c.to_string());
        let colo = colo_from_cf
            .or(colo_from_ray)
            .unwrap_or_else(|| "???".to_string());
        let html = LANDING_HTML.replace("{{COLO}}", &colo);
        return http::Response::builder()
            .status(200)
            .header(http::header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(ConnectRpcBody::Full(Full::new(bytes::Bytes::from(html))))
            .map_err(|e| worker::Error::RustError(format!("landing: {e}")));
    }

    // Plain health probe (skip RPC machinery for liveness checks).
    if req.uri().path() == "/healthz" {
        return http::Response::builder()
            .status(200)
            .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(ConnectRpcBody::Full(Full::new(bytes::Bytes::from_static(
                b"ok",
            ))))
            .map_err(|e| worker::Error::RustError(format!("healthz: {e}")));
    }

    // Manual refresh trigger — same logic as the weekly cron. Useful for
    // bootstrapping KV after first deploy. The data is public so we don't
    // gate this; worst case someone hits it spuriously and we re-fetch
    // upstream once.
    if req.uri().path() == "/__refresh" {
        return match refresh::run(&env).await {
            Ok(()) => http::Response::builder()
                .status(200)
                .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(ConnectRpcBody::Full(Full::new(bytes::Bytes::from_static(
                    b"refreshed",
                ))))
                .map_err(|e| worker::Error::RustError(format!("refresh: {e}"))),
            Err(e) => Err(worker::Error::RustError(format!("refresh failed: {e}"))),
        };
    }

    let state = build_state(&env)?;
    let router = RpcRouter::new();
    let router = Arc::new(LocatorServer::new(Arc::clone(&state))).register(router);

    let mut svc = ConnectRpcService::new(router);
    svc.call(req)
        .await
        .map_err(|e| worker::Error::RustError(format!("rpc dispatch: {e}")))
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    if let Err(e) = refresh::run(&env).await {
        worker::console_error!("refresh failed: {e}");
    }
}

const LANDING_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>cf-do-locator — live demo</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: light dark; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
    max-width: 760px; margin: 1.5rem auto; padding: 0 1rem; line-height: 1.55;
  }
  h1 { font-size: 1.5rem; margin-bottom: 0.1rem; }
  h2 { font-size: 1.05rem; margin-top: 2rem; }
  p.lede { color: #555; margin-top: 0; }
  @media (prefers-color-scheme: dark) { p.lede, .muted { color: #aaa; } }
  .muted { color: #666; }
  .card {
    border: 1px solid #ddd; border-radius: 8px; padding: 1rem 1.2rem;
    margin: 1rem 0; background: #fafafa;
  }
  @media (prefers-color-scheme: dark) {
    .card { background: #1a1a1a; border-color: #333; }
  }
  .big { font-size: 1.8rem; font-weight: 600; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  .verdict { font-size: 1.1rem; margin-top: 0.4rem; }
  .verdict strong { color: #0a7d3f; }
  @media (prefers-color-scheme: dark) { .verdict strong { color: #5ed18b; } }
  .pill {
    display: inline-block; padding: 0.1rem 0.5rem; border-radius: 12px;
    background: #0a7d3f; color: #fff; font-family: ui-monospace, monospace; font-size: 0.85rem;
  }
  .pill.warn { background: #b45309; }
  input[type="text"] {
    padding: 0.4rem 0.6rem; font-size: 1rem; border: 1px solid #aaa; border-radius: 4px;
    width: 6rem; text-transform: uppercase; font-family: ui-monospace, monospace;
  }
  button {
    padding: 0.4rem 0.9rem; font-size: 1rem; border: 1px solid #0a58ca; background: #0a58ca;
    color: white; border-radius: 4px; cursor: pointer;
  }
  button:hover { background: #0848a8; }
  button.secondary { background: transparent; color: #0a58ca; }
  pre {
    background: #f4f4f4; padding: 0.75rem; border-radius: 4px; overflow-x: auto;
    font-size: 0.85rem; line-height: 1.4;
  }
  @media (prefers-color-scheme: dark) { pre { background: #1a1a1a; } }
  code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  table { border-collapse: collapse; width: 100%; font-size: 0.9rem; }
  th, td { text-align: left; padding: 0.3rem 0.5rem; border-bottom: 1px solid #eee; }
  @media (prefers-color-scheme: dark) { th, td { border-color: #333; } }
  a { color: #0a58ca; }
  .scroll { max-height: 16rem; overflow-y: auto; border: 1px solid #ddd; border-radius: 4px; }
  @media (prefers-color-scheme: dark) { .scroll { border-color: #333; } }
</style>
</head>
<body>

<h1>cf-do-locator</h1>
<p class="lede">Live demo — tells you which Cloudflare region to put a Durable Object in.</p>

<div class="card" id="you">
  <div class="muted">You hit Cloudflare's edge</div>
  <div class="big" id="yourColo">{{COLO}}</div>
  <div class="verdict" id="yourVerdict">looking up…</div>
  <div class="muted" style="margin-top:0.5rem; font-size:0.9rem;" id="yourExplain"></div>
</div>

<h2>Try any edge</h2>
<p class="muted" style="margin-top:0">Type any Cloudflare IATA code (SYD, LAX, FRA, NRT, JNB, …)</p>
<div>
  <input id="probe" type="text" value="SYD" maxlength="3">
  <button onclick="lookup()">Look up</button>
  <span id="probeResult" style="margin-left:1rem; font-family: ui-monospace, monospace;"></span>
</div>

<h2>Snapshot</h2>
<div id="snapshotInfo" class="muted">loading…</div>

<h2>All known edges <button class="secondary" onclick="toggleTable()" id="tableBtn">show all</button></h2>
<div id="tableWrap" style="display:none">
  <input id="filter" type="text" placeholder="filter…" style="width:8rem; margin-bottom:0.5rem; text-transform:none" oninput="renderTable()">
  <div class="scroll">
    <table id="coloTable">
      <thead><tr><th>code</th><th>name</th><th>cfRegion</th><th>hint</th></tr></thead>
      <tbody></tbody>
    </table>
  </div>
</div>

<h2>Use it from your code</h2>
<p>Once at isolate boot, populate a local map; then look up locally per request:</p>
<pre>const r = await fetch("https://cf-do-locator.gedw99.workers.dev/locator.v1.LocatorService/ListColos",
  { method:"POST", headers:{"Content-Type":"application/json"}, body:"{}" });
const { colos } = await r.json();
const HINTS = new Map(colos.map(c =&gt; [c.code, c.hint]));

// per signup:
const hint = HINTS.get(request.cf?.colo ?? "");
env.USER_DO.newUniqueId(hint ? { locationHint: shortHint(hint) } : undefined);</pre>

<p class="muted">Runnable Rust + TS demos: <a href="https://github.com/joeblew999/cf-do-locator/tree/main/examples">github.com/joeblew999/cf-do-locator/examples</a></p>

<script>
const BASE = location.origin;
const HINT_NAMES = {
  oc:   "Oceania",
  wnam: "Western North America",
  enam: "Eastern North America",
  sam:  "South America",
  weur: "Western Europe",
  eeur: "Eastern Europe",
  apac: "Asia-Pacific",
  afr:  "Africa",
  me:   "Middle East",
};
function shortHint(h) {
  if (!h || h === "LOCATION_HINT_UNSPECIFIED") return null;
  return h.replace("LOCATION_HINT_", "").toLowerCase();
}
async function rpc(method, body) {
  const r = await fetch(`${BASE}/locator.v1.LocatorService/${method}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!r.ok) throw new Error(method + " " + r.status);
  return r.json();
}

let CACHE = new Map(); // code → { name, cfRegion, hint }

async function init() {
  // Snapshot + ListColos in parallel
  const [snap, list] = await Promise.all([
    rpc("GetSnapshot", {}),
    rpc("ListColos", {}),
  ]);
  document.getElementById("snapshotInfo").innerHTML =
    `version <code>${snap.snapshot.version}</code>` +
    ` &middot; ${snap.snapshot.totalColos} edges` +
    ` &middot; ${snap.snapshot.colosWithHint} with hint`;

  CACHE = new Map(list.colos.map(c => [c.code, c]));

  // Render the "you" card
  const code = document.getElementById("yourColo").textContent.trim();
  paintYou(code);

  renderTable();
}

function paintYou(code) {
  const verdict = document.getElementById("yourVerdict");
  const explain = document.getElementById("yourExplain");
  const info = CACHE.get(code);
  if (!info) {
    verdict.innerHTML = `<span class="pill warn">unknown edge</span>`;
    explain.textContent =
      "We have no measurements for this edge yet. Your consumer should create the DO without a locationHint and log it.";
    return;
  }
  const h = shortHint(info.hint);
  if (!h) {
    verdict.innerHTML = `<strong>${info.name}</strong> &nbsp; <span class="pill warn">no hint yet</span>`;
    explain.textContent = "This edge is known but has no measured DO-region mapping yet. Create the DO without a hint.";
    return;
  }
  verdict.innerHTML =
    `<strong>${info.name}</strong> &nbsp;→&nbsp; place DOs in ` +
    `<span class="pill">${h}</span> &nbsp;(${HINT_NAMES[h] || h})`;
  explain.innerHTML =
    `A consumer Worker handling a signup from <code>${code}</code> should call ` +
    `<code>newUniqueId({ locationHint: "${h}" })</code> — the DO lives in ${HINT_NAMES[h]} for life and ` +
    `the user's reads stay near them.`;
}

async function lookup() {
  const code = document.getElementById("probe").value.trim().toUpperCase();
  if (!code) return;
  try {
    const r = await rpc("GetColoInfo", { colo: code });
    const span = document.getElementById("probeResult");
    if (!r.known || !r.colo) {
      span.innerHTML = `<span class="pill warn">unknown</span>`;
      return;
    }
    const h = shortHint(r.colo.hint);
    span.innerHTML = h
      ? `${r.colo.name} → <span class="pill">${h}</span> (${HINT_NAMES[h] || h})`
      : `${r.colo.name} → <span class="pill warn">no hint</span>`;
  } catch (e) {
    document.getElementById("probeResult").textContent = String(e);
  }
}
document.getElementById("probe").addEventListener("keydown", e => {
  if (e.key === "Enter") lookup();
});

function toggleTable() {
  const wrap = document.getElementById("tableWrap");
  const btn = document.getElementById("tableBtn");
  if (wrap.style.display === "none") {
    wrap.style.display = "block";
    btn.textContent = "hide";
  } else {
    wrap.style.display = "none";
    btn.textContent = "show all";
  }
}

function renderTable() {
  const filter = (document.getElementById("filter")?.value || "").toLowerCase();
  const tbody = document.querySelector("#coloTable tbody");
  tbody.innerHTML = "";
  const rows = [...CACHE.values()]
    .filter(c => !filter || c.code.toLowerCase().includes(filter) ||
                  (c.name || "").toLowerCase().includes(filter) ||
                  (shortHint(c.hint) || "").includes(filter))
    .sort((a, b) => a.code.localeCompare(b.code));
  for (const c of rows) {
    const h = shortHint(c.hint);
    const tr = document.createElement("tr");
    tr.innerHTML =
      `<td><code>${c.code}</code></td>` +
      `<td>${c.name || ""}</td>` +
      `<td>${c.cfRegion || ""}</td>` +
      `<td>${h ? `<span class="pill">${h}</span>` : "—"}</td>`;
    tbody.appendChild(tr);
  }
}

init().catch(e => {
  document.getElementById("yourVerdict").textContent = "Error loading snapshot: " + e;
});
</script>

</body>
</html>
"##;

fn build_state(env: &Env) -> worker::Result<SharedState> {
    let kv = env.kv("DO_LOCATOR_KV")?;
    let snapshot_key = env
        .var("SNAPSHOT_KEY")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "snapshot".to_string());
    Ok(Arc::new(AppState { kv, snapshot_key }))
}
