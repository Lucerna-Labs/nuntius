//! Creator Command Center (CCC) Rust Command Line Interface
//!
//! Designed for human operators and LLM agents to manage, configure, and
//! automate the high-performance Creator Command Center (CCC) server.

use ccc_core::{AppContext, CccConfig, EventBus, PluginManager, ServiceRegistry};
use ccc_database::CccDB;
use ccc_radio_engine::{RadioEnginePlugin, StreamingProtocol};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;

mod registry;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to the ccc.toml configuration file.
    ///
    /// If absent, defaults are used (no plugins enabled).
    #[arg(short = 'c', long, default_value = "ccc.toml", global = true)]
    config: PathBuf,

    /// Turn debugging information on.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    debug: u8,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the REST API server with the plugins enabled in `ccc.toml`.
    Serve {
        /// Override the port from the configuration file.
        #[arg(short, long)]
        port: Option<u16>,
    },
    /// Start the native streaming radio engine standalone.
    Radio {
        #[arg(short, long, default_value_t = 8000)]
        port: u16,

        #[arg(long, value_enum, default_value_t = StreamingProtocol::Icecast)]
        protocol: StreamingProtocol,
    },
    /// List the plugins enabled in `ccc.toml` (vs. those known but disabled).
    Plugins,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let cfg = match CccConfig::load_or_default(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to read config '{}': {}", cli.config.display(), e);
            std::process::exit(1);
        }
    };

    match cli.command {
        Some(Commands::Serve { port }) => run_serve(cfg, port).await,
        Some(Commands::Radio { port, protocol }) => run_radio(port, protocol).await,
        Some(Commands::Plugins) => list_plugins(&cfg),
        None => {
            println!("Welcome to Creator Command Center (CCC) Rust CLI.");
            println!("Use --help for a list of available commands.");
        }
    }
}

async fn run_serve(cfg: CccConfig, port_override: Option<u16>) {
    let port = port_override.unwrap_or(cfg.server.port);
    let addr = format!("{}:{}", cfg.server.host, port);

    let db = match CccDB::new(&cfg.database.path.to_string_lossy()) {
        Ok(db) => Arc::new(db),
        Err(e) => {
            eprintln!(
                "Failed to open database at '{}': {}",
                cfg.database.path.display(),
                e
            );
            std::process::exit(1);
        }
    };
    let ctx = AppContext {
        db,
        events: Arc::new(EventBus::new()),
        services: Arc::new(ServiceRegistry::new()),
    };

    let mut manager = PluginManager::new();
    for name in &cfg.plugins.enabled {
        if let Err(e) = registry::register_by_name(&mut manager, name, &cfg, &ctx) {
            eprintln!("Skipping plugin '{}': {}", name, e);
        }
    }

    println!(
        "Starting Creator Command Center (CCC) on {} with {} plugin(s) enabled: {:?}",
        addr,
        manager.names().len(),
        manager.names()
    );

    let mut app = ccc_api::app_router(&manager, &ctx);

    // If the CMS plugin is enabled, build the auth-gated admin tree
    // (CMS handlers + every other plugin's admin_routes) and merge it.
    // If the CMS is disabled, no /admin surface exists at all — admin
    // routes from other plugins do not mount.
    if let Some(cms) = manager.find::<ccc_cms::CmsPlugin>() {
        let plugin_admin = manager.merged_admin_router(&ctx);
        let dashboard_ctx = ccc_cms::DashboardCtx {
            store: cms.store.clone(),
            nav: manager.merged_admin_nav(&ctx),
            plugin_names: manager.names().iter().map(|s| s.to_string()).collect(),
        };
        let admin_tree = ccc_cms::build_admin_tree(dashboard_ctx, plugin_admin);
        app = app.merge(admin_tree);
        println!(
            "CMS enabled at /admin (sidecar: {})",
            cms.admin_store_path.display()
        );
    }

    // Analytics is a global middleware: wrap the entire app once it's
    // assembled. Disabled => no recording, no overhead.
    if let Some(analytics) = manager.find::<ccc_plugin_analytics::CccPluginAnalyticsPlugin>() {
        app = ccc_plugin_analytics::apply_recording_layer(app, analytics);
        println!("Analytics middleware engaged");
    }

    if let Err(e) = ccc_api::serve(&addr, app).await {
        eprintln!("Failed to start server: {}", e);
    }
}

async fn run_radio(port: u16, protocol: StreamingProtocol) {
    println!(
        "Initializing Native Radio Engine on port {} with protocol {:?}...",
        port, protocol
    );
    let plugin = RadioEnginePlugin::new(port, protocol);
    if let Err(e) = plugin.run().await {
        eprintln!("Failed to run radio engine: {}", e);
    }
}

fn list_plugins(cfg: &CccConfig) {
    println!("Plugins known to this build:");
    for name in registry::known_plugins() {
        let on = cfg.plugins.enabled.iter().any(|n| n == name);
        let marker = if on { "[on] " } else { "[off]" };
        println!("  {} {}", marker, name);
    }
}
