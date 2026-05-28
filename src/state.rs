use std::sync::Arc;

use worker::kv::KvStore;

pub struct AppState {
    pub kv: KvStore,
    pub snapshot_key: String,
}

pub type SharedState = Arc<AppState>;
