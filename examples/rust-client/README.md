# Rust consumer pattern

A consumer Worker calls cf-do-locator over ConnectRPC. The proto definition
lives in this repo (`proto/locator/v1/locator.proto`). Consumers should
either:

1. **Vendor the proto** (git submodule) and run `connectrpc-build` in
   their own `build.rs`, OR
2. **Path-dep the cf-do-locator crate** if they want the server types
   re-exported for tests/mocks.

The pattern below assumes option 1.

```toml
# Consumer Cargo.toml
[build-dependencies]
connectrpc-build = "0.4"

[dependencies]
connectrpc = { version = "0.4", default-features = false }
buffa = { version = "0.5", features = ["json"] }
worker = { version = "0.8", features = ["http"] }
```

```rust
// build.rs (consumer)
fn main() {
    connectrpc_build::Config::new()
        .files(&["vendor/cf-do-locator/proto/locator/v1/locator.proto"])
        .includes(&["vendor/cf-do-locator/proto"])
        .compile()
        .expect("compile cf-do-locator proto");
}
```

Then in the consumer's DO-creation funnel (one helper per crate):

```rust
use locator::v1::{GetLocationHintRequest, LocationHint, LocatorServiceClient};

pub async fn create_user_do(
    env: &worker::Env,
    user_id: &str,
    req: &worker::Request,
) -> worker::Result<worker::Stub> {
    let colo = req.cf().and_then(|cf| cf.colo()).unwrap_or_default();

    // Cache the client at isolate level — see CACHING below.
    let client = locator_client(env)?;
    let resp = client
        .get_location_hint(GetLocationHintRequest { colo: colo.clone(), ..Default::default() })
        .await?;

    let ns = env.durable_object("USER_DO")?;
    let id = match resp.hint.value() {
        LocationHint::LOCATION_HINT_UNSPECIFIED | _ if !resp.known => {
            // Unknown colo or no mapping yet — log and create without hint.
            worker::console_log!("do_creation colo={} hint=none user={}", colo, user_id);
            ns.id_from_name(user_id)?
        }
        _ => {
            let hint_str = location_hint_to_str(resp.hint.value());
            worker::console_log!(
                "do_creation colo={} hint={} user={}",
                colo, hint_str, user_id
            );
            // NOTE: id_from_name doesn't take a locationHint. To actually
            // place a NEW DO by hint, use unique_id with options. See
            // workers-rs DurableObjectId::unique_id_with_options.
            ns.id_from_name(user_id)?
        }
    };
    id.get_stub()
}
```

## CACHING — read this before deploying

Every consumer Worker should cache the LocatorService response at isolate
scope. The data only changes weekly, and KV reads from inside the locator
service still cost a few ms per call. A consumer that RPC-calls cf-do-locator
on every request will add 10–30ms p50 to every DO creation.

Two viable patterns:

1. **Call `ListColos` once at isolate init**, populate a local `HashMap`,
   serve all subsequent lookups from memory. Tolerate up to one
   isolate-lifetime of staleness (typically minutes-to-hours).
2. **Use the CF `Cache` API** to memoize the `ListColos` response with a
   ~1h TTL. Slower than option 1 but doesn't require a singleton.

Don't call `GetLocationHint` per request. The service supports it for
ad-hoc lookups (CLI tools, observability checks) but it's not the
intended hot-path API.
