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

fn build_state(env: &Env) -> worker::Result<SharedState> {
    let kv = env.kv("DO_LOCATOR_KV")?;
    let snapshot_key = env
        .var("SNAPSHOT_KEY")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "snapshot".to_string());
    Ok(Arc::new(AppState { kv, snapshot_key }))
}
