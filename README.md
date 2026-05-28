# do-locator

**A web service that tells you where to place a Cloudflare Durable Object so it's fast for your users.**

Live at https://do-locator.gedw99.workers.dev

```bash
curl -X POST https://do-locator.gedw99.workers.dev/locator.v1.LocatorService/GetLocationHint \
  -H "Content-Type: application/json" \
  -d '{"colo":"SYD"}'
# → {"hint":"LOCATION_HINT_OC","known":true}
```

You give it a Cloudflare data-center code (the one in `request.cf.colo`, e.g. `SYD`, `LAX`, `FRA`). It tells you the best Durable Object region to place the DO in. Sydney → Oceania, LA → Western North America, Frankfurt → Eastern Europe.

## Why this exists

Durable Objects can be placed in specific regions via `locationHint` when you create them. **That hint is permanent.** Once a DO exists, it stays in that region for life. CF does not migrate DOs.

- Pick the right region at creation → user's reads/writes stay sub-10ms forever.
- Pick the wrong region → every read crosses an ocean, ~200ms forever.

The mapping from "colo the user arrived through" → "best DO region" depends on measured latency between every CF colo and every DO region. The data shifts as CF adds POPs and reshuffles traffic. [Connor Hindley](https://github.com/connyay) (a CF engineer) maintains the measurements at [where.durableobjects.live](https://where.durableobjects.live/). This service turns those measurements into a stable, weekly-refreshed lookup with hysteresis (so noisy boundary flips don't churn the answers).

**Why a service instead of a vendored library:** you have many Workers that all create DOs. One service means one weekly refresh, one observability surface, one update for all consumers when CF adds a new POP. Every consumer reads the same data via RPC and caches it locally per isolate.

## How a consumer uses it

```ts
import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { LocatorService } from "./gen/locator/v1/locator_pb.js";

// Once per isolate — populate a local map from the full snapshot:
const client = createClient(LocatorService, createConnectTransport({
  baseUrl: "https://do-locator.gedw99.workers.dev",
}));
const { colos } = await client.listColos({});
const HINTS = new Map(colos.map(c => [c.code, c.hint]));

// Per request — O(1) local lookup, no RPC:
const hint = HINTS.get(request.cf?.colo ?? "");
env.USER_DO.newUniqueId({ locationHint: hintToString(hint) });
```

That's the whole pattern. Don't call `GetLocationHint` per request — call `ListColos` once at isolate boot and cache.

Worked examples (with the funnel-helper pattern, observability log, per-DO-type policy):
- Rust: [examples/rust-client/README.md](examples/rust-client/README.md)
- TS: [examples/ts-client/README.md](examples/ts-client/README.md)

## The locationHint-is-permanent footgun

Different DO types want different "colo sources" at creation:

| DO type | Use this colo source |
|---|---|
| Per-user state | `request.cf.colo` at signup. Sticky-good even if the user roams. |
| Per-session / per-room | `request.cf.colo` at creation. Short-lived. |
| Per-tenant / per-org | **NOT** the admin's request colo. Let the org owner pick a region, or defer creation to first end-user touch. |
| Global singleton | Hardcode one hint. This service is irrelevant. |

Get the per-tenant case wrong and you'll pin every org user to wherever the admin happened to be sitting. Forever. Comment the *why* at your consumer's funnel helper so the next person doesn't "fix" it back to `request.cf.colo`.

Also: `idFromName` does **not** honor `locationHint` once a DO with that name exists anywhere. Only `newUniqueId({ locationHint })` actually places a new DO.

## RPCs

All four are POST + JSON. The service definition is in [proto/locator/v1/locator.proto](proto/locator/v1/locator.proto).

```bash
# Single lookup
curl -X POST $URL/locator.v1.LocatorService/GetLocationHint \
  -H "Content-Type: application/json" -d '{"colo":"SYD"}'
# → {"hint":"LOCATION_HINT_OC","known":true}

# Full info for one colo
curl -X POST $URL/locator.v1.LocatorService/GetColoInfo \
  -H "Content-Type: application/json" -d '{"colo":"FRA"}'
# → {"colo":{"code":"FRA","name":"Frankfurt, Germany","cfRegion":"Europe","hint":"LOCATION_HINT_EEUR"},"known":true}

# Every colo + snapshot version (the call consumers actually use)
curl -X POST $URL/locator.v1.LocatorService/ListColos \
  -H "Content-Type: application/json" -d '{}'

# Metadata only
curl -X POST $URL/locator.v1.LocatorService/GetSnapshot \
  -H "Content-Type: application/json" -d '{}'
# → {"snapshot":{"version":"2026-05-28","totalColos":340,"colosWithHint":278}}
```

Plus a plain `GET /healthz` for liveness probes.

## Deploying your own

```bash
mise run mise:install
fnox set -p keychain CLOUDFLARE_API_TOKEN <token>
fnox set -p keychain CLOUDFLARE_ACCOUNT_ID <id>

mise run kv:create               # → prints a namespace id
mise run kv:create:preview       # → prints a preview id
# Paste both ids into wrangler.toml's [[kv_namespaces]] block.
# KV ids are not secrets; commit them.

mise run worker:deploy           # build wasm + push
curl https://<your-worker>/__refresh   # populate KV (also runs weekly via cron)
```

## Running locally

```bash
mise run worker:dev          # wrangler dev --local on :8787
curl http://127.0.0.1:8787/__refresh
curl -X POST http://127.0.0.1:8787/locator.v1.LocatorService/GetLocationHint \
  -H "Content-Type: application/json" -d '{"colo":"SYD"}'
```

`wrangler dev --local` uses a local KV emulator — separate state from prod.

## How the refresh works

A scheduled handler runs Mondays at 03:00 UTC, in this same Worker:

1. HTTP GET `cloudflarestatus.com/api/v2/components.json` (colo names + CF region groupings).
2. HTTP GET `where.durableobjects.live/api/v3/data.json` (measured latency per colo per DO region).
3. Merge into one record per colo.
4. **Hysteresis:** if a colo's nearest region flipped vs the previous snapshot, only accept the flip if the new region is at least `max(15ms, 20%)` faster than the previously-chosen one. Region-boundary colos often differ by 2-3ms between regions and the raw winner bounces every refresh — this keeps the mapping stable.
5. Write the new snapshot JSON to KV.

To trigger a refresh outside the weekly schedule: `curl <url>/__refresh`. The data is public, so the endpoint is unauthenticated.

## Credit

Mapping data, the measurement infrastructure at [where.durableobjects.live](https://where.durableobjects.live/), and the hysteresis approach come from [Connor Hindley](https://github.com/connyay) (CF). His Rust crate [cf-colo-hint](https://github.com/connyay/cf-colo-hint) is the vendor-it-as-a-library alternative to this service.

## License

MIT.
