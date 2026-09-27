//! lanroam-cli: protocol integration and verification tool.
//!
//! Exercises discovery, the QUIC handshake, desk groups, link latency and
//! input sessions between two machines, or between two instances on one
//! machine (give each its own `--data-dir` and pass `--port 0`).

mod commands;
mod dryrun;
mod output;
mod run;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use lanroam_core::{DEFAULT_DISCOVERY_PORT, DEFAULT_PORT};

/// Version shown by `--version`. CI dev builds set `LANROAM_BUILD` to the
/// branch and commit, so a binary on the Windows test machine can be matched
/// to its source
pub(crate) const VERSION: &str = match option_env!("LANROAM_BUILD") {
    Some(build) => build,
    None => env!("CARGO_PKG_VERSION"),
};

/// Command-line definition: subcommands and global options
#[derive(Parser)]
#[command(
    name = "lanroam-cli",
    version = VERSION,
    about = "Lanroam - LAN keyboard and mouse sharing, integration tool",
    after_help = "<TARGET> formats:\n  \
        peer name | device id | fingerprint prefix   found via discovery, fingerprint pinned\n  \
        ip:port                                      direct; no discovery, no pinning"
)]
struct Cli {
    /// Identity data directory (default: ~/.lanroam)
    #[arg(long, global = true, value_name = "DIRECTORY")]
    data_dir: Option<PathBuf>,

    /// UDP multicast discovery port
    #[arg(long, global = true, value_name = "PORT", default_value_t = DEFAULT_DISCOVERY_PORT)]
    discovery_port: u16,

    #[command(subcommand)]
    command: Command,
}

/// Available subcommands
#[derive(Subcommand)]
enum Command {
    /// Show the local device identity
    Id,
    /// List nodes on the LAN without advertising this one
    Scan {
        /// Seconds to listen
        #[arg(long = "wait", value_name = "SECONDS", default_value_t = 6)]
        wait_secs: u64,
    },
    /// Run this device in its desk group, sharing its keyboard and mouse,
    /// with a console to join, arrange and leave
    Run {
        /// QUIC port (UDP; 0 picks a free one)
        #[arg(long, value_name = "PORT", default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Display name for this run only
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Capture nothing and print the input received instead of
        /// injecting it (a second instance on the same machine)
        #[arg(long)]
        dry_run: bool,
    },
    /// Connect to a node and measure round trips on the control stream and
    /// over datagrams
    Ping {
        /// Node to connect to (accepted formats are listed below)
        target: String,
        /// Probes per path
        #[arg(long, value_name = "N", default_value_t = 20)]
        count: u32,
        /// Milliseconds between datagram probes
        #[arg(long = "interval", value_name = "MS", default_value_t = 20)]
        interval_ms: u64,
        /// Seconds to wait for the target to show up in discovery
        #[arg(long = "wait", value_name = "SECONDS", default_value_t = 10)]
        wait_secs: u64,
    },
}

/// Options shared by every subcommand
pub(crate) struct CommonArgs {
    /// Identity data directory
    pub(crate) data_dir: PathBuf,
    /// UDP multicast discovery port
    pub(crate) discovery_port: u16,
}

/// Entry point: set up logging, parse arguments, dispatch
#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let cli = Cli::parse();
    let common = CommonArgs {
        data_dir: cli.data_dir.unwrap_or_else(default_data_dir),
        discovery_port: cli.discovery_port,
    };
    match cli.command {
        Command::Id => commands::cmd_id(&common),
        Command::Scan { wait_secs } => commands::cmd_scan(&common, wait_secs).await,
        Command::Run {
            port,
            name,
            dry_run,
        } => run::cmd_run(&common, port, name, dry_run).await,
        Command::Ping {
            target,
            count,
            interval_ms,
            wait_secs,
        } => commands::cmd_ping(&common, &target, count, interval_ms, wait_secs).await,
    }
}

/// Logs go to stderr; the level comes from RUST_LOG (default: warn)
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Default identity directory: ~/.lanroam (HOME / USERPROFILE, falling back
/// to the current directory)
fn default_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lanroam")
}
