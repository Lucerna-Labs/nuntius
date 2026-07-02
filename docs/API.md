# Nuntius API Reference

The machine-callable surface — for automation, monitoring, and AI assistants.

**TL;DR of the honest state today:** Nuntius has a real, JSON HTTP surface for
**reading** everything (health, now-playing, library, stats) with no auth, and
a live event stream (SSE/WebSocket) for the radio. **Writing** — creating
posts, uploading tracks, changing settings — currently goes through the admin
routes, which accept HTML form POSTs and are gated by a session cookie, not an
API token. So an assistant can *observe and report* out of the box; to have it
*manage and change* things, see [Automating writes](#automating-writes) and
[Recommended: a real management API](#recommended-a-real-management-api).

## Contents

1. [Read/observe endpoints (no auth)](#readobserve-endpoints)
2. [Live event streams](#live-event-streams)
3. [Write/manage surface today](#writemanage-surface-today)
4. [Automating writes today](#automating-writes)
5. [Recommended: a real management API](#recommended-a-real-management-api)

---

## Read/observe endpoints

All return JSON (except the two health text routes) and require **no
authentication** — safe for an assistant to poll. Base URL is the main port,
default `http://localhost:3000`.

### Platform

| Method | Path | Returns |
|---|---|---|
| GET | `/health` | `"OK …"` — liveness, always 200 while up |
| GET | `/version` | API/CLI version string |
| GET | `/health/plugins` | `{ready, unconfigured:[…], total}` — **200** when all plugins ready, **503** otherwise |

`GET /health/plugins` is the single best "is my station healthy?" call — one
request tells an assistant whether anything needs attention and names it.

### Radio

| Method | Path | Returns |
|---|---|---|
| GET | `/api/radio/nowplaying` | Full snapshot: `now_playing{title,artist,source,track_id,started_at,duration_seconds}`, `history[]` (newest-first), `live`, `relay`, `presenter`, `autodj_paused`, `recording`, `listeners`, `stream_name`, `up_next`, `active_playlist`, `now_unix` |
| GET | `/api/radio/library` | `[{id,title,artist,duration_seconds}]` — the whole track library |
| GET | `/api/radio/presenter` | `{configured,url,label,active,ts}` — remote presenter state |
| GET | `/api/radio/artwork/:id` | Embedded cover art bytes for a track (404 if none) |

### Other plugins

| Method | Path | Returns |
|---|---|---|
| GET | `/api/live-chat/recent` | `{enabled, messages[], slow_mode_ms, max_message_length}` |
| GET | `/api/stream-watch/status` | Dead-air watchdog status |
| GET | `/api/stream-watch/events` | Recent watchdog events (silence alerts) |
| GET | `/api/ads/next` | Next ad creative in rotation |
| GET | `/api/bridge/status`, `/api/bridge/bridges` | WebRTC bridge state |
| GET | `/admin/api/plugins` | Every plugin's id, name, kind, lifecycle state, nav |
| GET | `/admin/api/plugins/:id/status` | One plugin's detailed `status()` JSON (the dashboard cards) |

> `/admin/api/*` lives under `/admin`, so it is behind the auth gate unless
> `disable_auth = true`. The `/api/*` routes above are public.

Everything an assistant needs to **answer questions** about the station —
what's playing, how many listeners, is anything down, what's in the library,
what's scheduled next — is here today, no credentials required.

## Live event streams

Instead of polling, subscribe once and get pushed updates:

- **SSE:** `GET /api/radio/nowplaying/sse` — `data:`-framed `NowPlayingEvent`
  JSON. Best for one-way consumers (scripts, dashboards, LLM tool loops).
  Proxy-friendly, auto-reconnects.
- **WebSocket:** `GET /api/radio/nowplaying/stream` — same payloads as frames.
- **Chat WebSocket:** `GET /api/live-chat/ws` — receive `hello`/`msg`/`rejected`
  frames; send `{"type":"msg","nick":…,"text":…}` to post as a participant.

Event shape (internally tagged):

```json
{"type":"snapshot","now_playing":{…},"history":[…],"live":false,
 "listeners":3,"up_next":{"label":"Top of hour","track_title":"ID","fires_at":…},
 "active_playlist":"Morning","stream_name":"…","ts":…}
{"type":"update","now_playing":{…},"live":true,"listeners":5,"up_next":null,…}
```

## Write/manage surface today

State changes exist as HTTP endpoints, but they were built for the browser
console, so they have two properties an API client must handle:

1. **They take `application/x-www-form-urlencoded` form bodies**, not JSON, and
   most reply with a `303` redirect rather than a JSON result.
2. **They are protected by a session cookie, not a bearer token.** With
   `disable_auth = true` (see the User Guide) they are open; otherwise a client
   must first `POST /admin/login` with `username`/`password`, keep the returned
   signed `ccc_session` cookie, and send it on every subsequent request.

Representative write actions (all under `/admin`, all form POSTs):

| Action | Path | Fields |
|---|---|---|
| Create blog post | `POST /admin/blog/posts` | `title`, `slug`, `content` (markdown), `status` (`draft`/`published`) |
| Delete blog post | `POST /admin/blog/posts/:id/delete` | — |
| Upload track | `POST /admin/radio/tracks` | multipart `audio` file |
| Delete track | `POST /admin/radio/tracks/:id/delete` | — |
| Save radio settings | `POST /admin/radio` | the Engineering form fields (bitrate, crossfade, relay_url, clock items, …) |
| Pause / resume AutoDJ | `POST /admin/radio/autodj/pause` \| `/resume` | — |
| Start / stop recording | `POST /admin/radio/record/start` \| `/stop` | — |
| Create playlist | `POST /admin/radio/playlists` | playlist fields |
| Compose / send newsletter | `POST /admin/newsletter/send` | issue fields |

## Automating writes today

For an assistant to manage the station **right now**, without code changes,
the workable path is:

1. Run the server with `disable_auth = true` **on a private network the
   assistant can reach but the public cannot** (e.g. localhost, or a
   VPN/Tailscale interface), OR script the login: `POST /admin/login` with the
   operator credentials and reuse the signed `ccc_session` cookie.
2. Send `application/x-www-form-urlencoded` bodies matching the field names
   above; treat a `303`/`302` response as success and re-`GET` the relevant
   read endpoint to confirm the new state.
3. Poll `/health/plugins` and `/api/radio/nowplaying` (or hold the SSE stream
   open) to verify each action landed and to monitor for dead air.

This is enough for "read the now-playing, tell me if we go silent, pause the
AutoDJ, publish this post I drafted." It is **not** a clean, tokened,
JSON-in/JSON-out API — an assistant driving form posts against redirect
responses is workable but brittle.

## Recommended: a real management API

If AI-assisted management is a first-class goal (it sounds like it is), the
right next build is a dedicated `/api/v1/*` surface. It's a natural fit for
this architecture and I can add it as a focused piece of work. Shape:

- **Auth:** a bearer API token (`Authorization: Bearer …`), configured in
  `ccc.toml`, separate from the human login. Read scopes vs. write scopes.
- **JSON in, JSON out** for every resource, with proper status codes and error
  bodies — no HTML, no redirects.
- **Coverage:** posts, episodes, videos, tracks, playlists, clock items,
  radio settings, newsletter, plus the reads that already exist — so an
  assistant can do the full create/update/delete/schedule loop.
- **Introspection:** a machine-readable schema (OpenAPI) so the assistant
  discovers what it can do and validates payloads before sending.
- **Safety:** rate limiting and an audit log of every token-driven change, so
  automated management is reviewable.

This layers cleanly on top of the existing plugin handlers — the business
logic is already written; a v1 API mostly re-exposes it with tokens and JSON.
Say the word and I'll scope and build it.

---

### Quick reference for an assistant integrating today

```
Observe:   GET  /health/plugins            → is anything wrong?
           GET  /api/radio/nowplaying       → what's on, how many listeners
           SSE  /api/radio/nowplaying/sse   → live updates without polling
Act (form POST, session-cookie or disable_auth):
           POST /admin/radio/autodj/pause
           POST /admin/blog/posts           (title, slug, content, status)
           POST /admin/radio/tracks         (multipart audio)
```
