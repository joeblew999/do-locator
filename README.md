# cf-do-locator

**Place every Durable Object near its user, automatically.**

Drop into any Cloudflare Worker in 3 steps. Live: **https://cf-do-locator.gedw99.workers.dev** (interactive demo).

---

## TypeScript — 3 steps

### 1. Vendor the helper into your Worker

```bash
mkdir -p src/locator
curl -L https://raw.githubusercontent.com/joeblew999/cf-do-locator/main/examples/ts-client/src/lookup.ts \
  -o src/locator/lookup.ts
```

Zero dependencies. One file (~200 lines: cache, low-level RPC client, funnel helpers).

### 2. Boot the cache once per isolate

```ts
import { LocatorCache } from "./locator/lookup.ts";

let _cache: LocatorCache | null = null;
export async function locator(env: Env): Promise<LocatorCache> {
  if (!_cache) {
    _cache = new LocatorCache(env.CF_DO_LOCATOR_URL);
    await _cache.load();
  }
  return _cache;
}
```

Set `CF_DO_LOCATOR_URL = "https://cf-do-locator.gedw99.workers.dev"` in your `wrangler.toml` `[vars]`.

### 3. Route every DO creation through the funnel helpers

```ts
import { createUserDO, createTenantDO } from "./locator/lookup.ts";

// At signup — per-user DO, hint from request.cf.colo:
const cache = await locator(env);
const stub = createUserDO(
  cache,
  request.cf?.colo ?? "",
  userId,
  (log) => console.log("do_creation", log),  // Workers Analytics Engine etc.
);

// At org provisioning — per-tenant DO, NEVER admin's edge.
// `ownerRegion` comes from a region-picker UI or billing inference.
const stub = createTenantDO(ownerRegion, tenantId, log);
```

**Done.** Every DO this Worker creates now lives near its user, forever.

---

## Rust (workers-rs) — 3 steps

### 1. Vendor the helper

```bash
mkdir -p src/locator
curl -L https://raw.githubusercontent.com/joeblew999/cf-do-locator/main/examples/rust-client/src/lib.rs \
  -o src/locator/mod.rs
```

Add `ureq`, `serde`, `serde_json` to your `Cargo.toml`. (For wasm32 builds, swap `ureq` for `worker::Fetch` — same shape.)

### 2. Boot the cache once per isolate

```rust
use std::sync::OnceLock;
use crate::locator::LocatorCache;

static CACHE: OnceLock<LocatorCache> = OnceLock::new();

fn locator(env: &worker::Env) -> worker::Result<&'static LocatorCache> {
    Ok(CACHE.get_or_init(|| {
        let url = env.var("CF_DO_LOCATOR_URL").unwrap().to_string();
        let mut c = LocatorCache::new(url);
        c.load().expect("locator boot");
        c
    }))
}
```

### 3. Route every DO creation through the funnel helpers

```rust
use crate::locator::{create_user_do, create_tenant_do, CreationLog};

fn log_creation(log: CreationLog) {
    worker::console_log!("do_creation {:?}", log);
}

// At signup:
let colo = req.cf().map(|cf| cf.colo()).unwrap_or_default();
let do_id = create_user_do(locator(env)?, &colo, &user_id, log_creation);

// At org provisioning:
let do_id = create_tenant_do(&owner_region, &tenant_id, log_creation);
```

**Done.**

---

## See it in action

- **Live interactive demo**: https://cf-do-locator.gedw99.workers.dev/ — visit in a browser. The page detects your edge server-side and shows you which region your DOs should go in.
- **Runnable Rust + TS CLI demos**: [`examples/`](examples/) — `cargo run` / `npm run demo` against either the live service or your own local `wrangler dev`.

## RPC reference

The service contract is in [proto/locator/v1/locator.proto](proto/locator/v1/locator.proto). All RPCs are `POST` + JSON.

| Endpoint | Body | Returns |
|---|---|---|
| `GET /healthz` | — | `ok` |
| `POST /__refresh` | — | Forces a refresh from upstream. Auto-runs Mondays 03:00 UTC. |
| `POST /locator.v1.LocatorService/GetLocationHint` | `{"colo":"SYD"}` | One hint. |
| `POST /locator.v1.LocatorService/GetColoInfo` | `{"colo":"FRA"}` | Full info for one colo. |
| `POST /locator.v1.LocatorService/ListColos` | `{}` | Every colo. **The call consumers cache at boot.** |
| `POST /locator.v1.LocatorService/GetSnapshot` | `{}` | Metadata (version, counts). |

```bash
curl -X POST https://cf-do-locator.gedw99.workers.dev/locator.v1.LocatorService/GetLocationHint \
  -H "Content-Type: application/json" -d '{"colo":"SYD"}'
# → {"hint":"LOCATION_HINT_OC","known":true}
```

## Run the service locally

```bash
mise run worker:dev
curl http://127.0.0.1:8787/__refresh
```

## Deploy your own copy

```bash
mise run mise:install
fnox set -p keychain CLOUDFLARE_API_TOKEN <token>
fnox set -p keychain CLOUDFLARE_ACCOUNT_ID <id>
mise run kv:create                # prints an id
mise run kv:create:preview        # prints another
# paste both into wrangler.toml's [[kv_namespaces]] (KV ids are not secrets)
mise run worker:deploy
curl https://<your-worker>/__refresh
```

---

## Background

### Why this matters

Durable Objects live in one region for their entire lifetime — CF doesn't migrate them. If a Sydney user signs up and the DO lands in Virginia, every read and write that user ever does pays ~200ms instead of ~10ms. The only chance to get it right is **at creation time**, via `newUniqueId({ locationHint })`.

The mapping from "edge the user arrived at" → "right region for their DO" isn't obvious. It depends on measured latency between every CF edge and every DO region, and shifts as CF adds POPs. [Connor Hindley](https://github.com/connyay) (CF engineer) maintains the measurements at [where.durableobjects.live](https://where.durableobjects.live/); this service refreshes that data weekly into KV and exposes it over RPC so every Worker in your stack can share one source of truth.

### The footgun: pick the right colo source per DO type

The funnel helpers enforce this at the type level (`createTenantDO` literally won't accept a request colo — only an explicit region):

- **Per-user data** — `createUserDO(cache, request.cf.colo, ...)`. User signs up where they live; the DO sticks there.
- **Per-team / per-org data** — `createTenantDO(ownerRegion, ...)`. Do **not** use the admin's edge. The admin might be in Singapore provisioning a US-based team — pinning the whole org to APAC would be a disaster you can't undo.
- **Per-session / per-room** — `createSessionDO(cache, request.cf.colo, ...)`. Short-lived; creator's edge ≈ usage edge.

Also: `idFromName` does **not** honor `locationHint` if any DO with that name exists. Only `newUniqueId({ locationHint })` actually places.

### How the weekly refresh works

1. Downloads the colo list from `cloudflarestatus.com`.
2. Downloads measured latency per colo per DO region from `where.durableobjects.live`.
3. For each colo, picks the region with the lowest latency.
4. **Hysteresis** — if the winning region changed since last week, only accept the change if it's at least 15ms (or 20%) faster. Stops the table flapping between near-tied regions.
5. Writes the new table to KV.

Code: [src/refresh.rs](src/refresh.rs).

### Credit

Measurement data + the infrastructure at [where.durableobjects.live](https://where.durableobjects.live/) + the hysteresis trick are work by [Connor Hindley](https://github.com/connyay). His Rust crate [cf-colo-hint](https://github.com/connyay/cf-colo-hint) is the vendor-as-a-static-library alternative if you'd rather not depend on a service.

## License

MIT.
