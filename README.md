# Creator Command Center (CCC)
**A Next-Generation Media Hub by Lucerna Media**

Creator Command Center (CCC) is a revolutionary, high-performance, completely serverless-capable broadcasting and publishing platform written entirely in Rust. Originally inspired by traditional radio management tools, CCC has evolved into an ultra-efficient, multi-protocol hub for modern creators.

## Vision and Architecture

CCC is built from the ground up to minimize server strain, maximize concurrency, and provide unprecedented flexibility through a dynamic WebAssembly-ready plugin architecture.

### Key Innovations:

*   **P2P WebRTC Video & Audio:** Utilizing modern WebRTC mesh networks (via `p2p-media-loader`), CCC drastically reduces your server bandwidth. If 1,000 listeners tune into your live broadcast, they share the stream directly among themselves in the browser, reducing server load by up to 90%.
*   **Zero-Overhead Database (`redb`):** Instead of running a heavy SQL server, CCC uses `redb`—a pure Rust embedded key-value store. By leveraging clever zero-padded composite keys, CCC sorts massive datasets chronologically directly on disk, providing instant lookup times with zero CPU parsing overhead.
*   **Multi-Protocol Native Streaming:** Whether you need legacy Icecast (TCP) support for older hardware, or modern HTTP Live Streaming (HLS) for mobile apps, CCC's native Rust streaming engine handles it asynchronously with near-zero RAM footprint.
*   **GitHub-Backed Plugin Store:** Easily expand your station's capabilities (blogs, video podcasts, analytics) via our open-source, serverless plugin registry hosted directly on GitHub.
*   **API & Automation First:** Every feature is exposed via a high-performance REST API (`axum`) and a comprehensive CLI (`clap`), making CCC the perfect platform for automation by AI agents or custom scripts.

## Core Crates

*   `ccc-core`: The heart of the application, managing the `Plugin` trait ecosystem.
*   `ccc-database`: The ultra-fast, custom-indexed `redb` storage layer.
*   `ccc-radio-engine`: The native streaming engine supporting Icecast and HLS.
*   `ccc-api` & `ccc-cli`: The primary interfaces for interacting with the Command Center.
*   `ccc-plugin-*`: Extensible features including Video Podcasting, Blogging, and the decentralized Store Registry.

## Quick Start

```bash
# 1. Build
cargo build --release -p ccc-cli

# 2. Create a config (optional — defaults work for local dev)
cp ccc.toml.example ccc.toml  # if you have one, otherwise the binary
                              # creates ccc.toml on first run

# 3. First boot — opens /admin/setup to create the operator account
./target/release/ccc-cli serve

# 4. Visit http://localhost:3000/admin/setup in a browser.
#    This is the ONLY time the setup wizard is shown. After you set
#    a password, /admin/login is the standard entry point.
```

The CLI defaults to `host = 127.0.0.1` and `port = 3000`. Override
with `-p` or in `ccc.toml`:

```toml
[server]
host = "0.0.0.0"
port = 3000
```

### Health & observability

* `GET /health` — liveness probe (always 200 if the process is up)
* `GET /health/plugins` — readiness probe. Returns **200** when every
  enabled plugin is configured, **503** with `unconfigured: [...]`
  otherwise. Use this behind a load balancer.
* `GET /version` — API/CLI version
* `RUST_LOG=debug ./ccc-cli -dd serve` — structured logs via `tracing`.
  Levels: `-d` (info), `-dd` (debug), `-ddd` (trace).

### Plugins in `ccc.toml`

The config file is a flat list. Each enabled plugin name in
`[plugins] enabled` must match a known plugin:

```toml
[plugins]
enabled = [
    "cms",                # admin shell — required for /admin
    "blog",
    "podcast",
    "video-podcast",
    "radio-engine",
    "analytics",
    "bridge",
    "seo",
    "s3",
    "newsletter",
    "whisper",
    "activitypub",
    "store",
    "ads",
    "call-studio",
    "drop",
    "theme-dark-matter",
]

[plugin.podcast]
# The members token gates access to the members-only feed. CHANGE
# this before announcing your public RSS URL — anyone with the token
# can fetch the private feed forever.
members_token = "REPLACE_ME_WITH_A_LONG_RANDOM_STRING"
```

The `[plugin.<name>]` table is per-plugin configuration; see each
plugin's `src/lib.rs` for the full schema. Unknown keys are ignored.

### Operational scripts

* `Launch-Nuntius.ps1` (Windows) — start the server in the background
  and open `/admin` in the default browser. Falls back from CIM to
  `Get-Process` if WMI is unavailable.

---

## License

Creator Command Center (CCC) is released under the **PolyForm Small Business License 1.0.0** (Fair Code).

It is **100% free and open-source** for individuals, non-profits, and small businesses.
However, it is **restricted for use by Large Enterprises** (organizations with more than 100 employees or >$5,000,000 USD annual gross revenue).

If you represent a Large Enterprise and wish to use CCC in production, you must contact **Lucerna Media** to purchase a commercial license. Please see the `LICENSE` file for full details.
