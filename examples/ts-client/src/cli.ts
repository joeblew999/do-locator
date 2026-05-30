// Demo CLI for the LocatorCache pattern.
//
// Run with:
//   npm install         # one-time
//   npm run demo        # against local wrangler dev (default)
//   npm run demo:prod   # against https://cf-do-locator.gedw99.workers.dev

import { LocatorCache } from "./lookup.ts";

const URL = process.env.CF_DO_LOCATOR_URL ?? "http://127.0.0.1:8787";

async function main() {
  console.log(`→ cache.load() from ${URL}`);
  const cache = new LocatorCache(URL);
  await cache.load();
  console.log(`  snapshot version: ${cache.version}`);
  console.log(`  colos cached:     ${cache.size}`);
  console.log();

  // Demo lookups: every consumer per-request line looks like this.
  const cases = ["SYD", "LAX", "FRA", "JNB", "XXX"];
  for (const code of cases) {
    const hint = cache.hintFor(code);
    const display = hint ?? "(unknown — create DO without hint, log it)";
    console.log(`  ${code}  →  ${display}`);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
