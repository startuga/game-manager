//! Prints every detected game: `cargo run -p gm-core --example scan`

fn main() {
    let report = gm_core::scan_all();
    for game in &report.games {
        println!("{:<10} {:<50} {}", game.store.name(), game.name, game.id);
    }
    println!("{} games", report.games.len());
    for error in &report.errors {
        eprintln!("error: {error}");
    }
}
