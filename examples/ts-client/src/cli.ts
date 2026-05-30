// Demonstrates every public-facing feature of cf-do-locator:
//
//   1. GetSnapshot         — metadata probe before doing anything else
//   2. ListColos           — populates the in-isolate cache (the hot path)
//   3. GetLocationHint     — single ad-hoc lookup (cross-checks cache)
//   4. GetColoInfo         — full info for one colo
//   5. The funnel pattern  — createUserDO / createTenantDO / createSessionDO
//      with observability log + the correct policy per DO type

import {
  LocatorCache,
  Locator,
  createUserDO,
  createTenantDO,
  createSessionDO,
  type CreationLog,
} from "./lookup.ts";

const URL = process.env.CF_DO_LOCATOR_URL ?? "http://127.0.0.1:8787";

function section(title: string) {
  console.log();
  console.log(`── ${title} ${"─".repeat(60 - title.length)}`);
}

function logCreation(e: CreationLog) {
  // Real consumer would send this to Workers Analytics Engine / Logpush.
  console.log(
    `    [log] ${e.doType.padEnd(8)} colo=${e.colo.padEnd(11)} hint=${(e.hint ?? "(none)").padEnd(6)} do_id=${e.doId}`,
  );
}

async function main() {
  console.log(`URL: ${URL}`);

  // ── 1. GetSnapshot — cheap health/metadata check ──────────────────
  section("GetSnapshot");
  const snap = await Locator.getSnapshot(URL);
  console.log(`  version:         ${snap.version}`);
  console.log(`  total colos:     ${snap.totalColos}`);
  console.log(`  colos with hint: ${snap.colosWithHint}`);

  // ── 2. ListColos — the call consumers actually do, once per isolate ─
  section("ListColos → LocatorCache");
  const cache = new LocatorCache(URL);
  await cache.load();
  console.log(`  cached ${cache.size} colos (snapshot ${cache.version})`);

  // ── 3. GetLocationHint — ad-hoc single lookup, useful for CLIs / probes ─
  section("GetLocationHint(SYD)");
  const single = await Locator.getLocationHint(URL, "SYD");
  console.log(`  hint:  ${single.hint}`);
  console.log(`  known: ${single.known}`);
  console.log(`  (cache says: ${cache.hintFor("SYD")} — should match)`);

  // ── 4. GetColoInfo — full info: human name, CF region, hint ───────
  section("GetColoInfo(FRA)");
  const info = await Locator.getColoInfo(URL, "FRA");
  if (info.colo) {
    console.log(`  code:     ${info.colo.code}`);
    console.log(`  name:     ${info.colo.name}`);
    console.log(`  cfRegion: ${info.colo.cfRegion}`);
    console.log(`  hint:     ${info.colo.hint}`);
  }

  // ── 5. The funnel pattern — every DO creation goes through one of these ─
  section("Funnel pattern: per-user DO (use request.cf.colo)");
  // Two users sign up at different edges. Each gets a DO placed near them.
  createUserDO(cache, "SYD", "alice", logCreation);
  createUserDO(cache, "LAX", "bob",   logCreation);

  section("Funnel pattern: per-tenant DO (NEVER use admin's edge)");
  // Admin in Singapore (SIN) creates a US-based team. Wrong: use admin.cf.colo
  // (would pin all the team's data to APAC forever). Right: owner-picked region.
  const teamOwnerRegion = "wnam"; // explicit, from a region-picker UI
  createTenantDO(teamOwnerRegion, "team-acme", logCreation);
  // Same admin creates a EU-based team — different region.
  createTenantDO("weur", "team-globex", logCreation);

  section("Funnel pattern: per-session DO (creator's edge is fine)");
  createSessionDO(cache, "FRA", "chat-room-42",  logCreation);
  createSessionDO(cache, "NRT", "chat-room-43",  logCreation);

  section("Edge cases");
  // Unknown colo (new POP not yet in the table) — should log and create with no hint.
  createUserDO(cache, "XXX", "carol", logCreation);
  // Empty colo (request.cf was missing) — same treatment.
  createUserDO(cache, "",    "dave",  logCreation);

  console.log();
  console.log("✓ exercised every RPC + every funnel-helper code path");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
