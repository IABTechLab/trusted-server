//! Trusted Server CLI for configuration management and attestation.
//!
//! This tool provides commands for:
//! - Pushing configuration to edge platform Config Stores
//! - Validating configuration files
//! - Computing configuration hashes for attestation
//! - Local development with `fastly compute serve`

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

mod config;
mod error;
mod hash;
mod local;
mod platform;

use error::CliError;

#[derive(Parser)]
#[command(name = "tscli")]
#[command(about = "Trusted Server CLI for config and attestation management")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Configuration management
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Clone, ValueEnum, Debug)]
pub enum Platform {
    Fastly,
    // Cloudflare and Akamai support planned for future releases
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Platform::Fastly => write!(f, "fastly"),
        }
    }
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Push config to edge platform Config Store
    Push {
        /// Target platform
        #[arg(long, short, value_enum, default_value = "fastly")]
        platform: Platform,

        /// Path to the TOML configuration file
        #[arg(long, short)]
        file: PathBuf,

        /// Fastly Config Store ID
        #[arg(long)]
        store_id: String,

        /// Dry run - show what would be uploaded without actually uploading
        #[arg(long)]
        dry_run: bool,
    },

    /// Validate config against schemas and settings validation
    Validate {
        /// Path to the TOML configuration file
        #[arg(long, short)]
        file: PathBuf,
    },

    /// Compute and display config hash (SHA-256)
    Hash {
        /// Path to the TOML configuration file
        #[arg(long, short)]
        file: PathBuf,

        /// Output format
        #[arg(long, default_value = "text")]
        format: HashFormat,

        /// Hash the raw file without applying environment overrides
        #[arg(long)]
        raw: bool,
    },

    /// Compare local config with deployed config
    Diff {
        /// Target platform
        #[arg(long, short, value_enum, default_value = "fastly")]
        platform: Platform,

        /// Fastly Config Store ID
        #[arg(long)]
        store_id: String,

        /// Path to the local TOML configuration file
        #[arg(long, short)]
        file: PathBuf,
    },

    /// Pull current config from Config Store
    Pull {
        /// Target platform
        #[arg(long, short, value_enum, default_value = "fastly")]
        platform: Platform,

        /// Fastly Config Store ID
        #[arg(long)]
        store_id: String,

        /// Output file path
        #[arg(long, short)]
        output: PathBuf,
    },

    /// Generate config store JSON for local development with `fastly compute serve`
    Local {
        /// Path to the TOML configuration file
        #[arg(long, short)]
        file: PathBuf,

        /// Output JSON file path (default: target/trusted-server-config.json)
        #[arg(long, short, default_value = local::DEFAULT_OUTPUT_PATH)]
        output: PathBuf,
    },
}

#[derive(Clone, ValueEnum, Debug)]
pub enum HashFormat {
    Text,
    Json,
}

fn main() {
    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Commands::Config { action } => match action {
            ConfigAction::Push {
                platform,
                file,
                store_id,
                dry_run,
            } => config::push(platform, file, store_id, dry_run, cli.verbose),
            ConfigAction::Validate { file } => config::validate(file, cli.verbose),
            ConfigAction::Hash { file, format, raw } => {
                hash::compute_and_display(file, format, raw, cli.verbose)
            }
            ConfigAction::Diff {
                platform,
                store_id,
                file,
            } => config::diff(platform, store_id, file, cli.verbose),
            ConfigAction::Pull {
                platform,
                store_id,
                output,
            } => config::pull(platform, store_id, output, cli.verbose),
            ConfigAction::Local { file, output } => {
                local::generate_config_store_json(file, output, cli.verbose)
            }
        },
    }
}
