//! `locator.v1.LocatorService` implementation.
//!
//! Every RPC reads the snapshot blob from KV. Each read is ~1 KV op
//! (~10ms global). For high-volume lookup workloads, consumers should
//! call `ListColos` once per isolate and cache locally — see
//! `examples/ts-client/`.

use buffa::MessageField;
use connectrpc::{ConnectError, RequestContext, Response, ServiceResult};
use worker::send::IntoSendFuture;

use crate::proto::locator::v1::{
    GetColoInfoResponse, GetLocationHintResponse, GetSnapshotResponse, ListColosResponse,
    LocatorService, OwnedGetColoInfoRequestView, OwnedGetLocationHintRequestView,
    OwnedGetSnapshotRequestView, OwnedListColosRequestView,
};
use crate::snapshot::{Snapshot, entry_to_pb, hint_str_to_pb, snapshot_to_pb};
use crate::state::SharedState;

pub struct LocatorServer {
    state: SharedState,
}

impl LocatorServer {
    pub fn new(state: SharedState) -> Self {
        Self { state }
    }

    async fn load_snapshot(&self) -> Result<Snapshot, ConnectError> {
        let raw = self
            .state
            .kv
            .get(&self.state.snapshot_key)
            .json::<Snapshot>()
            .into_send()
            .await
            .map_err(|e| ConnectError::internal(format!("kv read: {e}")))?
            .ok_or_else(|| {
                ConnectError::unavailable("snapshot not yet populated; cron has not run")
            })?;
        Ok(raw)
    }
}

impl LocatorService for LocatorServer {
    async fn get_location_hint(
        &self,
        _ctx: RequestContext,
        request: OwnedGetLocationHintRequestView,
    ) -> ServiceResult<GetLocationHintResponse> {
        let snap = self.load_snapshot().await?;
        let entry = snap.find(&request.colo);
        let resp = GetLocationHintResponse {
            hint: hint_str_to_pb(entry.and_then(|e| e.nearest_region.as_deref())),
            known: entry.is_some(),
            ..Default::default()
        };
        Ok(Response::new(resp))
    }

    async fn get_colo_info(
        &self,
        _ctx: RequestContext,
        request: OwnedGetColoInfoRequestView,
    ) -> ServiceResult<GetColoInfoResponse> {
        let snap = self.load_snapshot().await?;
        let entry = snap.find(&request.colo);
        let resp = GetColoInfoResponse {
            colo: entry.map(entry_to_pb).map(MessageField::some).unwrap_or_default(),
            known: entry.is_some(),
            ..Default::default()
        };
        Ok(Response::new(resp))
    }

    async fn list_colos(
        &self,
        _ctx: RequestContext,
        _request: OwnedListColosRequestView,
    ) -> ServiceResult<ListColosResponse> {
        let snap = self.load_snapshot().await?;
        let resp = ListColosResponse {
            snapshot: MessageField::some(snapshot_to_pb(&snap)),
            colos: snap.colos.iter().map(entry_to_pb).collect(),
            ..Default::default()
        };
        Ok(Response::new(resp))
    }

    async fn get_snapshot(
        &self,
        _ctx: RequestContext,
        _request: OwnedGetSnapshotRequestView,
    ) -> ServiceResult<GetSnapshotResponse> {
        let snap = self.load_snapshot().await?;
        let resp = GetSnapshotResponse {
            snapshot: MessageField::some(snapshot_to_pb(&snap)),
            ..Default::default()
        };
        Ok(Response::new(resp))
    }
}

