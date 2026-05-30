# cf-do-locator Rust consumer example

A runnable demonstration of the consumer pattern: call `ListColos` once at boot, cache the table, do O(1) local lookups per request.

## Run it

Against local wrangler dev (default URL `http://127.0.0.1:8787`):

```bash
cargo run
```

Against the live service:

```bash
CF_DO_LOCATOR_URL=https://cf-do-locator.gedw99.workers.dev cargo run
```

Expected output:

```
→ cache.load() from http://127.0.0.1:8787
  snapshot version: 2026-05-28
  colos cached:     340

  SYD  →  oc
  LAX  →  wnam
  FRA  →  eeur
  JNB  →  afr
  XXX  →  (unknown — create DO without hint, log it)
```

## Files

- [`src/lib.rs`](src/lib.rs) — the reusable `LocatorCache` struct. Copy this into your Worker.
- [`src/main.rs`](src/main.rs) — the demo driver.

## Using `LocatorCache` in a real workers-rs Worker

The demo CLI uses `ureq` (sync, native) so it runs as a plain `cargo run`. In a real Worker you'd swap the transport for `worker::Fetch` but keep the same struct + cache pattern:

```rust
use std::sync::OnceLock;

static CACHE: OnceLock<LocatorCache> = OnceLock::new();

async fn locator(env: &worker::Env) -> worker::Result<&'static LocatorCache> {
    if let Some(c) = CACHE.get() {
        return Ok(c);
    }
    let url = env.var("CF_DO_LOCATOR_URL")?.to_string();
    let mut c = LocatorCache::new(url);
    c.load_via_worker_fetch().await?;   // your Worker-flavoured load()
    Ok(CACHE.get_or_init(|| c))
}

// In your DO-creation funnel:
pub async fn create_user_do(
    req: &worker::Request,
    env: &worker::Env,
    user_id: &str,
) -> worker::Result<worker::Stub> {
    let cache = locator(env).await?;
    let colo = req.cf().and_then(|cf| cf.colo()).unwrap_or_default();
    let hint = cache.hint_for(&colo);

    let ns = env.durable_object("USER_DO")?;
    // ⚠️  hint is only honoured by unique_id, NOT id_from_name.
    let id = ns.unique_id_with_options(/* { locationHint: hint } */)?;
    // Observability: log {colo, hint, do_type, do_id} here.
    id.get_stub()
}
```

## Per-DO-type policy

Different DO types want different "colo sources" — see the [parent README's "one footgun" section](../../README.md#the-one-footgun). Briefly:

- Per-user — use `req.cf.colo` at signup.
- Per-team / per-org — do **not** use the admin's edge; let the team owner pick a region.
- Per-session / per-room — use the creator's edge.

## Typed proto client (optional)

This demo uses raw `ureq` + serde, which works because ConnectRPC accepts plain HTTP + JSON natively. For a fully-typed Rust client, add `connectrpc-build` to a `build.rs` pointing at `../../proto`. The cache + lookup pattern doesn't change — only the wire encoding does.
