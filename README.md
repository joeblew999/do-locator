# cf-do-locator

Tells you which Cloudflare region to place a Durable Object in, given the edge a user arrived at.

Live at **https://cf-do-locator.gedw99.workers.dev**.

```bash
curl -X POST https://cf-do-locator.gedw99.workers.dev/locator.v1.LocatorService/GetLocationHint \
  -H "Content-Type: application/json" \
  -d '{"colo":"SYD"}'
# → {"hint":"LOCATION_HINT_OC","known":true}
```

## Use it

Call once at startup, cache the table, look up locally per request:

```ts
const { colos } = await locator.listColos({});
const HINTS = new Map(colos.map(c => [c.code, c.hint]));

// per signup:
const hint = HINTS.get(request.cf?.colo ?? "");
env.USER_DO.newUniqueId({ locationHint: hint });
```

Full pattern (Rust + TS, with the per-DO-type policy): [examples/rust-client/](examples/rust-client/README.md), [examples/ts-client/](examples/ts-client/README.md).

## Endpoints

Service contract: [proto/locator/v1/locator.proto](proto/locator/v1/locator.proto).

| Endpoint | Body | Returns |
|---|---|---|
| `GET /healthz` | — | `ok` |
| `POST /__refresh` | — | Forces a refresh from upstream. Also runs Mondays 03:00 UTC via cron. |
| `POST /locator.v1.LocatorService/GetLocationHint` | `{"colo":"SYD"}` | One hint. |
| `POST /locator.v1.LocatorService/GetColoInfo` | `{"colo":"FRA"}` | Full info for one colo. |
| `POST /locator.v1.LocatorService/ListColos` | `{}` | Every colo. **What consumers should call.** |
| `POST /locator.v1.LocatorService/GetSnapshot` | `{}` | Metadata (date, counts). |

## Run locally

```bash
mise run worker:dev
curl http://127.0.0.1:8787/__refresh
curl -X POST http://127.0.0.1:8787/locator.v1.LocatorService/GetLocationHint \
  -H "Content-Type: application/json" -d '{"colo":"SYD"}'
```

## Deploy your own

You shouldn't need to — use the live one. But if you want your own:

```bash
mise run mise:install
fnox set -p keychain CLOUDFLARE_API_TOKEN <token>
fnox set -p keychain CLOUDFLARE_ACCOUNT_ID <id>
mise run kv:create               # prints an id
mise run kv:create:preview       # prints another id
# Paste both ids into wrangler.toml's [[kv_namespaces]] block (they're not secrets).
mise run worker:deploy
curl https://<your-worker>/__refresh
```

---

## Background

### Why this exists

A user in Sydney signs up. Their request lands at the Sydney edge (`SYD`). Your app stores their account state in a **Durable Object** — Cloudflare's per-user (or per-team, per-room) chunk of state.

A DO lives in one region forever. CF won't migrate it.

- DO in `oc` (Oceania) → ~10ms reads for the Sydney user, forever.
- DO in `wnam` (Western North America) → ~200ms reads, forever.

The only chance to get this right is at creation time, via the `locationHint` argument. You need to know: "given the edge they arrived at, which region is closest?" The mapping isn't obvious — it depends on measured latency between every edge and every DO region, and it shifts as CF adds POPs.

This service publishes that mapping. The data refreshes weekly from [where.durableobjects.live](https://where.durableobjects.live/), maintained by Cloudflare engineer [Connor Hindley](https://github.com/connyay).

### The one footgun

The hint binds only at creation. So:

- **Per-user data** — use the user's edge at signup. They'll mostly stay near it; even if they roam, the DO can't follow them anyway.
- **Per-team / per-org data** — do NOT use the admin's edge. The admin might be in Singapore provisioning a US-based team. Let the team owner pick a region, or defer creation to first end-user touch.
- **Per-session / per-room** — use the creator's edge. Short-lived; creator's edge ≈ usage edge.

Comment the *why* at your consumer's funnel helper so the next person doesn't "fix" it back to `request.cf.colo` and silently mis-locate every team DO.

Also: `idFromName` does NOT honor `locationHint` once a DO with that name exists anywhere. Only `newUniqueId({ locationHint })` actually places a new DO.

### How the weekly refresh works

1. Downloads the colo list from `cloudflarestatus.com`.
2. Downloads measured latency per colo per DO region from `where.durableobjects.live`.
3. For each colo, picks the region with the lowest latency.
4. **Hysteresis:** if the winning region changed since last week, only accept the change if it's at least 15ms (or 20%) faster. Stops the table flapping between near-tied regions.
5. Writes the new table to KV.

Code: [src/refresh.rs](src/refresh.rs).

### Credit

Measurement data, the infrastructure at [where.durableobjects.live](https://where.durableobjects.live/), and the hysteresis trick are work by [Connor Hindley](https://github.com/connyay). His Rust crate [cf-colo-hint](https://github.com/connyay/cf-colo-hint) is the vendor-it-as-a-library alternative if you don't want to depend on a service.

## License

MIT.
