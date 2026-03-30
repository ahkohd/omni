#[path = "../ui_client/mod.rs"]
mod ui_client;

fn main() {
    if let Err(error) = ui_client::run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
