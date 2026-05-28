# do-locator

## TL;DR

When your Cloudflare app saves a user's data, the data has to live in some physical data center. If you pick a data center on the other side of the world from the user, every read and write to that data is slow (200ms+) for the rest of that user's life.

This service tells you which data center to use, given where the user is. You ask it "user just arrived at the Sydney edge, where should I put their data?" — it answers "Oceania".

That's it.

---

## The problem, with a concrete example

A user in Sydney signs up to your app.

Cloudflare runs your code at hundreds of edge data centers around the world. The Sydney user's request lands at the Sydney edge (`SYD`).

Your app saves their account state in a thing called a **Durable Object** — Cloudflare's name for a per-user (or per-team, per-room, whatever) chunk of state. **A Durable Object lives in one specific region forever.** Once it's created, Cloudflare won't move it.

If you accidentally create the DO in `wnam` (Western North America) instead of `oc` (Oceania), then every time the Sydney user reads or writes anything, the request flies from Sydney → North America → back. ~200ms each way. Forever.

If you create it in `oc`, it stays near them. ~10ms.

The only chance to get this right is **at the moment of creation**. So at signup time, you have to know: "given the edge they arrived at, what's the right region for their data?"

That mapping (edge → region) isn't obvious. It comes from latency measurements, and it changes as Cloudflare adds new data centers.

## What this service is

A web service. You ask it "given edge X, what region should I use?" It answers.

```bash
curl -X POST https://do-locator.gedw99.workers.dev/locator.v1.LocatorService/GetLocationHint \
  -H "Content-Type: application/json" \
  -d '{"colo":"SYD"}'
# → {"hint":"LOCATION_HINT_OC","known":true}
```

`SYD` is Sydney's edge code. `LOCATION_HINT_OC` is Oceania. Done.

The data updates itself once a week from Cloudflare's own measurements at [where.durableobjects.live](https://where.durableobjects.live/).

## How you'd use it from your code

You don't call this service every time a user signs up — that'd be slow and pointless. You call it **once when your Worker starts up**, cache the whole table in memory, then look up locally.

```ts
// Once, at startup:
const { colos } = await locator.listColos({});
const HINTS = new Map(colos.map(c => [c.code, c.hint]));

// Every signup after that:
const edgeCode = request.cf?.colo ?? "";
const region   = HINTS.get(edgeCode);
env.USER_DO.newUniqueId({ locationHint: region });
```

Three lines per signup, no network call. The full pattern (including how to handle unknown edges, and which kinds of DOs should NOT use the user's edge) is in [examples/ts-client/](examples/ts-client/README.md) and [examples/rust-client/](examples/rust-client/README.md).

## The one footgun you have to know

The hint only matters **when you create the DO**. After that, the DO is stuck in that region forever — even if the user moves to another continent. So:

- **Per-user data** — use the user's edge at signup. Good. They'll mostly stay near where they signed up; even if they roam, the DO can't follow them anyway.
- **Per-team / per-org data** — do NOT use the admin's edge. The admin might be in Singapore creating a US-based company. Let the team owner pick a region explicitly, or wait until the first real user touches it.
- **Per-session / per-room** — use the creator's edge. Sessions are short-lived, so the creator's edge ≈ where it'll be used.

Getting this wrong silently locks your customers into a slow path for the lifetime of their account.

## Endpoints

The service lives at https://do-locator.gedw99.workers.dev.

| Endpoint | What it returns |
|---|---|
| `GET /healthz` | `ok` (liveness probe) |
| `POST /__refresh` | Forces an immediate refresh of the table from upstream. Also runs automatically every Monday 03:00 UTC. |
| `POST /locator.v1.LocatorService/GetLocationHint` `{"colo":"SYD"}` | The hint for one edge code. |
| `POST /locator.v1.LocatorService/GetColoInfo` `{"colo":"FRA"}` | Full info for one edge (name, region, hint). |
| `POST /locator.v1.LocatorService/ListColos` `{}` | The whole table. **This is the one consumers should actually call.** |
| `POST /locator.v1.LocatorService/GetSnapshot` `{}` | Just metadata (date, total colos, count with hints). |

The service contract is in [proto/locator/v1/locator.proto](proto/locator/v1/locator.proto). Consumers vendor that file and run `connectrpc-build` (Rust) or `buf generate` (TS) to make a client.

## Running it locally

```bash
mise run worker:dev
curl http://127.0.0.1:8787/__refresh
curl -X POST http://127.0.0.1:8787/locator.v1.LocatorService/GetLocationHint \
  -H "Content-Type: application/json" -d '{"colo":"SYD"}'
```

`wrangler dev --local` uses a local KV emulator, separate from prod.

## Deploying your own copy

You shouldn't need to — just use the live one. But if you want your own:

```bash
mise run mise:install
fnox set -p keychain CLOUDFLARE_API_TOKEN <token>
fnox set -p keychain CLOUDFLARE_ACCOUNT_ID <id>

mise run kv:create               # prints an id
mise run kv:create:preview       # prints another id
# Paste both into wrangler.toml's [[kv_namespaces]] block.

mise run worker:deploy
curl https://<your-worker>/__refresh   # fills the table for the first time
```

## How the weekly refresh works

A scheduled handler runs Mondays at 03:00 UTC, inside the same Worker:

1. Downloads the list of all Cloudflare edges from `cloudflarestatus.com`.
2. Downloads measured latency between every edge and every DO region from `where.durableobjects.live`.
3. For each edge, picks the region with lowest latency.
4. **Hysteresis:** if the winning region changed since last week, only accept the change if the new region is at least 15ms (or 20%) faster than last week's pick. Stops the table from flapping between two regions that are nearly tied.
5. Writes the new table to KV.

That's the whole refresh. See [src/refresh.rs](src/refresh.rs).

## Credit

The latency measurements are work by [Connor Hindley](https://github.com/connyay), a Cloudflare engineer, at [where.durableobjects.live](https://where.durableobjects.live/). The hysteresis trick is ported from his Rust crate [cf-colo-hint](https://github.com/connyay/cf-colo-hint), which is the same thing as this service but as a static library you vendor — useful if you don't want to depend on another service.

## License

MIT.
