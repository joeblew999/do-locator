// Reusable consumer pattern: call ListColos once at isolate startup,
// cache the result in module scope, do O(1) local lookups per request.
//
// ConnectRPC speaks plain HTTP + JSON, so this uses `fetch` directly
// rather than a generated client.

// ─────────────────────────────────────────────────────────────────────
// Generated-by-hand proto type mirrors. In a real consumer these would
// come from `buf generate`; we inline them so the demo has zero codegen.
// ─────────────────────────────────────────────────────────────────────

export type LocationHint =
  | "LOCATION_HINT_UNSPECIFIED"
  | "LOCATION_HINT_WNAM"
  | "LOCATION_HINT_ENAM"
  | "LOCATION_HINT_SAM"
  | "LOCATION_HINT_WEUR"
  | "LOCATION_HINT_EEUR"
  | "LOCATION_HINT_APAC"
  | "LOCATION_HINT_OC"
  | "LOCATION_HINT_AFR"
  | "LOCATION_HINT_ME";

export interface Colo {
  code: string;
  name: string;
  cfRegion: string;
  hint?: LocationHint; // proto3 omits default-value enums; treat as UNSPECIFIED
}

export interface Snapshot {
  version: string;
  totalColos: number;
  colosWithHint: number;
}

// ─────────────────────────────────────────────────────────────────────
// Low-level RPC calls — one per service method. Useful for tooling /
// observability / debugging. Real per-request hot path goes through
// `LocatorCache` below.
// ─────────────────────────────────────────────────────────────────────

async function rpc<T>(baseUrl: string, method: string, body: unknown): Promise<T> {
  const resp = await fetch(`${baseUrl}/locator.v1.LocatorService/${method}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!resp.ok) {
    throw new Error(`${method} failed: ${resp.status} ${resp.statusText}`);
  }
  return (await resp.json()) as T;
}

export const Locator = {
  async getSnapshot(baseUrl: string): Promise<Snapshot> {
    const r = await rpc<{ snapshot: Snapshot }>(baseUrl, "GetSnapshot", {});
    return r.snapshot;
  },
  async listColos(baseUrl: string): Promise<{ snapshot: Snapshot; colos: Colo[] }> {
    return rpc(baseUrl, "ListColos", {});
  },
  async getLocationHint(
    baseUrl: string,
    colo: string,
  ): Promise<{ hint: LocationHint; known: boolean }> {
    return rpc(baseUrl, "GetLocationHint", { colo });
  },
  async getColoInfo(
    baseUrl: string,
    colo: string,
  ): Promise<{ colo?: Colo; known: boolean }> {
    return rpc(baseUrl, "GetColoInfo", { colo });
  },
};

// ─────────────────────────────────────────────────────────────────────
// Cache — construct once at Worker isolate boot, reuse every request.
// ─────────────────────────────────────────────────────────────────────

/** Strip the "LOCATION_HINT_" prefix; null for UNSPECIFIED / missing. */
export function hintCode(h: LocationHint | undefined): string | null {
  if (!h || h === "LOCATION_HINT_UNSPECIFIED") return null;
  return h.replace("LOCATION_HINT_", "").toLowerCase();
}

export class LocatorCache {
  private hints: Map<string, LocationHint> = new Map();
  private snapshot: Snapshot | null = null;

  constructor(private baseUrl: string) {}

  /** One-shot ListColos call. Idempotent. */
  async load(): Promise<void> {
    const { snapshot, colos } = await Locator.listColos(this.baseUrl);
    this.snapshot = snapshot;
    this.hints = new Map(
      colos.map((c) => [c.code, c.hint ?? "LOCATION_HINT_UNSPECIFIED"]),
    );
  }

  /** "oc", "wnam", … or null (unknown colo or no mapping). */
  hintFor(colo: string): string | null {
    return hintCode(this.hints.get(colo));
  }

  get version(): string {
    return this.snapshot?.version ?? "(not loaded)";
  }
  get size(): number {
    return this.hints.size;
  }
}

// ─────────────────────────────────────────────────────────────────────
// The DO-creation funnel — the actual point of all this.
//
// Every DO `idFromName` / `newUniqueId` call in your Worker should go
// through one of these helpers. That's where the colo-source policy
// lives (per DO type), and where you log the observability event.
// ─────────────────────────────────────────────────────────────────────

export type DoType = "user" | "tenant" | "session";

export interface CreationLog {
  colo: string; // request.cf.colo OR "_explicit_" for tenant case
  hint: string | null;
  doType: DoType;
  doId: string;
  ts: number;
}

/** Fake `env.X_DO.newUniqueId({ locationHint })` — real consumer calls the
 *  workers-rs / workers-js DO namespace here. We just return a synthetic id. */
function fakeNewUniqueId(hint: string | null): string {
  const region = hint ?? "auto";
  const rand = Math.random().toString(36).slice(2, 10);
  return `do-${region}-${rand}`;
}

/** Per-user DO. Policy: request.cf.colo at signup. */
export function createUserDO(
  cache: LocatorCache,
  reqColo: string,
  userId: string,
  log: (e: CreationLog) => void,
): string {
  const hint = cache.hintFor(reqColo);
  const doId = fakeNewUniqueId(hint);
  log({ colo: reqColo, hint, doType: "user", doId, ts: Date.now() });
  // Real: env.USER_DO.newUniqueId(hint ? { locationHint: hint } : undefined)
  return doId;
}

/** Per-tenant DO. Policy: EXPLICIT region from the owner, NOT request.cf.colo. */
export function createTenantDO(
  ownerRegion: string,
  tenantId: string,
  log: (e: CreationLog) => void,
): string {
  const doId = fakeNewUniqueId(ownerRegion);
  log({
    colo: "_explicit_",
    hint: ownerRegion,
    doType: "tenant",
    doId,
    ts: Date.now(),
  });
  return doId;
}

/** Per-session DO. Policy: request.cf.colo of the creator. */
export function createSessionDO(
  cache: LocatorCache,
  reqColo: string,
  sessionId: string,
  log: (e: CreationLog) => void,
): string {
  const hint = cache.hintFor(reqColo);
  const doId = fakeNewUniqueId(hint);
  log({ colo: reqColo, hint, doType: "session", doId, ts: Date.now() });
  return doId;
}
