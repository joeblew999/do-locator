# TS consumer pattern

A consumer Worker (or browser) calls do-locator over ConnectRPC using
`@connectrpc/connect-web` + `@bufbuild/protobuf` clients generated from
`proto/locator/v1/locator.proto`.

## Setup

```json
// consumer package.json
{
  "scripts": {
    "proto:gen": "buf generate"
  },
  "devDependencies": {
    "@bufbuild/buf": "latest",
    "@bufbuild/protoc-gen-es": "latest"
  },
  "dependencies": {
    "@connectrpc/connect": "latest",
    "@connectrpc/connect-web": "latest",
    "@bufbuild/protobuf": "latest"
  }
}
```

```yaml
# consumer buf.gen.yaml
version: v2
inputs:
  - directory: ../do-locator/proto    # git submodule path
plugins:
  - local: protoc-gen-es
    out: src/gen
    opt: target=ts
```

## DO-creation funnel — one helper per Worker

```ts
import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { LocatorService, LocationHint } from "./gen/locator/v1/locator_pb.js";

// Cache at module scope — one client per isolate.
let _client: ReturnType<typeof createClient<typeof LocatorService>> | null = null;
let _colos: Map<string, LocationHint> | null = null;

function getLocatorClient(env: Env) {
  if (!_client) {
    _client = createClient(
      LocatorService,
      createConnectTransport({ baseUrl: env.DO_LOCATOR_URL }),
    );
  }
  return _client;
}

// Populate the in-isolate map once. Subsequent requests are O(1) memory lookups,
// not RPCs.
async function ensureColos(env: Env): Promise<Map<string, LocationHint>> {
  if (!_colos) {
    const client = getLocatorClient(env);
    const { colos } = await client.listColos({});
    _colos = new Map(colos.map((c) => [c.code, c.hint]));
  }
  return _colos;
}

interface Env {
  USER_DO: DurableObjectNamespace;
  DO_LOCATOR_URL: string;  // e.g. "https://do-locator.<account>.workers.dev"
}

export async function createUserDO(
  request: Request,
  env: Env,
  userId: string,
): Promise<DurableObjectStub> {
  const colo = (request.cf?.colo as string | undefined) ?? "";
  const colos = await ensureColos(env);
  const hint = colos.get(colo);

  // `idFromName` is deterministic and won't honour locationHint after first
  // creation. For NEW DOs that should be placed by hint, use `newUniqueId`.
  const id = env.USER_DO.idFromName(userId);

  console.log(
    "do_creation",
    JSON.stringify({
      colo,
      hint: hint === undefined || hint === LocationHint.UNSPECIFIED
        ? null
        : LocationHint[hint],
      do_type: "user",
      do_id: id.toString(),
      ts: Date.now(),
    }),
  );

  return env.USER_DO.get(id);
}
```

## Don't call GetLocationHint per request

`ListColos` returns ~340 colos at ~50KB. One call per isolate is fine.
`GetLocationHint` per DO creation is what you want to AVOID — it adds
10–30ms p50 to every creation path. The intended API is ListColos +
local cache; GetLocationHint exists for CLI tools and observability
checks.
