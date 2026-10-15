// ABOUTME: picot-daemon entry point: parses argv and reports its build identity.
// ABOUTME: T1 scaffold only — the host stack (admin RPC, HostServer) lands in T2-T4.

use std::process::ExitCode;

/// Command line contract from plan D7: the app passes every path in, so the
/// daemon never guesses a platform directory.
#[derive(Debug, Default)]
struct Args {
    data_dir: Option<String>,
    config_dir: Option<String>,
    static_dir: Option<String>,
    bundle_identifier: Option<String>,
    version: bool,
}

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("picot-daemon: {message}");
            eprintln!(
                "usage: picot-daemon [--data-dir DIR] [--config-dir DIR] [--static-dir DIR] \
                 [--bundle-identifier ID] [--version]"
            );
            return ExitCode::from(2);
        }
    };

    if args.version {
        println!("{}", build_identity());
        return ExitCode::SUCCESS;
    }

    // T2-T4 attach HostServer, the Pi runtimes, and the admin listener here.
    println!("{}", build_identity());
    println!(
        "scaffold: has data-dir={} config-dir={} static-dir={} bundle-identifier={}",
        args.data_dir.as_deref().unwrap_or("(unset)"),
        args.config_dir.as_deref().unwrap_or("(unset)"),
        args.static_dir.as_deref().unwrap_or("(unset)"),
        args.bundle_identifier.as_deref().unwrap_or("(unset)"),
    );
    ExitCode::SUCCESS
}

/// Same string the app and the host crate report: one version and one build id
/// per cargo invocation, which is what the version+build identity match (T4)
/// compares.
fn build_identity() -> String {
    format!(
        "picot-daemon {} (build {})",
        env!("CARGO_PKG_VERSION"),
        picot_host::BUILD_ID
    )
}

impl Args {
    fn parse(argv: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = Args::default();
        let mut argv = argv.peekable();
        while let Some(flag) = argv.next() {
            match flag.as_str() {
                "--version" | "-V" => args.version = true,
                "--data-dir" | "--config-dir" | "--static-dir" | "--bundle-identifier" => {
                    let value = argv
                        .next()
                        .ok_or_else(|| format!("{flag} requires a value"))?;
                    match flag.as_str() {
                        "--data-dir" => args.data_dir = Some(value),
                        "--config-dir" => args.config_dir = Some(value),
                        "--static-dir" => args.static_dir = Some(value),
                        _ => args.bundle_identifier = Some(value),
                    }
                }
                other => return Err(format!("unrecognized argument {other}")),
            }
        }
        Ok(args)
    }
}
