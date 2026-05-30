# cf-do-locator Rust consumer

The reusable cache + funnel helpers that any workers-rs Worker should adopt to get DOs placed in the right region.

## Drop it into your Worker

```bash
mkdir -p src/locator
curl -L https://raw.githubusercontent.com/joeblew999/cf-do-locator/main/examples/rust-client/src/lib.rs \
  -o src/locator/mod.rs
```

Then add to your `Cargo.toml`:

```toml
[dependencies]
ureq       = { version = "2.10", default-features = false, features = ["tls"] }
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
```

For a wasm32 Workers build, swap `ureq` for `worker::Fetch` (the loader is the only thing that needs changing — the cache + funnel API are identical). The CLI demo uses `ureq` so `cargo run` works natively.

## Wire it in

```rust
// src/worker.rs (or wherever your fetch handler lives)
use std::sync::OnceLock;
use crate::locator::{CreationLog, LocatorCache, create_user_do, create_tenant_do};

// One cache per isolate.
static CACHE: OnceLock<LocatorCache> = OnceLock::new();

fn locator(env: &worker::Env) -> worker::Result<&'static LocatorCache> {
    Ok(CACHE.get_or_init(|| {
        let url = env
            .var("CF_DO_LOCATOR_URL")
            .expect("CF_DO_LOCATOR_URL")
            .to_string();
        let mut c = LocatorCache::new(url);
        c.load().expect("ListColos");
        c
    }))
}

fn log_creation(e: CreationLog) {
    worker::console_log!("do_creation {:?}", e);
}

// ── DO-creation funnel — every DO creation in your crate should go
//    through one of these helpers. They enforce per-DO-type policy at the
//    type level (e.g. create_tenant_do can't take a request colo). ──

pub fn signup(req: &worker::Request, env: &worker::Env, user_id: &str) -> worker::Result<()> {
    let cache  = locator(env)?;
    let colo   = req.cf().map(|cf| cf.colo()).unwrap_or_default();
    let _do_id = create_user_do(cache, &colo, user_id, log_creation);
    // Real code: env.durable_object("USER_DO")?.unique_id_with_options(...).
    Ok(())
}

pub fn provision_org(env: &worker::Env, tenant_id: &str, owner_region: &str) -> worker::Result<()> {
    // NOT request.cf.colo — admin might be in Singapore provisioning a
    // US-based team. Hint binds at creation and is permanent.
    let _do_id = create_tenant_do(owner_region, tenant_id, log_creation);
    Ok(())
}
```

## See it work — run the demo CLI

The same library powers a CLI demo that exercises every RPC and every funnel helper. Run against either local `wrangler dev` or the live service.

```bash
cd examples/rust-client
cargo run                                                              # → http://127.0.0.1:8787
CF_DO_LOCATOR_URL=https://cf-do-locator.gedw99.workers.dev cargo run    # → prod
```

Output (the canonical sanity check):

```
── GetSnapshot ─────────────────────────────────────────────────
  version:         2026-05-30
  total colos:     340
  colos with hint: 277

── ListColos → LocatorCache ────────────────────────────────────
  cached 340 colos (snapshot 2026-05-30)

── Funnel pattern: per-user DO (use request.cf.colo) ───────────
    [log] user     colo=SYD  hint=oc    do_id=do-oc-...
    [log] user     colo=LAX  hint=wnam  do_id=do-wnam-...

── Funnel pattern: per-tenant DO (NEVER use admin's edge) ──────
    [log] tenant   colo=_explicit_  hint=wnam  do_id=do-wnam-...

── Funnel pattern: per-session DO (creator's edge is fine) ─────
    [log] session  colo=FRA  hint=eeur  do_id=do-eeur-...

── Edge cases ──────────────────────────────────────────────────
    [log] user     colo=XXX  hint=(none)  do_id=do-auto-...
    [log] user     colo=     hint=(none)  do_id=do-auto-...
```

## Files

- [`src/lib.rs`](src/lib.rs) — the file you vendor into your Worker (rename to `mod.rs` per the snippet above).
- [`src/main.rs`](src/main.rs) — the demo driver (don't vendor; just for `cargo run`).
