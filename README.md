# Nuntius — Creator Command Center

**A self-hosted, radio-first creator platform, written entirely in Rust.**
By Lucerna Media.

Nuntius runs a 24/7 internet radio station — and everything around it — from a
single binary: AutoDJ with real PCM crossfading and EBU R128 loudness, live
source ingest, a modern listener page with live chat and lock-screen media
controls, plus a blog, podcast feeds, video publishing, a newsletter, and
ActivityPub federation. No external database, no runtime dependencies beyond
`ffmpeg`.

- **[User Guide](docs/USER-GUIDE.md)** — running your station: uploading music,
  scheduling, going live, publishing content.
- **[Developer Guide](docs/DEV-GUIDE.md)** — architecture, plugin API, and how
  to debug, fix, and extend the platform.

## Highlights

- **24/7 radio engine** — Icecast-compatible ingest (`SOURCE`/`PUT`), AutoDJ
  with sample-accurate crossfades, loudness normalization, scheduled playlists,
  hourly clock rotation (jingles/station IDs), dead-air backup rotation, relay,
  SRT/RTP/RTMP remote presenter ingest, on-air recording, and optional HLS
  output.
- **A modern listener experience** — live now-playing over SSE (no polling),
  album art, Media Session lock-screen controls, spectrum visualizer, listener
  chat, up-next display, installable as a PWA.
- **Same-origin streaming** — audio is served through the main HTTP port, so a
  single HTTPS reverse proxy covers pages, API, and sound.
- **Creator suite** — markdown blog (sanitized), podcast RSS (public + members
  feed), video with HLS transcoding, newsletter with SMTP delivery, SEO
  (robots/sitemap), privacy-light analytics, Whisper transcription, WebRTC
  bridge, and signed ActivityPub federation.
- **Zero-overhead storage** — a single embedded [redb](https://github.com/cberner/redb)
  file with chronologically-sorted composite keys. No SQL server to run.
- **Plugin architecture** — every feature above is a plugin behind one trait;
  disabled plugins are never even constructed.

## Quick start

```bash
# 1. Build (ffmpeg must be on PATH for the radio engine)
cargo build --release -p ccc-cli

# 2. Run — a default ccc.toml is created on first boot
./target/release/ccc-cli serve

# 3. Open http://localhost:3000
#    /            the public site (radio hero + latest content)
#    /radio       the full listener page
#    /admin       the operator console (one-time setup wizard on first visit)
```

On Windows, double-click **`Launch-Nuntius.cmd`** (or the "Launch Nuntius"
shortcut) — it starts the server in the background and opens the console.

## Configuration in 30 seconds

`ccc.toml` is the single source of truth:

```toml
[server]
host = "0.0.0.0"
port = 3000

[site]
# What YOUR deployment calls itself on public pages, titles, and RSS
# channels. "Nuntius" is only the out-of-the-box fallback.
name = "My Station"

[plugins]
enabled = ["cms", "radio-engine", "blog", "podcast", "live-chat", "stream-watch"]

[plugin.cms]
admin_store_path = "admin.toml"
# disable_auth = true   # ONLY for machines nothing else can reach
```

Each plugin keeps its own sidecar TOML (`radio.toml`, `newsletter.toml`, …),
all editable from the admin console. Run `ccc-cli plugins` to list every
plugin this build knows. Unknown names in `enabled` abort startup with a
clear error — silence is not an option.

## Health & observability

- `GET /health` — liveness (200 while the process is up)
- `GET /health/plugins` — readiness (503 lists unconfigured plugins)
- `GET /version` — API/CLI version
- `RUST_LOG=debug ccc-cli -dd serve` — structured logs via `tracing`

## License

Nuntius / Creator Command Center is released under the **PolyForm Small
Business License 1.0.0** (Fair Code).

It is **100% free and open-source** for individuals, non-profits, and small
businesses. Use by **Large Enterprises** (more than 100 employees or over
$5,000,000 USD annual gross revenue) requires a commercial license from
**Lucerna Media**. See [`LICENSE`](LICENSE) for the full terms.
