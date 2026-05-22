use crate::stats::SemLifetimeStats;

pub fn run() {
    let stats = SemLifetimeStats::load();

    if stats.total_diffs == 0 {
        println!("No sem diffs recorded yet. Run some diffs first!");
        return;
    }

    let total = stats.total_entities_analyzed;
    let noise_pct = if total > 0 {
        (stats.noise_filtered as f64 / total as f64 * 100.0) as u64
    } else {
        0
    };

    println!();
    println!("  sem lifetime stats");
    println!("  {}", "\u{2500}".repeat(36));
    println!("  {:>8}  diffs performed", stats.total_diffs);
    println!("  {:>8}  entities analyzed", stats.total_entities_analyzed);
    println!("  {:>8}  changes detected", stats.total_changes_detected);
    println!("  {:>8}  noise filtered", stats.noise_filtered);
    println!();
    println!(
        "  -> {} diffs, {}% noise filtered",
        stats.total_diffs, noise_pct
    );
    println!();
}
