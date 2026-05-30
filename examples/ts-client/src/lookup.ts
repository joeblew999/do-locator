// The reusable consumer pattern: call ListColos once at isolate startup,
// cache the result in module scope, do O(1) local lookups per request.
//
// ConnectRPC speaks plain HTTP + JSON, so we use `fetch` directly here
// rather than generating a client. For a typed client see the README.

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
  hint: LocationHint;
}

export interface Snapshot {
  version: string;
  totalColos: number;
  colosWithHint: number;
}

export interface ListColosResponse {
  snapshot: Snapshot;
  colos: Colo[];
}

/** Strip the "LOCATION_HINT_" prefix; returns null for UNSPECIFIED. */
export function hintCode(h: LocationHint): string | null {
  if (h === "LOCATION_HINT_UNSPECIFIED") return null;
  return h.replace("LOCATION_HINT_", "").toLowerCase();
}

/**
 * Consumer cache. Construct once at Worker isolate boot; reuse across
 * every request.
 */
export class LocatorCache {
  private hints: Map<string, LocationHint> = new Map();
  private snapshot: Snapshot | null = null;

  constructor(private baseUrl: string) {}

  /** Fetch the full table once. Idempotent — safe to call again later. */
  async load(): Promise<void> {
    const resp = await fetch(
      `${this.baseUrl}/locator.v1.LocatorService/ListColos`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: "{}",
      },
    );
    if (!resp.ok) {
      throw new Error(`ListColos failed: ${resp.status} ${resp.statusText}`);
    }
    const data = (await resp.json()) as ListColosResponse;
    this.snapshot = data.snapshot;
    this.hints = new Map(data.colos.map((c) => [c.code, c.hint]));
  }

  /** Return the hint code ("oc", "wnam", ...) for an edge code, or null. */
  hintFor(colo: string): string | null {
    const h = this.hints.get(colo);
    if (h === undefined) return null;
    return hintCode(h);
  }

  get version(): string {
    return this.snapshot?.version ?? "(not loaded)";
  }
  get size(): number {
    return this.hints.size;
  }
}
