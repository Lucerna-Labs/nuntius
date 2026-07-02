# Nuntius Developer Guide

For people who want to **fix, debug, or extend** the platform. Operator
documentation lives in the [User Guide](USER-GUIDE.md).

## Contents

1. [Workspace layout](#workspace-layout)
2. [Boot sequence](#boot-sequence)
3. [The Plugin trait](#the-plugin-trait)
4. [Routing model (read this before adding routes)](#routing-model)
5. [Configuration](#configuration)
6. [Database patterns](#database-patterns)
7. [Cross-plugin communication](#cross-plugin-communication)
8. [The radio engine](#the-radio-engine)
9. [Front-end conventions](#front-end-conventions)
10. [Adding a plugin](#adding-a-plugin)
11. [Testing and debugging](#testing-and-debugging)
12. [Gotchas that have actually bitten us](#gotchas)

---

## Workspace layout

| Crate | Role |
|---|---|
| `core` | `Plugin` trait, `PluginManager`, `AppContext` (DB + `EventBus` + `ServiceRegistry`), `CccConfig` |
| `api` | Builds the axum router from all plugins; `/health`, `/version`; serves the app |
| `cli` | `ccc-cli` binary: config load, DB open, plugin registration (`registry.rs`), graceful shutdown |
| `database` | `CccDB` — embedded [redb](https://github.com/cberner/redb) wrapper, all table definitions |
| `ccc-cms` | Admin shell (`/admin`): auth, three-column layout, plugin page composition, public assets (`/assets/public.css`) |
| `radio-engine` | The station: TCP ingest/serving, AutoDJ, mixer, now-playing push, HLS, public radio pages |
| `ccc-plugin-utils` | Shared helpers: date/escape/filename/bytes formatting, `TomlSidecar<T>`, `render<T>` |
| `plugin-*` / `ccc-plugin-*` | Everything else: blog, podcast, video, newsletter, chat, federation, … |

## Boot sequence

`cli/src/main.rs::run_serve`:

1. `CccConfig::load_or_default("ccc.toml")`
2. Validate `[plugins].enabled` — **unknown names abort startup** (see
   `registry::unknown_plugin_names`), as does any plugin whose
   `from_config()`/`init()` returns `Err`. No silent skips.
3. Open redb → build `AppContext { db, events, services }`
4. `registry::register_by_name` for each enabled plugin (constructs via
   `from_config`, calls `init()`)
5. `ccc_api::app_router` merges every plugin's `routes()`; if the CMS plugin
   is enabled, `build_admin_tree` wraps every plugin's `admin_routes()` in
   the auth middleware + security headers under `/admin`
6. axum serves with graceful shutdown (SIGINT/SIGTERM cancels the radio
   engine's `CancellationToken`)

## The Plugin trait

`core/src/lib.rs`. The methods that matter:

- `from_config(&CccConfig) -> Result<Self>` *(convention, not on the trait)* —
  construct from config; return `Err` to abort boot with a clear message.
- `init(&self)` — one-time startup work. **No `AppContext` here** — anything
  needing the DB spawns from `routes()`/`admin_routes()` instead.
- `routes(&self, ctx)` — public router, merged into `/`.
- `admin_routes(&self, ctx)` — admin router, merged under `/admin` behind
  auth. Long-running workers are spawned here (see below).
- `status(&self, ctx) -> PluginStatus` — powers the dashboard cards and
  `/health/plugins`.
- `admin_nav(&self, ctx)` — entries for the console's left rail.

**Background workers**: use `EngineState::spawn_once`-style guards or the
shared pattern in each plugin — `routes()`/`admin_routes()` are called once
at boot, and workers must be idempotent against being spawned from either.

## Routing model

**Plugin routers are MERGED, not nested.** `PluginManager::merged_router`
calls `Router::merge` for every plugin, and `build_admin_tree` does the same
for admin routers under the `/admin` nest. Consequences:

- A route you declare as `/radio/tracks` in `admin_routes()` is served at
  `/admin/radio/tracks` — the plugin **id never appears in the URL**.
- Two plugins declaring the same path collide; axum's behavior is
  last-merge-wins with no warning. Prefix your routes with your plugin's
  namespace (`/blog/…`, `/newsletter/…`).
- `admin_url()` and every `Redirect::to(...)` you write must use the real
  merged path. (Historical bug: radio-engine redirected to
  `/admin/radio-engine/radio/...` — 404s on every button press.)

## Configuration

Two layers:

1. **`ccc.toml`** (`CccConfig`): `[server]`, `[database]`, `[site]` (the
   operator's public site name — use `cfg.site.name` for anything
   listener-facing), `[plugins].enabled`, and untyped `[plugin.<id>]`
   subtables read by each plugin's `from_config`.
2. **Sidecar TOMLs** (`radio.toml`, `newsletter.toml`, …): live settings a
   plugin edits at runtime, via `ccc_plugin_utils::TomlSidecar<T>`
   (`load_or_init` / `snapshot` / `update`, poison-safe).

> ⚠️ **Sidecar format**: `TomlSidecar<T>` serializes `T`'s fields at the
> **top level** — no `[section]` header. Only two files differ: `radio.toml`
> wraps its settings in `[radio]` (its own `SettingsStore`) and
> `federation.toml` in `[federation]` (hand-rolled to preserve the RSA key).
> A wrong header makes the plugin fail at boot with a parse error naming the
> file.

## Database patterns

`database/src/lib.rs` — read it top to bottom once; every table follows the
same idioms:

- `TableDefinition` constants, opened per-transaction.
- Values are `bincode`-encoded structs via the crate's `encode`/`decode`.
- **Chronological indexes** use zero-padded composite string keys
  (`{ts:020}_{id}`) so range scans come back sorted straight off disk.
- Monotonic ids come from `next_id(name)` on the counters table — never
  `SystemTime` (collisions).
- Slug indexes enforce uniqueness at write time.
- Listener statistics: `record_listener_sample` (minute-keyed max-upsert),
  `listener_samples(from,to)` (inclusive), `prune_listener_samples`.

Add tests in the same file; the suite uses throwaway temp DB files.

## Cross-plugin communication

- **`EventBus`** (`ctx.events`, tokio broadcast): fire-and-forget domain
  events — `BlogPostPublished` triggers newsletter delivery and ActivityPub
  fan-out. Subscribe in a spawned worker; publishing never blocks.
- **`ServiceRegistry`** (`ctx.services`): typed capability handles (e.g.
  membership). Deliberately non-generic — add a field per service.
- **HTTP**: stream-watch probes the radio engine's public endpoints like any
  external client would.

## The radio engine

`radio-engine/src/lib.rs` (~3k lines) + `mixer.rs`, `nowplaying.rs`,
`library.rs`, `playlists.rs`.

**Audio fan-out**: one `tokio::sync::broadcast::Sender<Bytes>` (`state.tx`)
carries the encoded master stream. Every consumer subscribes: TCP listeners
on port 8000 (with optional ICY metadata interleaving), the same-origin HTTP
route `/radio/stream`, variant transcoders, the recorder, and the HLS
pipeline. Feed the channel and everyone hears it.

**Source priority** (each preempts everything below):
live `SOURCE`/`PUT` ingest → relay → remote presenter (ffmpeg) → AutoDJ.
Preemption is via `AtomicBool`s checked in `mixer::preempted` and the AutoDJ
loop.

**AutoDJ modes**: with `crossfade_seconds > 0` a persistent ffmpeg encoder
takes raw PCM on stdin and per-track ffmpeg decoders are mixed
sample-by-sample across the fade window (`mixer.rs`); on pipeline failure it
falls back to a per-track pacer and retries the mixer after a backoff.

**Now-playing push** (`nowplaying.rs`): every state change emits a
`NowPlayingEvent` over a typed broadcast channel; `/api/radio/nowplaying/sse`
and `.../stream` (WebSocket) fan it out. Wire format (serde, internally
tagged):

```json
{"type":"snapshot", "now_playing":{...}, "history":[...], "live":false,
 "listeners":3, "stream_name":"...", "up_next":{...}, "active_playlist":"...", "ts":...}
{"type":"update",   "now_playing":{...}, "live":false, "listeners":3,
 "up_next":{...}, "active_playlist":"...", "ts":...}
```

`history` is **newest-first**. The listener page consumes SSE with a polling
fallback — keep both paths working when you change the shape (additive
fields only).

**Supporting endpoints**: `/api/radio/artwork/:id` (lofty extraction, bounded
in-memory cache, 8 MB cap), `/radio/manifest.webmanifest` + `/radio/icon.svg`
(PWA), `/radio/hls/live.m3u8` + strictly-validated segment names (see
`is_safe_segment_name` — never join request input into paths without it).

## Front-end conventions

- **Askama templates**, compiled into the binary — a template/context
  mismatch is a **compile error**, so `cargo check` validates HTML changes.
- **Public pages** share the design system in `ccc-cms/static/public.css`
  (served at `/assets/public.css`): tokens (`--signal` amber, `--phosphor`
  teal, `--bg-deep`…), `.pub-*` components, the `onair` lamp. Public pages
  must NOT link `admin.css`. Page-specific styles go in a small inline
  `<style>` block.
- **No JS framework.** Vanilla ES5-compatible scripts inline in each
  template. Server data reaches the DOM via `textContent`/`createElement` —
  never string-concatenated HTML.
- **Chat wire format** (`ccc-plugin-live-chat`): client sends
  `{"type":"msg","nick":…,"text":…}` / `{"type":"ping"}`; server sends
  `{"type":"hello","history":[newest-first],…}`, `{"type":"msg",…}`,
  `{"type":"rejected","reason":…}`, `{"type":"pong"}`.

## Adding a plugin

1. `cargo new ccc-plugin-foo --lib`; add to `[workspace] members`.
2. Depend on `ccc-core`, `ccc-database` (if needed), `ccc-plugin-utils`,
   `axum`, `askama`.
3. Implement the struct + `from_config` + `impl Plugin` (id, kind, icon,
   status, routes/admin_routes, admin_nav).
4. Register it in **both** places in `cli/src/registry.rs`: the dispatch
   `match` and the `known_plugins` list (startup validation checks the list).
5. Add the id to `[plugins].enabled` in `ccc.toml`.
6. Settings? Use `TomlSidecar<T>` with a flat struct — get persistence,
   snapshots, and poison-safety for free.
7. Public pages? Use `/assets/public.css` + the `.pub-*` chrome; take the
   site name from `cfg.site.name`, never hardcode one.

## Testing and debugging

```bash
cargo test --workspace          # ~170 unit tests, no external services needed
cargo check -p <crate>          # fast iteration; validates Askama templates too
RUST_LOG=debug ccc-cli -dd serve
```

- `-d` / `-dd` / `-ddd` map to info/debug/trace; `RUST_LOG` overrides.
- The Windows launcher writes `nuntius-server.out.log` / `.err.log`.
- `GET /health/plugins` tells you which plugin thinks it's unconfigured.
- To exercise the full radio path without a real DJ, pipe a tone in:
  `ffmpeg -re -f lavfi -i sine=frequency=440 -c:a libmp3lame -f mp3
  icecast://source:PASSWORD@localhost:8000/listen.mp3`

## Gotchas

Real issues that have cost time in this codebase — check these first:

- **Windows: rebuilds fail with "Access is denied … ccc-cli.exe"** — the
  server is running and the exe is locked. Stop it, build, restart.
- **`.cmd` files must be CRLF.** With LF-only endings, `cmd`'s `goto`
  scanner jumps mid-line and you get baffling `'x' is not recognized`
  errors from the middle of arguments.
- **Sidecar TOML headers** — see [Configuration](#configuration). A
  `[whisper]` header in `whisper.toml` silently disabled the plugin for
  months when startup errors were still soft.
- **Admin routes are merged, not nested** — redirects to
  `/admin/<plugin-id>/…` 404.
- **Never lowercase an `Authorization` header before decoding** — base64 is
  case-sensitive; header *names* and *schemes* are the case-insensitive
  parts. This broke all source authentication once.
- **`broadcast::Receiver` + silent stations** — a task parked on `recv()`
  never notices its client hung up. Socket-serving loops must `select!` on
  a client read to detect disconnects during dead air (leaks tasks and the
  listener count otherwise).
- **`timeout.exe` fails when stdin is redirected** — batch scripts that
  might run non-interactively should sleep with `ping -n N 127.0.0.1`.
- **Usernames get trailing spaces from autofill** — trim identity fields at
  write AND compare trimmed; never trim passwords.
- **HLS segment names are reused across pipeline restarts** — they are not
  immutable; serve them `Cache-Control: no-store`.

## Conventions

- Commit style: `feat(scope): …` / `fix(scope): …` / `refactor(scope): …`,
  matching `git log`.
- Every bug fix lands with a regression test in the same crate.
- Comments explain constraints the code can't show — not what the next line
  does.
- Public-page changes: verify against a running server (`cargo run -p
  ccc-cli -- serve`), not just `cargo check`.
