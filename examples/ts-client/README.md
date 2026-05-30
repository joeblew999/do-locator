# cf-do-locator TS consumer example

A runnable demonstration of the consumer pattern: call `ListColos` once at boot, cache the table in memory, do O(1) local lookups per request.

## Run it

Against local wrangler dev (default URL `http://127.0.0.1:8787`):

```bash
npm install
npm run demo
```

Against the live service:

```bash
npm run demo:prod
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

- [`src/lookup.ts`](src/lookup.ts) — the reusable `LocatorCache` class. Copy this into your Worker.
- [`src/cli.ts`](src/cli.ts) — the demo driver.

## Using `LocatorCache` in a real Worker

```ts
import { LocatorCache } from "./lookup.ts";

// Module scope — one cache per isolate. Lazy-init on first request.
let _cache: LocatorCache | null = null;
async function locator(env: Env): Promise<LocatorCache> {
  if (!_cache) {
    _cache = new LocatorCache(env.CF_DO_LOCATOR_URL);
    await _cache.load();
  }
  return _cache;
}

// In your DO-creation funnel:
export async function createUserDO(req: Request, env: Env, userId: string) {
  const cache = await locator(env);
  const hint = cache.hintFor(req.cf?.colo ?? "");
  // ⚠️  hint is only honoured by newUniqueId, NOT idFromName.
  const id = env.USER_DO.newUniqueId(
    hint ? { locationHint: hint } : undefined,
  );
  // Observability: log {colo, hint, do_type, do_id} here.
  return env.USER_DO.get(id);
}
```

## Per-DO-type policy

Different DO types want different "colo sources" — see the [parent README's "one footgun" section](../../README.md#the-one-footgun). Briefly:

- Per-user — use `req.cf.colo` at signup.
- Per-team / per-org — do **not** use the admin's edge; let the team owner pick a region.
- Per-session / per-room — use the creator's edge.

## Typed client (optional)

This demo uses raw `fetch` + JSON, which works because ConnectRPC accepts that natively. For a fully-typed client you can generate one from the proto:

```bash
npx -y @bufbuild/buf@latest generate
# uses ../../buf.gen.yaml against ../../proto/locator/v1/locator.proto
```

The output drops into `gen/`. Then import the `LocatorService` client from `@connectrpc/connect` and replace the raw `fetch` calls. The cache+lookup pattern stays the same.
