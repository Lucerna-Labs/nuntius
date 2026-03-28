use clap::{Parser, Subcommand};
use azuracast_radio_engine::RadioEnginePlugin;

/// AzuraCast Rust Command Line Interface
///
/// This CLI is designed for both human operators and LLM agents to easily
/// manage, configure, and automate the high-performance AzuraCast server.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Turn debugging information on
    #[arg(short, long, action = clap::ArgAction::Count)]
    debug: u8,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the REST API server
    Serve {
        /// The port to run the server on
        #[arg(short, long, default_value_t = 3000)]
        port: u16,
    },
    /// Start the native streaming radio engine
    Radio {
        /// The port to run the streaming server on
        #[arg(short, long, default_value_t = 8000)]
        port: u16,
    },
    /// List available plugins
    Plugins,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Some(Commands::Serve { port }) => {
            let addr = format!("0.0.0.0:{}", port);
            println!("Starting AzuraCast API server on {}...", addr);
            if let Err(e) = azuracast_api::serve(&addr).await {
                eprintln!("Failed to start server: {}", e);
            }
        }
        Some(Commands::Radio { port }) => {
            println!("Initializing Native Radio Engine on port {}...", port);
            let plugin = RadioEnginePlugin::new(*port);
            if let Err(e) = plugin.run().await {
                eprintln!("Failed to run radio engine: {}", e);
            }
        }
        Some(Commands::Plugins) => {
            println!("Plugin subsystem initializing...");
            // Plugin manager would be initialized here
            println!("No plugins loaded yet.");
        }
        None => {
            println!("Welcome to AzuraCast Rust CLI.");
            println!("Use --help for a list of available commands.");
        }
    }
}
