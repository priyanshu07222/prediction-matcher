//! API server binary (HTTP + WebSocket) — wired up in later steps.

use prediction_matcher::OrderBook;

fn main() {
    // Ensures the library is linked; replace with HTTP server wiring later.
    let _book = OrderBook::new();
}
