// ABOUTME: Thin CLI entry over picot_host::relay::smoke_client (plan T11).
// ABOUTME: Manual real-relay smoke: consume an offer URL, pair, one probe.

//! Usage (see `USAGE` in the library for the full contract):
//!
//! ```sh
//! RELAY_SMOKE_OFFER='https://picot.021521.xyz/#offer=…' \
//! cargo run -p picot-host --example relay-smoke-client
//! ```
//!
//! Exit codes: 0 smoke ok, 1 run failure, 2 usage error. This example is the
//! smoke entry only — it is not shipped and not wired into any product path.

use picot_host::relay::smoke_client::{self, SmokeOptions};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options: SmokeOptions = match smoke_client::parse_args(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("relay-smoke-client: {error}\n{}", smoke_client::USAGE);
            std::process::exit(2);
        }
    };
    match smoke_client::run_smoke(&options).await {
        Ok(report) => {
            print!("{}", report.render());
            if !report.ok() {
                eprintln!("relay-smoke-client: no correlated probe response");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("relay-smoke-client: {error}");
            std::process::exit(1);
        }
    }
}
