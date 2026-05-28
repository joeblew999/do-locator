# do-locator

**Cloudflare colo → Durable Objects `locationHint` lookup service. ConnectRPC on Workers, KV-backed, weekly refresh.**

When a Worker creates a Durable Object, passing a `locationHint` places the DO close to the requesting user. The hint is permanent — DOs do not migrate. Picking it well at creation time is the difference between sub-10ms reads and cross-region round-trips.

This service is the single runtime source of truth for that mapping across every consuming Worker. One service, one KV blob, many ConnectRPC consumers.

## Architecture

```
                    Layer 0  upstream (CF / Connor Hindley)
                              │
                ┌─────────────┴─────────────┐
                ▼                           ▼
     cloudflarestatus.com           where.durableobjects.live
                │                           │
                └──────────┬────────────────┘
                           ▼
        ┌────────────────────────────────────────┐
        │  Layer 1  scheduled refresh (weekly)   │
        │  src/refresh.rs                        │
        │  Mondays 03:00 UTC, in this Worker     │
        │  - HTTP GET both upstreams             │
        │  - merge + hysteresis vs prev snapshot │
        │  - KV PUT new snapshot                 │
        └────────────────────────────────────────┘
                           │
                           ▼
        ┌────────────────────────────────────────┐
        │  Layer 2  KV blob "snapshot"           │
        │  ~50KB JSON, ~340 colos, weekly diff   │
        └────────────────────────────────────────┘
                           │
                           ▼
        ┌────────────────────────────────────────┐
        │  Layer 3  ConnectRPC service           │
        │  src/service.rs                        │
        │  - GetLocationHint(colo) → hint        │
        │  - GetColoInfo(colo)     → full info   │
        │  - ListColos()           → all colos   │
        │  - GetSnapshot()         → metadata    │
        └────────────────────────────────────────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
       consumer-1     consumer-2    consumer-N
        (Rust)         (TS)          (any lang
        Worker         Worker        with proto)
```

## Consumer pattern

**Don't RPC us per request.** Call `ListColos` once at isolate init, cache in memory, do all per-request lookups locally. The data changes weekly at most.

- Rust example: [examples/rust-client/](examples/rust-client/README.md)
- TS example: [examples/ts-client/](examples/ts-client/README.md)

Both examples include the **DO-creation funnel** pattern (one helper per consumer Worker, all `idFromName`/`newUniqueId` go through it) and the **observability log** every consumer should emit.

## Critical: locationHint is PERMANENT

`locationHint` applies only at DO **creation**. DOs do not migrate. Consequences:

| DO type | Right colo source at creation |
|---|---|
| Per-user state | `request.cf.colo` at signup — user's home colo is sticky-good even if they roam. |
| Per-session / per-room | `request.cf.colo` at creation. Short-lived so creation-colo ≈ usage-colo. |
| Per-tenant / per-org | **NOT** `request.cf.colo` of the admin. Use explicit region from owner, billing-address inference, or defer creation to first end-user touch. |
| Globally shared singleton | Hardcode one hint. This service is irrelevant. |

Comment the *why* at the funnel helper site. Someone six months later will "fix" it to always use `request.cf.colo` and silently mis-locate every tenant DO.

`idFromName` is deterministic across colos and effectively ignores the hint once any DO with that name has been created anywhere. Hints only bind at `newUniqueId({ locationHint })`.

## Hysteresis

Region nearest-neighbour latencies often differ by only a few ms for colos at region boundaries — the raw upstream winner flaps between refreshes. The refresh handler keeps the previous decision unless the new region is at least `max(15ms, 20%)` faster. Logic ported from [connyay/cf-colo-hint](https://github.com/connyay/cf-colo-hint).

## Repo layout

```
do-locator/
├── proto/
│   └── locator/v1/locator.proto    Service contract; consumers vendor + codegen
├── src/
│   ├── lib.rs                      fetch + scheduled event handlers
│   ├── service.rs                  LocatorService impl
│   ├── refresh.rs                  download + hysteresis + KV put
│   ├── snapshot.rs                 KV blob shape + proto conversions
│   └── state.rs                    AppState (KV binding)
├── examples/
│   ├── rust-client/README.md       consumer Worker pattern (Rust)
│   └── ts-client/README.md         consumer Worker pattern (TS)
├── data/                           test fixtures only (not the canonical state — KV is)
├── build.rs                        connectrpc-build → src/proto codegen
├── Cargo.toml                      cdylib + rlib, edition 2024
├── wrangler.toml                   KV binding + cron trigger
├── fnox.toml                       keychain-backed secret contract
├── mise.toml                       task pipeline (cargo:*, worker:*, kv:*, dev:*)
├── pitchfork.toml                  dev daemon supervisor
└── CLAUDE.md                       conventions for AI agents
```

## First deploy

```bash
mise run mise:install                                # install all CLIs
fnox set -p keychain CLOUDFLARE_API_TOKEN <token>    # if not already set
fnox set -p keychain CLOUDFLARE_ACCOUNT_ID <id>      # likewise
mise run cf:check                                    # verify CF auth
mise run kv:create                                   # → returns KV namespace id
fnox set -p keychain DO_LOCATOR_KV_NAMESPACE_ID <id> # save it
mise run kv:create:preview                           # → preview namespace
fnox set -p keychain DO_LOCATOR_KV_PREVIEW_ID <id>   # save that too
mise run worker:deploy                               # ship it
mise run worker:tail                                 # tail logs while cron fires
```

The first `scheduled` event won't fire until the next Monday. To bootstrap immediately, invoke the scheduled handler manually:

```bash
fnox exec -- wrangler triggers schedule do-locator
```

## Sanity-check a deployed instance with curl

ConnectRPC speaks plain HTTP+JSON; you don't need a generated client to verify a deployment.

```bash
URL=https://do-locator.<your-account>.workers.dev

# Health probe (no RPC machinery)
curl "$URL/healthz"

# Snapshot metadata — version, total colos, with-hint count
curl -X POST "$URL/locator.v1.LocatorService/GetSnapshot" \
  -H "Content-Type: application/json" \
  -d '{}'

# Single colo lookup
curl -X POST "$URL/locator.v1.LocatorService/GetLocationHint" \
  -H "Content-Type: application/json" \
  -d '{"colo":"SYD"}'
# → {"hint":"LOCATION_HINT_OC","known":true}

# Full info
curl -X POST "$URL/locator.v1.LocatorService/GetColoInfo" \
  -H "Content-Type: application/json" \
  -d '{"colo":"SYD"}'

# List every colo (response is ~50KB — consumers cache this per isolate)
curl -X POST "$URL/locator.v1.LocatorService/ListColos" \
  -H "Content-Type: application/json" \
  -d '{}'
```

If `GetSnapshot` returns `503` with "snapshot not yet populated", the cron hasn't run yet — kick it manually:

```bash
mise run kv:bootstrap
```

## Refreshing locally

The refresh pipeline runs **inside the Worker** — there's no nushell or Python codegen anymore. To test the logic:

```bash
mise run cargo:test    # unit tests for parse_iata_suffix + hysteresis
mise run worker:dev    # local wrangler, RPC against local KV
```

## Credit

Hysteresis approach + the data sources (cloudflarestatus.com + where.durableobjects.live) are work by [Connor Hindley](https://github.com/connyay), a Cloudflare engineer. See his Rust crate [cf-colo-hint](https://github.com/connyay/cf-colo-hint) for the vendored-library shape if you want to skip the service.

## License

MIT.
