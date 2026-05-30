// Demo CLI for the LocatorCache pattern.
//
// Run with:
//   cargo run                # against local wrangler dev (default)
//   CF_DO_LOCATOR_URL=https://cf-do-locator.gedw99.workers.dev cargo run

use cf_do_locator_rust_example::LocatorCache;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("CF_DO_LOCATOR_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8787".to_string());

    println!("→ cache.load() from {url}");
    let mut cache = LocatorCache::new(&url);
    cache.load()?;
    println!("  snapshot version: {}", cache.version());
    println!("  colos cached:     {}", cache.size());
    println!();

    for code in ["SYD", "LAX", "FRA", "JNB", "XXX"] {
        let display = match cache.hint_for(code) {
            Some(h) => h,
            None => "(unknown — create DO without hint, log it)".to_string(),
        };
        println!("  {code}  →  {display}");
    }
    Ok(())
}
