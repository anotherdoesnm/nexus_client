//! Entry point. All of the client lives in the library (`nexus_tui::run`), so
//! the protocol layer can be tested without a terminal.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    nexus_tui::run().await
}
