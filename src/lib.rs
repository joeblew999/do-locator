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
    // Landing page so browser visitors don't see a cryptic
    // `{"code":"unimplemented","message":"method not found: "}`
    // from the ConnectRPC router when they hit /.
    if req.uri().path() == "/" {
        return http::Response::builder()
            .status(200)
            .header(http::header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(ConnectRpcBody::Full(Full::new(bytes::Bytes::from_static(
                LANDING_HTML.as_bytes(),
            ))))
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

const LANDING_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>cf-do-locator</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
         max-width: 720px; margin: 2rem auto; padding: 0 1rem; line-height: 1.5; color: #222; }
  h1 { font-size: 1.4rem; margin-bottom: 0.2rem; }
  p.lede { color: #555; margin-top: 0; }
  pre { background: #f4f4f4; padding: 0.75rem; border-radius: 4px; overflow-x: auto;
        font-size: 0.85rem; }
  code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  ul.endpoints { list-style: none; padding: 0; }
  ul.endpoints li { padding: 0.25rem 0; }
  ul.endpoints code { background: #f4f4f4; padding: 0.1rem 0.3rem; border-radius: 3px; }
  a { color: #0a58ca; }
</style>
</head>
<body>
<h1>cf-do-locator</h1>
<p class="lede">Tells you which Cloudflare region to place a Durable Object in, given the edge a user arrived at.</p>

<p>This URL is an RPC service, not a webpage. Try:</p>
<pre>curl -X POST https://cf-do-locator.gedw99.workers.dev/locator.v1.LocatorService/GetLocationHint \
  -H "Content-Type: application/json" \
  -d '{"colo":"SYD"}'</pre>
<p>→ <code>{"hint":"LOCATION_HINT_OC","known":true}</code></p>

<h2>Endpoints</h2>
<ul class="endpoints">
  <li><code>GET  /healthz</code> &nbsp;— liveness</li>
  <li><code>POST /__refresh</code> &nbsp;— manual snapshot refresh from upstream</li>
  <li><code>POST /locator.v1.LocatorService/GetLocationHint</code> &nbsp;<code>{"colo":"SYD"}</code></li>
  <li><code>POST /locator.v1.LocatorService/GetColoInfo</code> &nbsp;<code>{"colo":"FRA"}</code></li>
  <li><code>POST /locator.v1.LocatorService/ListColos</code> &nbsp;<code>{}</code> &nbsp;— <em>what consumers actually call</em></li>
  <li><code>POST /locator.v1.LocatorService/GetSnapshot</code> &nbsp;<code>{}</code></li>
</ul>

<h2>Source</h2>
<p><a href="https://github.com/joeblew999/cf-do-locator">github.com/joeblew999/cf-do-locator</a> — README, proto definition, runnable Rust + TS consumer examples.</p>
</body>
</html>
"#;

fn build_state(env: &Env) -> worker::Result<SharedState> {
    let kv = env.kv("DO_LOCATOR_KV")?;
    let snapshot_key = env
        .var("SNAPSHOT_KEY")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "snapshot".to_string());
    Ok(Arc::new(AppState { kv, snapshot_key }))
}
