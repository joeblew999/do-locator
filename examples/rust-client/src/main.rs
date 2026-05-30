// Demonstrates every public-facing feature of cf-do-locator:
//
//   1. GetSnapshot         — metadata probe
//   2. ListColos           — populates the in-isolate cache (hot path)
//   3. GetLocationHint     — single ad-hoc lookup
//   4. GetColoInfo         — full info for one colo
//   5. The funnel pattern  — create_user_do / create_tenant_do / create_session_do
//      with observability log + the correct policy per DO type

use cf_do_locator_rust_example::{
    CreationLog, Locator, LocatorCache, create_session_do, create_tenant_do, create_user_do,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("CF_DO_LOCATOR_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8787".to_string());
    println!("URL: {url}");

    let rpc = Locator::new(&url);

    // ── 1. GetSnapshot ─────────────────────────────────────────────────
    section("GetSnapshot");
    let snap = rpc.get_snapshot()?;
    println!("  version:         {}", snap.version);
    println!("  total colos:     {}", snap.total_colos);
    println!("  colos with hint: {}", snap.colos_with_hint);

    // ── 2. ListColos → cache ───────────────────────────────────────────
    section("ListColos → LocatorCache");
    let mut cache = LocatorCache::new(&url);
    cache.load()?;
    println!("  cached {} colos (snapshot {})", cache.size(), cache.version());

    // ── 3. GetLocationHint ─────────────────────────────────────────────
    section("GetLocationHint(SYD)");
    let single = rpc.get_location_hint("SYD")?;
    println!("  hint:  {}", single.hint);
    println!("  known: {}", single.known);
    println!("  (cache says: {:?} — should match)", cache.hint_for("SYD"));

    // ── 4. GetColoInfo ─────────────────────────────────────────────────
    section("GetColoInfo(FRA)");
    let info = rpc.get_colo_info("FRA")?;
    if let Some(c) = info.colo {
        println!("  code:     {}", c.code);
        println!("  name:     {}", c.name);
        println!("  cfRegion: {}", c.cf_region);
        println!("  hint:     {}", c.hint);
    }

    // ── 5. Funnel pattern: per-user DO ─────────────────────────────────
    section("Funnel pattern: per-user DO (use request.cf.colo)");
    create_user_do(&cache, "SYD", "alice", log_creation);
    create_user_do(&cache, "LAX", "bob", log_creation);

    // ── Funnel pattern: per-tenant DO ──────────────────────────────────
    section("Funnel pattern: per-tenant DO (NEVER use admin's edge)");
    // Admin in Singapore creates a US-based team. Wrong: use admin.cf.colo.
    // Right: explicit owner-picked region.
    create_tenant_do("wnam", "team-acme", log_creation);
    create_tenant_do("weur", "team-globex", log_creation);

    // ── Funnel pattern: per-session DO ─────────────────────────────────
    section("Funnel pattern: per-session DO (creator's edge is fine)");
    create_session_do(&cache, "FRA", "chat-room-42", log_creation);
    create_session_do(&cache, "NRT", "chat-room-43", log_creation);

    // ── Edge cases ─────────────────────────────────────────────────────
    section("Edge cases");
    create_user_do(&cache, "XXX", "carol", log_creation); // unknown POP
    create_user_do(&cache, "", "dave", log_creation); // missing cf.colo

    println!();
    println!("✓ exercised every RPC + every funnel-helper code path");
    Ok(())
}

fn section(title: &str) {
    let dashes = "─".repeat(60usize.saturating_sub(title.chars().count()));
    println!();
    println!("── {title} {dashes}");
}

fn log_creation(e: CreationLog) {
    // Real consumer would send this to Workers Analytics Engine / Logpush.
    println!(
        "    [log] {:<8} colo={:<11} hint={:<6} do_id={}",
        e.do_type.as_str(),
        e.colo,
        e.hint.as_deref().unwrap_or("(none)"),
        e.do_id,
    );
}
