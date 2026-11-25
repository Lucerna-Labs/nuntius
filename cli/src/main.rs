//! Nuntius (CCC) Rust Command Line Interface
//!
//! Designed for human operators and LLM agents to manage, configure, and
//! automate the high-performance Nuntius (CCC) server.

use ccc_core::{AppContext, CccConfig, EventBus, PluginManager, ServiceRegistry};
use ccc_database::CccDB;
use ccc_radio_engine::{RadioEnginePlugin, StreamingProtocol};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

mod registry;

/// Initialize structured logging. The `RUST_LOG` env var wins if set
/// (so operators can override without rebuilding). Otherwise the `-d`
/// count maps to a level:
///   -d       -> warn
///   -dd      -> info
///   -ddd     -> debug
///   -dddd    -> trace
fn init_tracing(debug: u8) {
    use tracing_subscriber::{fmt, EnvFilter};
    let default_level = match debug {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(format!("ccc_={default_level},ccc_cli={default_level}"))
    });
    // Best-effort init: if a subscriber is already set (e.g. tests) just skip.
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}

/// Resolve when the process receives SIGINT (Ctrl-C) or SIGTERM. Used
/// to gracefully shut down both the axum server and the radio engine.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("SIGINT received, shutting down"),
        _ = terminate => info!("SIGTERM received, shutting down"),
    }
}

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
    init_tracing(cli.debug);

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
            println!("Welcome to Nuntius (CCC) Rust CLI.");
            println!("Use --help for a list of available commands.");
        }
    }
}

async fn run_serve(cfg: CccConfig, port_override: Option<u16>) {
    let port = port_override.unwrap_or(cfg.server.port);
    let addr = format!("{}:{}", cfg.server.host, port);

    // Reject typo'd plugin names before doing any real work (opening the
    // database, binding the socket). A silently skipped plugin is a feature
    // the operator asked for and didn't get.
    let unknown = registry::unknown_plugin_names(&cfg.plugins.enabled);
    if !unknown.is_empty() {
        eprintln!(
            "Unknown plugin name(s) in [plugins].enabled: {}",
            unknown.join(", ")
        );
        eprintln!(
            "Plugins known to this build: {}",
            registry::known_plugins().join(", ")
        );
        eprintln!("Fix the names in ccc.toml (or run `ccc plugins` to list them) and retry.");
        std::process::exit(1);
    }

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
        // Every name was validated above, so a failure here is a plugin
        // that *is* known but couldn't construct or init — real
        // misconfiguration (bad credentials, unreadable paths). Booting
        // without it would silently drop a feature the operator enabled,
        // so abort startup instead.
        if let Err(e) = registry::register_by_name(&mut manager, name, &cfg, &ctx) {
            eprintln!("Plugin '{name}' failed to start: {e}");
            eprintln!(
                "Fix its [plugin.{name}] section in ccc.toml or remove '{name}' from [plugins].enabled."
            );
            std::process::exit(1);
        }
    }
    // Freeze the manager so the CMS admin shell can hold a shared
    // reference and serve `/admin/api/plugins/*` from live data.
    let manager = Arc::new(manager);

    info!(
        addr = %addr,
        plugin_count = manager.names().len(),
        plugins = ?manager.names(),
        "Starting Nuntius (CCC)"
    );

    let mut app = ccc_api::app_router(manager.clone(), &ctx);

    // If the CMS plugin is enabled, build the auth-gated admin tree
    // (CMS handlers + every other plugin's admin_routes) and merge it.
    // If the CMS is disabled, no /admin surface exists at all - admin
    // routes from other plugins do not mount.
    if let Some(cms) = manager.find::<ccc_cms::CmsPlugin>() {
        let plugin_admin = manager.merged_admin_router(&ctx);
        let dashboard_ctx = ccc_cms::DashboardCtx {
            store: cms.store.clone(),
            nav: manager.merged_admin_nav(&ctx),
            plugin_names: manager.names().iter().map(|s| s.to_string()).collect(),
            plugin_manager: manager.clone(),
            app_ctx: ctx.clone(),
        };
        let admin_tree = ccc_cms::build_admin_tree(dashboard_ctx, plugin_admin);
        app = app.merge(admin_tree);
        info!(
            sidecar = %cms.admin_store_path.display(),
            "CMS enabled at /admin"
        );
    }

    // Multi-domain / multi-subdomain blog routing: when a request arrives
    // with a Host header matching a configured BlogSite, serve that blog at
    // `/` and `/posts/:slug` without colliding with radio-engine's `/` route.
    if manager
        .find::<ccc_plugin_blog::StaticBlogPlugin>()
        .is_some()
    {
        app = ccc_plugin_blog::apply_multisite_blog_layer(app, ctx.db.clone());
        info!("Multi-domain blog host routing engaged");
    }

    // Analytics is a global middleware: wrap the entire app once it's
    // assembled. Disabled => no recording, no overhead.
    if let Some(analytics) = manager.find::<ccc_plugin_analytics::CccPluginAnalyticsPlugin>() {
        app = ccc_plugin_analytics::apply_recording_layer(app, analytics);
        info!("Analytics middleware engaged");
    }

    // The radio engine plugin, if loaded, runs its own accept loop in
    // a background task. We need to flip its shutdown token when the
    // process is told to stop so the autodj/relay/recorder tasks wind
    // down cleanly instead of being killed mid-write. We capture the
    // token *before* any awaits so the borrow doesn't cross them.
    let radio_shutdown_token = manager
        .find::<RadioEnginePlugin>()
        .map(|p| p.shutdown_token());

    let serve_shutdown = async move {
        shutdown_signal().await;
        if let Some(token) = radio_shutdown_token {
            token.cancel();
        }
    };

    if let Err(e) = ccc_api::serve(&addr, app, serve_shutdown).await {
        warn!(error = %e, "Failed to start server");
    }
}

async fn run_radio(port: u16, protocol: StreamingProtocol) {
    info!(port, ?protocol, "Initializing Native Radio Engine");
    let plugin = RadioEnginePlugin::new(port, protocol);
    // Race the engine against the shutdown signal so SIGINT/SIGTERM
    // produces a clean exit instead of the OS killing us mid-broadcast.
    tokio::select! {
        _ = shutdown_signal() => {
            info!("shutdown signal received, stopping radio engine");
            plugin.shutdown();
        }
        res = plugin.run() => {
            if let Err(e) = res {
                warn!(error = %e, "Radio engine exited with error");
            }
        }
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
