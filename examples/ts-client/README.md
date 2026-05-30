# cf-do-locator TS consumer

The reusable cache + funnel helpers that any TS Cloudflare Worker should adopt to get DOs placed in the right region.

## Drop it into your Worker

```bash
mkdir -p src/locator
curl -L https://raw.githubusercontent.com/joeblew999/cf-do-locator/main/examples/ts-client/src/lookup.ts \
  -o src/locator/lookup.ts
```

That's the entire library. Zero deps. ~200 lines: low-level RPC client (`Locator`), in-isolate cache (`LocatorCache`), and the three DO-creation funnel helpers (`createUserDO`, `createTenantDO`, `createSessionDO`).

## Wire it in

```ts
// src/worker.ts (or wherever your fetch handler lives)
import { LocatorCache, createUserDO, createTenantDO, type CreationLog } from "./locator/lookup.ts";

interface Env {
  CF_DO_LOCATOR_URL: string;          // set in wrangler.toml [vars]
  USER_DO:   DurableObjectNamespace;
  TENANT_DO: DurableObjectNamespace;
}

// One cache per isolate. Lazy-init on first request.
let _cache: LocatorCache | null = null;
async function locator(env: Env) {
  if (!_cache) {
    _cache = new LocatorCache(env.CF_DO_LOCATOR_URL);
    await _cache.load();              // single ListColos call
  }
  return _cache;
}

function logCreation(e: CreationLog) {
  // Workers Analytics Engine, Logpush, plain console.log — your choice.
  console.log("do_creation", JSON.stringify(e));
}

// ── DO-creation funnel — every idFromName / newUniqueId in your codebase
//    should go through one of these helpers. They enforce the per-DO-type
//    policy at the type level (e.g. createTenantDO can't take a request colo). ──

export async function signup(req: Request, env: Env, userId: string) {
  const cache  = await locator(env);
  const reqColo = (req.cf?.colo as string | undefined) ?? "";
  const doId   = createUserDO(cache, reqColo, userId, logCreation);
  // Your real code calls env.USER_DO.newUniqueId({locationHint}) inside
  // createUserDO; the demo helper returns a fake id for runnability.
  return env.USER_DO.get(env.USER_DO.idFromString(doId));
}

export async function provisionOrg(env: Env, tenantId: string, ownerRegion: string) {
  // NOT request.cf.colo — the admin might be in Singapore provisioning a
  // US-based team. Hint binds at creation and is permanent.
  const doId = createTenantDO(ownerRegion, tenantId, logCreation);
  return env.TENANT_DO.get(env.TENANT_DO.idFromString(doId));
}
```

## See it work — run the demo CLI

The same `lookup.ts` powers a CLI demo that exercises every RPC and every funnel helper. Run it against either local `wrangler dev` or the live service.

```bash
cd examples/ts-client
npm install
npm run demo            # → http://127.0.0.1:8787 (default)
npm run demo:prod       # → https://cf-do-locator.gedw99.workers.dev
```

Output is the canonical sanity check — if your `lookup.ts` is wired right, you'll see this:

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

- [`src/lookup.ts`](src/lookup.ts) — the file you vendor into your Worker.
- [`src/cli.ts`](src/cli.ts) — the demo driver (don't vendor; just for `npm run demo`).
