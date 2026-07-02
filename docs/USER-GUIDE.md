# Nuntius User Guide

This guide is for **operators** — the person running the station. No
programming knowledge required. If you want to change the code, see the
[Developer Guide](DEV-GUIDE.md).

## Contents

1. [Starting and stopping](#starting-and-stopping)
2. [First-run setup and signing in](#first-run-setup-and-signing-in)
3. [Naming your site](#naming-your-site)
4. [The radio station](#the-radio-station)
5. [What your listeners see](#what-your-listeners-see)
6. [Publishing: blog, podcasts, video](#publishing-blog-podcasts-video)
7. [Newsletter](#newsletter)
8. [Listener chat](#listener-chat)
9. [Other plugins](#other-plugins)
10. [Backups and recovery](#backups-and-recovery)
11. [Troubleshooting](#troubleshooting)

---

## Starting and stopping

**Windows:** double-click `Launch-Nuntius.cmd` (or the red "Launch Nuntius"
shortcut). The server starts minimized in a console window titled
**Nuntius Server** and your browser opens the admin console. Closing that
console window stops the station. Running the launcher again while the
server is up never starts a duplicate — it just reopens the browser.

**Any platform:**

```bash
./target/release/ccc-cli serve        # or target/debug for dev builds
```

The public site is at `http://<host>:3000`, the admin console at
`http://<host>:3000/admin`. The radio engine additionally listens on its own
TCP port (default **8000**) for broadcast sources and legacy stream players.

> The station is only "24/7" while the process runs. For unattended
> operation, register the command as a Windows service / systemd unit.

## First-run setup and signing in

The first visit to `/admin` opens a one-time setup wizard that creates the
operator account (stored argon2-hashed in `admin.toml`). After that,
`/admin/login` is the normal entry, and **Sign out** lives at the bottom of
the console's left rail.

- **Forgot the password?** Stop the server, delete `admin.toml`, start again —
  the setup wizard runs once more. Nothing else is lost.
- **Don't want a login at all?** (e.g. the server is only reachable from your
  own machine) — add `disable_auth = true` under `[plugin.cms]` in `ccc.toml`
  and restart. The server prints a warning at every boot while this is on.
  **Remove the line before exposing the station to the internet** — an open
  console hands your library, settings, and mail credentials to anyone with
  the URL.

## Naming your site

`ccc.toml`:

```toml
[site]
name = "My Station"
```

This name appears in public page headers, page titles, and podcast RSS
channel names. Two related names you may also want to set:

- **Stream name** (Radio → Engineering): the on-air identity shown on the
  radio pages and in ICY metadata for external players.
- **SEO site name** (`/admin/seo`): used for robots.txt/sitemap metadata.

## The radio station

Everything lives under **Radio** in the admin console, in four tabs.

### Studio (the dashboard)

The live console: ON AIR lamp, current source (LIVE / RELAY / AUTODJ),
listener count, VU meters, now-playing with progress, a **Listeners — last 3
hours** sparkline with the 24-hour peak, and recently played. Two transport
buttons:

- **Stop/Resume AutoDJ** — silences the station without touching the library.
- **Record broadcast** — captures the on-air signal to
  `<library>/recordings/recording-<timestamp>.mp3` until stopped.

### Library

Upload tracks here (MP3, M4A/AAC, FLAC, OGG, Opus — up to 200 MB each).
Title/artist/album/duration are read from the file's tags; loudness is
measured in the background so every track airs at the same volume (EBU R128).
Embedded cover art appears automatically on the listener page.

### Playlists

Group tracks into playlists, **Sequential** or **Shuffle**, and give each a
schedule: always, or a time window with days of the week and a priority.
When no playlist matches the current time, the AutoDJ shuffles the whole
library. The listener page shows the active playlist's name on the source
badge.

### Engineering

The technical settings, saved to `radio.toml`:

- **Stream name / mount / port / source password** — identity and ingest.
  **Change the source password immediately** (default is `hackme`).
- **Bitrate, loudness normalization, crossfade seconds** — sound of the
  AutoDJ. Crossfades are true PCM mixes, not fade-out/fade-in.
- **Clock rotation** — items that fire at a minute of every hour (station
  IDs, jingles): pick the minute, the track, the days of the week. Listeners
  see the next one as "UP NEXT" on the player.
- **Dead-air guard** — if nothing airs for N seconds, a backup rotation of
  chosen tracks takes over (bounded by a maximum duration).
- **Frequency caps** — at most N plays per artist/album within a rolling
  window, so shuffle never repeats an artist back-to-back.
- **Relay** — pull another Icecast/SHOUTcast stream and rebroadcast it. A
  live source always preempts the relay.
- **Remote presenter** — pull a remote studio feed over SRT, RTP, or RTMP
  (with optional SRT passphrase). Sits between live source and AutoDJ in
  priority.
- **HLS output** — optional `.m3u8` stream at `/radio/hls/live.m3u8` for
  apps and players that prefer HLS. Off by default.

### Going live (broadcasting into the station)

Any Icecast-compatible source client works — BUTT, Mixxx, ffmpeg, OBS with an
Icecast output. Point it at:

```
host:      your server
port:      8000            (the radio TCP port, not 3000)
mount:     /listen.mp3
user:      source
password:  <your source password from Engineering>
```

Example with ffmpeg:

```bash
ffmpeg -re -i show.mp3 -c:a libmp3lame -b:a 128k -f mp3 \
  icecast://source:YOURPASSWORD@yourhost:8000/listen.mp3
```

The moment a source connects, the AutoDJ yields, the studio dashboard and
every listener page flip to **LIVE** in real time, and when the source
disconnects the AutoDJ resumes by itself.

**Priority order:** Live source → Relay → Remote presenter → AutoDJ.

## What your listeners see

- `/` — the home page: live player card, latest podcasts, video, upcoming.
- `/radio` — the full player: artwork, live progress, spectrum visualizer,
  play/volume, chat, "UP NEXT" from your clock rotation, transmission log.
  Now-playing updates are pushed instantly — no refreshing.
- **Lock-screen controls**: on phones and desktops the track title, artist,
  and artwork appear in the system media controls.
- **Install as app**: browsers offer "install to home screen" — the station
  runs like a native app.
- Direct stream URLs for external players:
  `http://<host>:3000/radio/stream` (same-origin) or
  `http://<host>:8000/listen.mp3` (classic Icecast mount with ICY metadata).

## Publishing: blog, podcasts, video

- **Blog** (`/admin/blog/posts`): markdown posts with live preview; published
  posts appear at `/blog/` and are sanitized against script injection.
  Publishing fires an event that the newsletter and federation plugins react
  to automatically.
- **Podcasts** (`/admin/podcasts`): upload episodes, publish, and the RSS
  feed at `/podcasts/feed.xml` updates. A separate members-only feed lives at
  `/podcasts/members.xml?token=…` — set `members_token` under
  `[plugin.podcast]` in `ccc.toml` and share the URL privately.
- **Video** (`/admin/videos`): upload a source file; the platform transcodes
  it to adaptive HLS with ffmpeg in the background. Public pages at `/videos`.

## Newsletter

`/admin/newsletter` — subscribers, compose, send. Configure SMTP under its
settings (host, port, credentials, sender). The public signup form is at
`/newsletter/subscribe` and is linked from your pages. When you publish a
blog post, an issue is delivered to subscribers automatically.

## Listener chat

Enabled out of the box when the `live-chat` plugin is on: a chat rail appears
beside the player. `/admin/live-chat` controls it: on/off, slow mode
(seconds between messages per person), message length, history size,
connection cap, and nickname bans. Chat history lives in memory only — it
clears on restart by design.

## Other plugins

| Plugin | Where | What it does |
|---|---|---|
| Stream Watch | `/admin/stream-watch` | Dead-air watchdog that probes your own stream and alerts on silence |
| Analytics | `/admin/analytics` | Recent request stats (in-memory ring buffer) |
| SEO | `/admin/seo` | robots.txt, sitemap.xml, site metadata |
| Federation | `/admin/federation` | ActivityPub actor — people on Mastodon & co. can follow your station; posts federate automatically |
| Transcripts | `/admin/transcripts` | Whisper speech-to-text for uploaded audio (needs a `whisper` binary on the machine) |
| Storage | `/admin/storage` | Optional S3/R2/B2 off-site storage for media |
| Ads | `/admin/ads` | Simple creative rotation served at `/api/ads/next` |

Enable/disable any of them in the `[plugins]` list in `ccc.toml`. A disabled
plugin costs nothing — it is never even constructed.

## Backups and recovery

Everything that matters is a handful of files next to the binary:

| File(s) | Contents |
|---|---|
| `ccc.redb` | The database: posts, episodes, tracks metadata, playlists, subscribers, stats |
| `media/` | Uploaded audio/video files and recordings |
| `ccc.toml` | Which plugins run + per-plugin config pointers |
| `radio.toml`, `admin.toml`, `federation.toml`, `newsletter.toml`, … | Per-plugin settings, credentials, and keys |

Copy those while the server is stopped and you have a complete backup.
`admin.toml` and `federation.toml` contain secrets (password hash, signing
keys) — treat them accordingly.

## Troubleshooting

- **The station is silent** — check, in order: is AutoDJ paused (Studio tab)?
  Does the Library have tracks with a duration (untagged files show `—` and
  are skipped)? Is `ffmpeg` on PATH? The dead-air guard needs backup tracks
  configured before it can cover gaps.
- **A live source can't connect** — user must be `source`, the password must
  match Engineering, and the client must dial port **8000** (not 3000).
- **"Plugin 'x' failed to start" at boot** — the error names the config file
  and section to fix. The server refuses to start rather than run half-broken;
  fix the named file or remove the plugin from `enabled`.
- **Locked out of `/admin`** — delete `admin.toml`, restart, run setup again.
- **Listener count looks wrong** — Stream Watch's probe counts as one
  listener while it is enabled.
- **Logs** — the Windows launcher writes `nuntius-server.out.log` and
  `nuntius-server.err.log` next to the binary. Run the CLI with `-d`, `-dd`
  or `RUST_LOG=debug` for more detail.
