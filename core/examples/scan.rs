//! Prints every detected game: `cargo run -p gm-core --example scan [-- -v]`
//! `-v` also prints how each game is launched.

fn main() {
    let verbose = std::env::args().any(|arg| arg == "-v");
    let report = gm_core::scan_all();
    for game in &report.games {
        println!("{:<10} {:<50} {}", game.store.name(), game.name, game.id);
        if verbose {
            println!("           play: {:?}", game.launch);
            for alt in &game.alt_launches {
                println!("           {}: {:?}", alt.label, alt.launch);
            }
        }
    }
    println!("{} games", report.games.len());
    for error in &report.errors {
        eprintln!("error: {error}");
    }
}
