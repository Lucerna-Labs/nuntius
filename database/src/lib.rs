#![allow(clippy::result_large_err)]
// `redb::Error` is ~160 bytes; clippy::result_large_err is upstream-driven and
// not actionable without wrapping every call site.

use redb::{Database, Error, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

/// The main table where raw blog posts are stored.
/// Key: Post ID (u64)
/// Value: Serialized BlogPost
const BLOG_POSTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("blog_posts");

/// Chronological index of *Published* blog posts only (drafts are excluded
/// so the public reader doesn't leak unpublished work).
/// Key: `[zero-padded published_at]_[post_id]`
/// Value: Post ID (u64)
const BLOG_POSTS_CHRONO_INDEX: TableDefinition<&str, u64> =
    TableDefinition::new("blog_posts_chrono");

/// Slug → ID lookup for the default (legacy) blog site (site_id = 0).
const BLOG_POSTS_SLUG_INDEX: TableDefinition<&str, u64> = TableDefinition::new("blog_posts_slug");

/// Blog sites (multi-domain / multi-subdomain blogs).
/// Key: Site ID (u64, >= 1; 0 is reserved for the legacy default blog).
/// Value: JSON-serialized `BlogSite`.
const BLOG_SITES_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("blog_sites");

/// Site slug → Site ID lookup (unique across all blog sites).
const BLOG_SITE_SLUG_INDEX: TableDefinition<&str, u64> = TableDefinition::new("blog_site_slug");

/// Normalized domain/subdomain host → Site ID lookup (unique across all blog sites).
const BLOG_SITE_HOST_INDEX: TableDefinition<&str, u64> = TableDefinition::new("blog_site_host");

/// Post ID → Site ID mapping. Absent entries belong to site_id = 0 (legacy default).
const BLOG_POST_SITE_MAP: TableDefinition<u64, u64> = TableDefinition::new("blog_post_site_map");

/// Per-site slug → Post ID lookup. Allows distinct blogs to reuse the same post slugs.
const BLOG_POST_SITE_SLUG_INDEX: TableDefinition<(u64, &str), u64> =
    TableDefinition::new("blog_post_site_slug");

/// AI Team bots (Agents plugin).
const AGENT_BOTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("agent_bots");

/// Chat messages per bot: Key = (bot_id, message_id), Value = JSON `AgentMessage`.
const AGENT_MESSAGES_TABLE: TableDefinition<(u64, u64), &[u8]> =
    TableDefinition::new("agent_messages");

/// Pending / resolved bot action proposals (Approve-what-matters queue).
const AGENT_PROPOSALS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("agent_proposals");

/// Transcription jobs (Whisper plugin).
/// Key: Job ID (u64 == unix-second creation time)
/// Value: Serialized `TranscriptionJob`
const TRANSCRIPTION_JOBS_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("transcription_jobs");

/// Newsletter subscribers (Newsletter plugin).
const NEWSLETTER_SUBSCRIBERS_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("newsletter_subscribers");
/// Email -> ID lookup (uniqueness enforcement).
const NEWSLETTER_EMAIL_INDEX: TableDefinition<&str, u64> =
    TableDefinition::new("newsletter_email_index");

/// Ads (Ads plugin).
const ADS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("ads");

/// Federation followers (ActivityPub plugin).
/// Key: follower actor URL (e.g. https://mastodon.social/users/alice)
/// Value: Serialized FederationFollower
const FEDERATION_FOLLOWERS_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("federation_followers");

/// Monotonic per-name counters. Plugins use these to assign primary keys
/// (podcast episode id, video id, transcription job id) instead of
/// `SystemTime::as_secs()`, which is wall-clock based and can collide when
/// two writes land in the same second.
/// Key: Counter name (e.g. "podcast_episode")
/// Value: Next id to hand out (u64; first allocation is 1)
const COUNTERS_TABLE: TableDefinition<&str, u64> = TableDefinition::new("counters");

/// Radio playlists.
/// Key: Playlist ID (u64)
/// Value: Serialized `Playlist`
const PLAYLISTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("radio_playlists");
/// Track entries within a playlist, indexed for fast scan.
/// Key: (playlist_id, position) — gives ordered range scans per playlist.
/// Value: track_id (u64)
const PLAYLIST_TRACKS_TABLE: TableDefinition<(u64, u64), u64> =
    TableDefinition::new("radio_playlist_tracks");

/// The main table where tracks are stored.
/// Key: Track ID (u64)
/// Value: Serialized Track
const TRACKS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("tracks");

/// The main table where podcast episodes are stored.
/// Key: Episode ID (u64)
/// Value: Serialized PodcastEpisode
const PODCASTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("podcasts");

/// The custom index table for quickly querying the latest podcast episodes.
/// Indexes ONLY published episodes (drafts excluded so the public RSS
/// feed doesn't leak unpublished work).
/// Key: Composite string of `[zero-padded published_at]_[episode_id]`
/// Value: Episode ID (u64)
const PODCASTS_CHRONO_INDEX: TableDefinition<&str, u64> = TableDefinition::new("podcasts_chrono");

/// Slug → episode-id lookup. Slugs unique across drafts + published.
const PODCASTS_SLUG_INDEX: TableDefinition<&str, u64> = TableDefinition::new("podcasts_slug_index");

/// The main table where video podcast metadata is stored.
/// Key: Video ID (u64)
/// Value: Serialized VideoPodcast
const VIDEO_PODCASTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("video_podcasts");

/// Chronological index of *Published* video podcasts only.
const VIDEO_PODCASTS_CHRONO_INDEX: TableDefinition<&str, u64> =
    TableDefinition::new("video_podcasts_chrono");

const VIDEO_PODCASTS_SLUG_INDEX: TableDefinition<&str, u64> =
    TableDefinition::new("video_podcasts_slug_index");

/// Listener statistics (radio plugin).
/// One row per minute holds the peak concurrent listener count observed
/// during that minute. u64 keys sort numerically in redb, so range scans
/// over `[from_ts, to_ts]` come back in chronological order for free.
/// Key: unix timestamp rounded down to the minute (seconds, u64)
/// Value: peak concurrent listeners observed during that minute (u32)
const LISTENER_STATS_TABLE: TableDefinition<u64, u32> = TableDefinition::new("listener_stats");

// ----------------------------------------------------------------------------
// bincode helpers
//
// `bincode::serialize`/`deserialize` return `Result`, but the entire database
// layer used to `.unwrap()` them. A single corrupted row, a partial write from
// a crashed save, or a future schema mismatch then panicked the axum worker
// thread (a panic on `Result::unwrap()` in a worker becomes a `JoinError` and
// the request just hangs). The helpers below map both directions to
// `redb::Error::Corrupted` so a bad row is a clean error response, not a
// crash. Use these everywhere instead of calling bincode directly.
// ----------------------------------------------------------------------------

fn decode<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, Error> {
    bincode::deserialize(raw)
        .map_err(|e| Error::Corrupted(format!("bincode decode ({} bytes): {e}", raw.len())))
}

fn encode<T: serde::Serialize>(val: &T) -> Result<Vec<u8>, Error> {
    bincode::serialize(val).map_err(|e| Error::Corrupted(format!("bincode encode: {e}")))
}

fn decode_json<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(raw)
        .map_err(|e| Error::Corrupted(format!("json decode ({} bytes): {e}", raw.len())))
}

fn encode_json<T: serde::Serialize>(val: &T) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(val).map_err(|e| Error::Corrupted(format!("json encode: {e}")))
}

fn default_blog_theme() -> String {
    "signal".into()
}

/// Normalize a domain or subdomain host string for storage and matching:
/// trims whitespace, strips scheme (`http://`/`https://`), strips any path
/// or port, lowercases, and strips trailing dots.
pub fn normalize_host(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    let s = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let s = s.split('/').next().unwrap_or("");
    let s = s.split(':').next().unwrap_or("");
    s.trim_end_matches('.').to_string()
}

/// A distinct blog site mapped to one or more domains or subdomains.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BlogSite {
    pub id: u64,
    pub name: String,
    /// URL-safe identifier used for `/sites/:slug/` preview/path access.
    pub slug: String,
    /// Normalized hostnames routed to this blog (e.g. `notes.lucernamedia.com`, `myblog.org`).
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// Visual accent theme: `"signal"`, `"phosphor"`, or `"ink"`.
    #[serde(default = "default_blog_theme")]
    pub theme: String,
    pub created_at: u64,
    pub updated_at: u64,
}

/// An AI bot contact inside Nuntius Creator Command Center.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentBot {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub system_prompt: String,
    /// Provider label configured by the operator (e.g. `"api"`, `"custom"`).
    pub provider: String,
    /// Model identifier configured by the operator.
    pub model: String,
    /// Optional env var name holding the operator's API key.
    #[serde(default)]
    pub api_key_env: String,
    /// Default target blog `site_id` when this bot drafts blog posts (0 = default blog).
    #[serde(default)]
    pub default_blog_site_id: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

/// A persisted chat message in a bot's conversation thread.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentMessage {
    pub id: u64,
    pub bot_id: u64,
    /// `"user"` or `"assistant"`
    pub role: String,
    pub content: String,
    pub created_at: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProposalStatus {
    #[default]
    Pending,
    Approved,
    Rejected,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Pending => "pending",
            ProposalStatus::Approved => "approved",
            ProposalStatus::Rejected => "rejected",
        }
    }
}

/// A proposed Nuntius write action waiting for operator approval.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentProposal {
    pub id: u64,
    pub bot_id: u64,
    /// Currently `"draft_blog_post"`.
    pub kind: String,
    pub site_id: u64,
    pub title: String,
    pub slug: String,
    pub content: String,
    pub status: ProposalStatus,
    pub created_at: u64,
}

/// Publication state of a blog post.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum PostStatus {
    #[default]
    Draft,
    Published,
}

impl PostStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PostStatus::Draft => "draft",
            PostStatus::Published => "published",
        }
    }

    pub fn from_form(s: &str) -> Self {
        match s {
            "published" => PostStatus::Published,
            _ => PostStatus::Draft,
        }
    }
}

/// A blog post.
///
/// `slug` is the URL fragment under `/blog/:slug`; it must be unique
/// per published post (enforced at write time).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BlogPost {
    pub id: u64,
    pub slug: String,
    pub title: String,
    /// Markdown body.
    pub content: String,
    pub status: PostStatus,
    /// Unix seconds when the row was first created.
    pub created_at: u64,
    /// Unix seconds when the row was last edited.
    pub updated_at: u64,
    /// Unix seconds of the most recent transition to Published, if any.
    /// Drives the chronological index used by `/blog/`.
    pub published_at: Option<u64>,
}

/// Lifecycle of a transcription job.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranscriptionStatus {
    #[default]
    Queued,
    Running,
    Completed,
    Failed,
}

impl TranscriptionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TranscriptionStatus::Queued => "queued",
            TranscriptionStatus::Running => "running",
            TranscriptionStatus::Completed => "completed",
            TranscriptionStatus::Failed => "failed",
        }
    }
}

/// A transcription job — one audio file → one transcript.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TranscriptionJob {
    pub id: u64,
    /// Filesystem path to the audio file the user wants transcribed.
    pub audio_path: String,
    pub status: TranscriptionStatus,
    /// Whisper-style language hint, e.g. "auto", "en". Plugin defaults
    /// from settings if blank.
    pub language: String,
    /// Plugin model name passed to whisper (e.g. "base", "small").
    pub model: String,
    /// The latest transcript text. Editable by the operator after the
    /// job completes — supports manual correction without re-running.
    pub transcript: Option<String>,
    /// Set when status == Failed.
    pub error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// An ad creative — the unit served via `/api/ads/next`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Ad {
    pub id: u64,
    pub name: String,
    pub media_url: String,
    pub click_url: Option<String>,
    pub active: bool,
    /// Selection weight for round-robin / weighted random. Higher = more often.
    pub weight: u32,
    pub created_at: u64,
    pub updated_at: u64,
}

/// A remote ActivityPub actor that has followed our station.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FederationFollower {
    /// Remote actor URL — also the table key.
    pub actor_url: String,
    /// Their inbox URL (where we deliver Create Note activities).
    pub inbox_url: String,
    /// Optional preferred username for display ("alice").
    pub preferred_username: Option<String>,
    /// Optional display name.
    pub name: Option<String>,
    pub followed_at: u64,
}

/// A newsletter subscriber.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Subscriber {
    pub id: u64,
    pub email: String,
    pub active: bool,
    pub created_at: u64,
}

/// A music track in the radio station's library.
///
/// `file_path` points at the on-disk audio file the AutoDJ streams.
/// `duration_seconds` and `size_bytes` together let the streaming
/// pacer compute the effective bitrate and chunk the file at real time.
/// `target_gain_db` is the EBU R128-derived gain adjustment that
/// brings the track to the station's target loudness (-23 LUFS by
/// default). Measured asynchronously via ffmpeg's loudnorm filter
/// after upload; `None` until measurement completes.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Track {
    pub id: u64,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub genre: String,
    pub file_path: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub duration_seconds: u64,
    pub created_at: u64,
    /// EBU R128 loudness offset, negative or positive dB. None = unmeasured.
    #[serde(default)]
    pub target_gain_db: Option<f32>,
}

/// How AutoDJ traverses a playlist when it's active.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaylistKind {
    /// Walk tracks in the order the operator added them.
    #[default]
    Sequential,
    /// Reshuffle the playlist each pass.
    Shuffle,
}

impl PlaylistKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PlaylistKind::Sequential => "sequential",
            PlaylistKind::Shuffle => "shuffle",
        }
    }
    pub fn from_form(s: &str) -> Self {
        match s {
            "shuffle" => PlaylistKind::Shuffle,
            _ => PlaylistKind::Sequential,
        }
    }
}

/// When this playlist should be selected by AutoDJ.
///
/// `Always` plays whenever no higher-priority playlist matches.
/// `TimeWindow` plays only during the specified daily window on the
/// specified weekdays. Times are minutes-since-midnight in the
/// server's local time.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub enum PlaylistSchedule {
    #[default]
    Always,
    TimeWindow {
        /// 0..=1439 (00:00 to 23:59).
        start_minute: u16,
        end_minute: u16,
        /// Bit mask: bit 0 = Sunday, bit 1 = Monday, ... bit 6 = Saturday.
        /// 0x7F = every day. 0 = never (effectively disabled).
        days_mask: u8,
    },
}

/// A named, scheduled collection of tracks.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Playlist {
    pub id: u64,
    pub name: String,
    pub kind: PlaylistKind,
    pub schedule: PlaylistSchedule,
    /// Higher beats lower when multiple playlists match the current time.
    pub priority: i32,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Publication state of a podcast episode.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum EpisodeStatus {
    #[default]
    Draft,
    Published,
}

impl EpisodeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            EpisodeStatus::Draft => "draft",
            EpisodeStatus::Published => "published",
        }
    }
    pub fn from_form(s: &str) -> Self {
        match s {
            "published" => EpisodeStatus::Published,
            _ => EpisodeStatus::Draft,
        }
    }
}

/// A podcast episode.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PodcastEpisode {
    pub id: u64,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub audio_path: String,
    /// e.g. "audio/mpeg", "audio/mp4". Drives RSS enclosure type.
    pub mime_type: String,
    /// Bytes on disk — required for the RSS `enclosure length` attribute.
    pub size_bytes: u64,
    /// 0 if unknown — populated by the upload pipeline if it can probe.
    pub duration_seconds: u64,
    pub status: EpisodeStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub published_at: Option<u64>,
    /// Foreign key into TRANSCRIPTION_JOBS_TABLE, if a job was queued.
    pub transcript_job_id: Option<u64>,
    /// True iff this episode should only appear in the members-only feed
    /// (`/podcasts/members.xml?token=...`). Drops out of the public feed.
    #[serde(default)]
    pub members_only: bool,
}

/// Lifecycle of a video podcast episode:
/// upload -> Transcoding (ffmpeg) -> Draft (ready, manually published) -> Published
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoStatus {
    #[default]
    Transcoding,
    Draft,
    Published,
    Failed,
}

impl VideoStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            VideoStatus::Transcoding => "transcoding",
            VideoStatus::Draft => "draft",
            VideoStatus::Published => "published",
            VideoStatus::Failed => "failed",
        }
    }
}

/// A video podcast episode.
///
/// `source_path` is the original uploaded file (kept on disk so re-runs
/// of the transcoder are possible). `hls_directory` holds the segmented
/// `.m3u8` + `.ts` files generated by ffmpeg.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VideoPodcast {
    pub id: u64,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub source_path: String,
    pub hls_directory: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub duration_seconds: u64,
    pub status: VideoStatus,
    pub error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub published_at: Option<u64>,
}

/// The main Database manager for Nuntius (CCC)
pub struct CccDB {
    db: Database,
}

impl CccDB {
    /// Initialize the database from a file path.
    pub fn new(path: &str) -> Result<Self, Error> {
        let db = Database::create(path)?;

        // Ensure our tables are created
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(BLOG_POSTS_TABLE)?;
            write_txn.open_table(BLOG_POSTS_CHRONO_INDEX)?;
            write_txn.open_table(BLOG_POSTS_SLUG_INDEX)?;
            write_txn.open_table(BLOG_SITES_TABLE)?;
            write_txn.open_table(BLOG_SITE_SLUG_INDEX)?;
            write_txn.open_table(BLOG_SITE_HOST_INDEX)?;
            write_txn.open_table(BLOG_POST_SITE_MAP)?;
            write_txn.open_table(BLOG_POST_SITE_SLUG_INDEX)?;
            write_txn.open_table(AGENT_BOTS_TABLE)?;
            write_txn.open_table(AGENT_MESSAGES_TABLE)?;
            write_txn.open_table(AGENT_PROPOSALS_TABLE)?;
            write_txn.open_table(TRANSCRIPTION_JOBS_TABLE)?;
            write_txn.open_table(NEWSLETTER_SUBSCRIBERS_TABLE)?;
            write_txn.open_table(NEWSLETTER_EMAIL_INDEX)?;
            write_txn.open_table(ADS_TABLE)?;
            write_txn.open_table(FEDERATION_FOLLOWERS_TABLE)?;
            write_txn.open_table(PLAYLISTS_TABLE)?;
            write_txn.open_table(PLAYLIST_TRACKS_TABLE)?;
            write_txn.open_table(TRACKS_TABLE)?;
            write_txn.open_table(PODCASTS_TABLE)?;
            write_txn.open_table(PODCASTS_CHRONO_INDEX)?;
            write_txn.open_table(PODCASTS_SLUG_INDEX)?;
            write_txn.open_table(VIDEO_PODCASTS_TABLE)?;
            write_txn.open_table(VIDEO_PODCASTS_CHRONO_INDEX)?;
            write_txn.open_table(VIDEO_PODCASTS_SLUG_INDEX)?;
            write_txn.open_table(LISTENER_STATS_TABLE)?;
            write_txn.open_table(COUNTERS_TABLE)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

    /// Allocate the next id for a named counter. Atomic across writers.
    /// Returns 1 on the first call for a given name, then 2, 3, ...
    /// Use this instead of `SystemTime::as_secs()` for any primary key
    /// that can be written more than once per second (CI-driven podcast
    /// imports, batch uploads, etc.).
    pub fn next_id(&self, name: &str) -> Result<u64, Error> {
        let write_txn = self.db.begin_write()?;
        let next = {
            let mut counters = write_txn.open_table(COUNTERS_TABLE)?;
            let current = counters.get(name)?.map(|v| v.value()).unwrap_or(0);
            let next = current + 1;
            counters.insert(name, next)?;
            next
        };
        write_txn.commit()?;
        Ok(next)
    }

    // --- BLOG SITES (MULTI-DOMAIN / MULTI-SUBDOMAIN) ---

    /// Insert or update a blog site. Enforces unique `slug` and unique
    /// normalized hostnames across all sites.
    pub fn upsert_blog_site(&self, site: &BlogSite) -> Result<(), Error> {
        if site.id == 0 {
            return Err(Error::Corrupted(
                "blog site id 0 is reserved for the legacy default blog".into(),
            ));
        }
        let slug = site.slug.trim().to_ascii_lowercase();
        if slug.is_empty() {
            return Err(Error::Corrupted("blog site slug cannot be empty".into()));
        }

        let mut normalized_hosts = Vec::new();
        for h in &site.hosts {
            let n = normalize_host(h);
            if !n.is_empty() && !normalized_hosts.contains(&n) {
                normalized_hosts.push(n);
            }
        }

        let mut clean = site.clone();
        clean.slug = slug;
        clean.hosts = normalized_hosts;

        let write_txn = self.db.begin_write()?;
        {
            let mut sites_table = write_txn.open_table(BLOG_SITES_TABLE)?;
            let mut slug_table = write_txn.open_table(BLOG_SITE_SLUG_INDEX)?;
            let mut host_table = write_txn.open_table(BLOG_SITE_HOST_INDEX)?;

            if let Some(existing_id) = slug_table.get(clean.slug.as_str())? {
                if existing_id.value() != clean.id {
                    return Err(Error::Corrupted(format!(
                        "blog site slug '{}' already in use by site {}",
                        clean.slug,
                        existing_id.value()
                    )));
                }
            }
            for host in &clean.hosts {
                if let Some(existing_id) = host_table.get(host.as_str())? {
                    if existing_id.value() != clean.id {
                        return Err(Error::Corrupted(format!(
                            "domain '{}' is already assigned to another blog (site {})",
                            host,
                            existing_id.value()
                        )));
                    }
                }
            }

            // Clean up previous slug and host bindings if updating an existing site.
            if let Some(prev_raw) = sites_table.get(clean.id)? {
                let prev: BlogSite = decode_json(prev_raw.value())?;
                if prev.slug != clean.slug {
                    slug_table.remove(prev.slug.as_str())?;
                }
                for old_host in &prev.hosts {
                    if !clean.hosts.contains(old_host) {
                        host_table.remove(old_host.as_str())?;
                    }
                }
            }

            let encoded = encode_json(&clean)?;
            sites_table.insert(clean.id, encoded.as_slice())?;
            slug_table.insert(clean.slug.as_str(), clean.id)?;
            for host in &clean.hosts {
                host_table.insert(host.as_str(), clean.id)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Delete a blog site and all posts assigned to it.
    pub fn delete_blog_site(&self, id: u64) -> Result<(), Error> {
        let post_ids: Vec<u64> = self
            .all_blog_posts_for_site(id)?
            .into_iter()
            .map(|p| p.id)
            .collect();
        for pid in post_ids {
            self.delete_blog_post(pid)?;
        }

        let write_txn = self.db.begin_write()?;
        {
            let mut sites_table = write_txn.open_table(BLOG_SITES_TABLE)?;
            let mut slug_table = write_txn.open_table(BLOG_SITE_SLUG_INDEX)?;
            let mut host_table = write_txn.open_table(BLOG_SITE_HOST_INDEX)?;

            if let Some(prev_raw) = sites_table.get(id)? {
                let prev: BlogSite = decode_json(prev_raw.value())?;
                slug_table.remove(prev.slug.as_str())?;
                for host in &prev.hosts {
                    host_table.remove(host.as_str())?;
                }
            }
            sites_table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Get a blog site by id.
    pub fn get_blog_site(&self, id: u64) -> Result<Option<BlogSite>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(BLOG_SITES_TABLE)?;
        table
            .get(id)?
            .map(|raw| decode_json(raw.value()))
            .transpose()
    }

    /// Get a blog site by its URL slug.
    pub fn get_blog_site_by_slug(&self, slug: &str) -> Result<Option<BlogSite>, Error> {
        let read_txn = self.db.begin_read()?;
        let slug_table = read_txn.open_table(BLOG_SITE_SLUG_INDEX)?;
        let sites_table = read_txn.open_table(BLOG_SITES_TABLE)?;
        let key = slug.trim().to_ascii_lowercase();
        let id = match slug_table.get(key.as_str())? {
            Some(v) => v.value(),
            None => return Ok(None),
        };
        sites_table
            .get(id)?
            .map(|raw| decode_json(raw.value()))
            .transpose()
    }

    /// Get a blog site matching an incoming HTTP `Host` header value.
    pub fn get_blog_site_by_host(&self, raw_host: &str) -> Result<Option<BlogSite>, Error> {
        let host = normalize_host(raw_host);
        if host.is_empty() {
            return Ok(None);
        }
        let read_txn = self.db.begin_read()?;
        let host_table = read_txn.open_table(BLOG_SITE_HOST_INDEX)?;
        let sites_table = read_txn.open_table(BLOG_SITES_TABLE)?;
        let id = match host_table.get(host.as_str())? {
            Some(v) => v.value(),
            None => return Ok(None),
        };
        sites_table
            .get(id)?
            .map(|raw| decode_json(raw.value()))
            .transpose()
    }

    /// All configured blog sites, ordered by name.
    pub fn all_blog_sites(&self) -> Result<Vec<BlogSite>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(BLOG_SITES_TABLE)?;
        let mut out: Vec<BlogSite> = table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode_json(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(out)
    }

    /// Return the `site_id` owning `post_id` (0 = legacy default blog).
    pub fn get_blog_post_site_id(&self, post_id: u64) -> Result<u64, Error> {
        let read_txn = self.db.begin_read()?;
        let map = read_txn.open_table(BLOG_POST_SITE_MAP)?;
        Ok(map.get(post_id)?.map(|v| v.value()).unwrap_or(0))
    }

    // --- BLOG POSTS ---

    fn chrono_key(published_at: u64, id: u64) -> String {
        format!("{:020}_{}", published_at, id)
    }

    /// Insert or update a blog post on the default blog site (`site_id = 0`).
    pub fn upsert_blog_post(&self, post: &BlogPost) -> Result<(), Error> {
        self.upsert_blog_post_for_site(0, post)
    }

    /// Insert or update a blog post on a specific `site_id` (0 = legacy default blog).
    /// Enforces slug uniqueness *within* that site (allowing distinct blogs to reuse slugs)
    /// and keeps the chrono index synchronized with `status`.
    pub fn upsert_blog_post_for_site(&self, site_id: u64, post: &BlogPost) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main_table = write_txn.open_table(BLOG_POSTS_TABLE)?;
            let mut chrono_table = write_txn.open_table(BLOG_POSTS_CHRONO_INDEX)?;
            let mut slug_table = write_txn.open_table(BLOG_POSTS_SLUG_INDEX)?;
            let mut site_map = write_txn.open_table(BLOG_POST_SITE_MAP)?;
            let mut site_slug_table = write_txn.open_table(BLOG_POST_SITE_SLUG_INDEX)?;

            // Per-site slug uniqueness: if another post in the same site owns this slug, refuse.
            if let Some(existing_id) = site_slug_table.get((site_id, post.slug.as_str()))? {
                if existing_id.value() != post.id {
                    return Err(Error::Corrupted(format!(
                        "blog slug '{}' already in use by post {}",
                        post.slug,
                        existing_id.value()
                    )));
                }
            }
            if site_id == 0 {
                if let Some(existing_id) = slug_table.get(post.slug.as_str())? {
                    if existing_id.value() != post.id {
                        return Err(Error::Corrupted(format!(
                            "blog slug '{}' already in use by post {}",
                            post.slug,
                            existing_id.value()
                        )));
                    }
                }
            }

            // Drop any stale row+indexes for this id (handles slug renames,
            // site moves, and status flips cleanly).
            let prev_site_id = site_map.get(post.id)?.map(|v| v.value()).unwrap_or(0);
            if let Some(prev_raw) = main_table.get(post.id)? {
                let prev: BlogPost = decode(prev_raw.value())?;
                if prev.slug != post.slug || prev_site_id != site_id {
                    site_slug_table.remove((prev_site_id, prev.slug.as_str()))?;
                    if prev_site_id == 0 {
                        slug_table.remove(prev.slug.as_str())?;
                    }
                }
                if let Some(ts) = prev.published_at {
                    chrono_table.remove(Self::chrono_key(ts, prev.id).as_str())?;
                }
            }

            let encoded: Vec<u8> = encode(post)?;
            main_table.insert(post.id, encoded.as_slice())?;
            site_map.insert(post.id, site_id)?;
            site_slug_table.insert((site_id, post.slug.as_str()), post.id)?;
            if site_id == 0 {
                slug_table.insert(post.slug.as_str(), post.id)?;
            }

            if post.status == PostStatus::Published {
                if let Some(ts) = post.published_at {
                    chrono_table.insert(Self::chrono_key(ts, post.id).as_str(), post.id)?;
                }
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Delete a blog post and clean up its index entries.
    pub fn delete_blog_post(&self, id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main_table = write_txn.open_table(BLOG_POSTS_TABLE)?;
            let mut chrono_table = write_txn.open_table(BLOG_POSTS_CHRONO_INDEX)?;
            let mut slug_table = write_txn.open_table(BLOG_POSTS_SLUG_INDEX)?;
            let mut site_map = write_txn.open_table(BLOG_POST_SITE_MAP)?;
            let mut site_slug_table = write_txn.open_table(BLOG_POST_SITE_SLUG_INDEX)?;

            let prev_site_id = site_map.get(id)?.map(|v| v.value()).unwrap_or(0);
            if let Some(prev_raw) = main_table.get(id)? {
                let prev: BlogPost = decode(prev_raw.value())?;
                site_slug_table.remove((prev_site_id, prev.slug.as_str()))?;
                if prev_site_id == 0 {
                    slug_table.remove(prev.slug.as_str())?;
                }
                if let Some(ts) = prev.published_at {
                    chrono_table.remove(Self::chrono_key(ts, prev.id).as_str())?;
                }
            }
            site_map.remove(id)?;
            main_table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Single post by id. Returns None if not found.
    pub fn get_blog_post(&self, id: u64) -> Result<Option<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(BLOG_POSTS_TABLE)?;
        table.get(id)?.map(|raw| decode(raw.value())).transpose()
    }

    /// Single post by slug on the legacy default blog (`site_id = 0`).
    pub fn get_blog_post_by_slug(&self, slug: &str) -> Result<Option<BlogPost>, Error> {
        self.get_blog_post_by_site_and_slug(0, slug)
    }

    /// Single post by `(site_id, slug)`. For `site_id == 0`, falls back to the
    /// legacy `BLOG_POSTS_SLUG_INDEX` so pre-multisite rows remain readable.
    pub fn get_blog_post_by_site_and_slug(
        &self,
        site_id: u64,
        slug: &str,
    ) -> Result<Option<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let site_slug_table = read_txn.open_table(BLOG_POST_SITE_SLUG_INDEX)?;
        let legacy_slug_table = read_txn.open_table(BLOG_POSTS_SLUG_INDEX)?;
        let main_table = read_txn.open_table(BLOG_POSTS_TABLE)?;

        let id = match site_slug_table.get((site_id, slug))? {
            Some(v) => v.value(),
            None if site_id == 0 => match legacy_slug_table.get(slug)? {
                Some(v) => v.value(),
                None => return Ok(None),
            },
            None => return Ok(None),
        };
        main_table
            .get(id)?
            .map(|raw| decode(raw.value()))
            .transpose()
    }

    /// Most-recent Published posts across all blogs.
    pub fn latest_published_blog_posts(&self, limit: usize) -> Result<Vec<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let chrono_table = read_txn.open_table(BLOG_POSTS_CHRONO_INDEX)?;
        let main_table = read_txn.open_table(BLOG_POSTS_TABLE)?;

        let mut out = Vec::new();
        for item in chrono_table.range("0"..="9")?.rev().take(limit) {
            let (_, id) = item?;
            if let Some(raw) = main_table.get(id.value())? {
                out.push(decode(raw.value())?);
            }
        }
        Ok(out)
    }

    /// Most-recent Published posts for a single `site_id` (0 = legacy default blog).
    pub fn latest_published_blog_posts_for_site(
        &self,
        site_id: u64,
        limit: usize,
    ) -> Result<Vec<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let chrono_table = read_txn.open_table(BLOG_POSTS_CHRONO_INDEX)?;
        let main_table = read_txn.open_table(BLOG_POSTS_TABLE)?;
        let site_map = read_txn.open_table(BLOG_POST_SITE_MAP)?;

        let mut out = Vec::new();
        for item in chrono_table.range("0"..="9")?.rev() {
            if out.len() >= limit {
                break;
            }
            let (_, id) = item?;
            let pid = id.value();
            let owner = site_map.get(pid)?.map(|v| v.value()).unwrap_or(0);
            if owner == site_id {
                if let Some(raw) = main_table.get(pid)? {
                    out.push(decode(raw.value())?);
                }
            }
        }
        Ok(out)
    }

    /// Every post (drafts + published) across all sites, newest first.
    pub fn all_blog_posts(&self) -> Result<Vec<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let main_table = read_txn.open_table(BLOG_POSTS_TABLE)?;

        let mut out: Vec<BlogPost> = main_table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    /// Every post (drafts + published) belonging to a single `site_id`, newest first.
    pub fn all_blog_posts_for_site(&self, site_id: u64) -> Result<Vec<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let main_table = read_txn.open_table(BLOG_POSTS_TABLE)?;
        let site_map = read_txn.open_table(BLOG_POST_SITE_MAP)?;

        let mut out: Vec<BlogPost> = Vec::new();
        for item in main_table.iter()? {
            let (id, raw) = item?;
            let owner = site_map.get(id.value())?.map(|v| v.value()).unwrap_or(0);
            if owner == site_id {
                out.push(decode(raw.value())?);
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    // --- AGENT BOTS, MESSAGES & PROPOSALS ---

    pub fn upsert_agent_bot(&self, bot: &AgentBot) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(AGENT_BOTS_TABLE)?;
            let encoded = encode_json(bot)?;
            table.insert(bot.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_agent_bot(&self, id: u64) -> Result<Option<AgentBot>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(AGENT_BOTS_TABLE)?;
        table
            .get(id)?
            .map(|raw| decode_json(raw.value()))
            .transpose()
    }

    pub fn all_agent_bots(&self) -> Result<Vec<AgentBot>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(AGENT_BOTS_TABLE)?;
        let mut out: Vec<AgentBot> = table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode_json(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by_key(|b| b.id);
        Ok(out)
    }

    pub fn delete_agent_bot(&self, id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut bots = write_txn.open_table(AGENT_BOTS_TABLE)?;
            bots.remove(id)?;

            let mut msgs = write_txn.open_table(AGENT_MESSAGES_TABLE)?;
            let msg_keys: Vec<(u64, u64)> = msgs
                .range((id, 0u64)..=(id, u64::MAX))?
                .filter_map(|r| r.ok())
                .map(|(k, _)| k.value())
                .collect();
            for k in msg_keys {
                msgs.remove(k)?;
            }

            let mut props = write_txn.open_table(AGENT_PROPOSALS_TABLE)?;
            let prop_ids: Vec<u64> = props
                .iter()?
                .filter_map(|r| r.ok())
                .filter_map(|(k, v)| {
                    decode_json::<AgentProposal>(v.value())
                        .ok()
                        .filter(|p| p.bot_id == id)
                        .map(|_| k.value())
                })
                .collect();
            for pid in prop_ids {
                props.remove(pid)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn append_agent_message(&self, msg: &AgentMessage) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut msgs = write_txn.open_table(AGENT_MESSAGES_TABLE)?;
            let encoded = encode_json(msg)?;
            msgs.insert((msg.bot_id, msg.id), encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn agent_messages_for_bot(&self, bot_id: u64) -> Result<Vec<AgentMessage>, Error> {
        let read_txn = self.db.begin_read()?;
        let msgs = read_txn.open_table(AGENT_MESSAGES_TABLE)?;
        let mut out = Vec::new();
        for item in msgs.range((bot_id, 0u64)..=(bot_id, u64::MAX))? {
            let (_, v) = item?;
            out.push(decode_json(v.value())?);
        }
        Ok(out)
    }

    pub fn clear_agent_messages(&self, bot_id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut msgs = write_txn.open_table(AGENT_MESSAGES_TABLE)?;
            let keys: Vec<(u64, u64)> = msgs
                .range((bot_id, 0u64)..=(bot_id, u64::MAX))?
                .filter_map(|r| r.ok())
                .map(|(k, _)| k.value())
                .collect();
            for k in keys {
                msgs.remove(k)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn upsert_agent_proposal(&self, prop: &AgentProposal) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut props = write_txn.open_table(AGENT_PROPOSALS_TABLE)?;
            let encoded = encode_json(prop)?;
            props.insert(prop.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_agent_proposal(&self, id: u64) -> Result<Option<AgentProposal>, Error> {
        let read_txn = self.db.begin_read()?;
        let props = read_txn.open_table(AGENT_PROPOSALS_TABLE)?;
        props
            .get(id)?
            .map(|raw| decode_json(raw.value()))
            .transpose()
    }

    pub fn agent_proposals_for_bot(&self, bot_id: u64) -> Result<Vec<AgentProposal>, Error> {
        let read_txn = self.db.begin_read()?;
        let props = read_txn.open_table(AGENT_PROPOSALS_TABLE)?;
        let mut out: Vec<AgentProposal> = props
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode_json::<AgentProposal>(v.value()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|p| p.bot_id == bot_id)
            .collect();
        out.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(out)
    }

    pub fn pending_agent_proposals_count(&self) -> Result<usize, Error> {
        let read_txn = self.db.begin_read()?;
        let props = read_txn.open_table(AGENT_PROPOSALS_TABLE)?;
        let mut count = 0usize;
        for item in props.iter()? {
            let (_, v) = item?;
            if let Ok(p) = decode_json::<AgentProposal>(v.value()) {
                if p.status == ProposalStatus::Pending {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    // --- PODCASTS ---

    fn podcast_chrono_key(published_at: u64, id: u64) -> String {
        format!("{:020}_{}", published_at, id)
    }

    /// Insert or update an episode. Maintains slug uniqueness and the
    /// Published-only chrono index in a single transaction.
    pub fn upsert_podcast_episode(&self, ep: &PodcastEpisode) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main = write_txn.open_table(PODCASTS_TABLE)?;
            let mut chrono = write_txn.open_table(PODCASTS_CHRONO_INDEX)?;
            let mut slug = write_txn.open_table(PODCASTS_SLUG_INDEX)?;

            if let Some(existing) = slug.get(ep.slug.as_str())? {
                if existing.value() != ep.id {
                    return Err(Error::Corrupted(format!(
                        "podcast slug '{}' already used by episode {}",
                        ep.slug,
                        existing.value()
                    )));
                }
            }
            if let Some(prev_raw) = main.get(ep.id)? {
                let prev: PodcastEpisode = decode(prev_raw.value())?;
                if prev.slug != ep.slug {
                    slug.remove(prev.slug.as_str())?;
                }
                if let Some(ts) = prev.published_at {
                    chrono.remove(Self::podcast_chrono_key(ts, prev.id).as_str())?;
                }
            }

            let bytes = encode(ep)?;
            main.insert(ep.id, bytes.as_slice())?;
            slug.insert(ep.slug.as_str(), ep.id)?;
            if ep.status == EpisodeStatus::Published {
                if let Some(ts) = ep.published_at {
                    chrono.insert(Self::podcast_chrono_key(ts, ep.id).as_str(), ep.id)?;
                }
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn delete_podcast_episode(&self, id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main = write_txn.open_table(PODCASTS_TABLE)?;
            let mut chrono = write_txn.open_table(PODCASTS_CHRONO_INDEX)?;
            let mut slug = write_txn.open_table(PODCASTS_SLUG_INDEX)?;
            if let Some(prev_raw) = main.get(id)? {
                let prev: PodcastEpisode = decode(prev_raw.value())?;
                slug.remove(prev.slug.as_str())?;
                if let Some(ts) = prev.published_at {
                    chrono.remove(Self::podcast_chrono_key(ts, prev.id).as_str())?;
                }
            }
            main.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Most-recent published episodes for the public RSS feed.
    pub fn latest_published_podcast_episodes(
        &self,
        limit: usize,
    ) -> Result<Vec<PodcastEpisode>, Error> {
        let read_txn = self.db.begin_read()?;
        let chrono = read_txn.open_table(PODCASTS_CHRONO_INDEX)?;
        let main = read_txn.open_table(PODCASTS_TABLE)?;
        let mut out = Vec::new();
        for item in chrono.range("0"..="9")?.rev().take(limit) {
            let (_, id) = item?;
            if let Some(raw) = main.get(id.value())? {
                out.push(decode(raw.value())?);
            }
        }
        Ok(out)
    }

    /// All episodes (drafts + published) — used by the admin list.
    pub fn all_podcast_episodes(&self) -> Result<Vec<PodcastEpisode>, Error> {
        let read_txn = self.db.begin_read()?;
        let main = read_txn.open_table(PODCASTS_TABLE)?;
        let mut out: Vec<PodcastEpisode> = main
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    pub fn get_podcast_episode(&self, id: u64) -> Result<Option<PodcastEpisode>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PODCASTS_TABLE)?;
        table.get(id)?.map(|raw| decode(raw.value())).transpose()
    }

    pub fn get_podcast_episode_by_slug(&self, slug: &str) -> Result<Option<PodcastEpisode>, Error> {
        let read_txn = self.db.begin_read()?;
        let slug_table = read_txn.open_table(PODCASTS_SLUG_INDEX)?;
        let main = read_txn.open_table(PODCASTS_TABLE)?;
        let id = match slug_table.get(slug)? {
            Some(v) => v.value(),
            None => return Ok(None),
        };
        main.get(id)?.map(|raw| decode(raw.value())).transpose()
    }

    // --- VIDEO PODCASTS ---

    fn video_chrono_key(published_at: u64, id: u64) -> String {
        format!("{:020}_{}", published_at, id)
    }

    pub fn upsert_video_podcast(&self, video: &VideoPodcast) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main = write_txn.open_table(VIDEO_PODCASTS_TABLE)?;
            let mut chrono = write_txn.open_table(VIDEO_PODCASTS_CHRONO_INDEX)?;
            let mut slug = write_txn.open_table(VIDEO_PODCASTS_SLUG_INDEX)?;

            if let Some(existing) = slug.get(video.slug.as_str())? {
                if existing.value() != video.id {
                    return Err(Error::Corrupted(format!(
                        "video slug '{}' already used by id {}",
                        video.slug,
                        existing.value()
                    )));
                }
            }
            if let Some(prev_raw) = main.get(video.id)? {
                let prev: VideoPodcast = decode(prev_raw.value())?;
                if prev.slug != video.slug {
                    slug.remove(prev.slug.as_str())?;
                }
                if let Some(ts) = prev.published_at {
                    chrono.remove(Self::video_chrono_key(ts, prev.id).as_str())?;
                }
            }

            let bytes = encode(video)?;
            main.insert(video.id, bytes.as_slice())?;
            slug.insert(video.slug.as_str(), video.id)?;
            if video.status == VideoStatus::Published {
                if let Some(ts) = video.published_at {
                    chrono.insert(Self::video_chrono_key(ts, video.id).as_str(), video.id)?;
                }
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn delete_video_podcast(&self, id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main = write_txn.open_table(VIDEO_PODCASTS_TABLE)?;
            let mut chrono = write_txn.open_table(VIDEO_PODCASTS_CHRONO_INDEX)?;
            let mut slug = write_txn.open_table(VIDEO_PODCASTS_SLUG_INDEX)?;
            if let Some(prev_raw) = main.get(id)? {
                let prev: VideoPodcast = decode(prev_raw.value())?;
                slug.remove(prev.slug.as_str())?;
                if let Some(ts) = prev.published_at {
                    chrono.remove(Self::video_chrono_key(ts, prev.id).as_str())?;
                }
            }
            main.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_video_podcast(&self, id: u64) -> Result<Option<VideoPodcast>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(VIDEO_PODCASTS_TABLE)?;
        table.get(id)?.map(|raw| decode(raw.value())).transpose()
    }

    pub fn get_video_podcast_by_slug(&self, slug: &str) -> Result<Option<VideoPodcast>, Error> {
        let read_txn = self.db.begin_read()?;
        let slug_table = read_txn.open_table(VIDEO_PODCASTS_SLUG_INDEX)?;
        let main = read_txn.open_table(VIDEO_PODCASTS_TABLE)?;
        let id = match slug_table.get(slug)? {
            Some(v) => v.value(),
            None => return Ok(None),
        };
        main.get(id)?.map(|raw| decode(raw.value())).transpose()
    }

    pub fn latest_published_video_podcasts(
        &self,
        limit: usize,
    ) -> Result<Vec<VideoPodcast>, Error> {
        let read_txn = self.db.begin_read()?;
        let chrono = read_txn.open_table(VIDEO_PODCASTS_CHRONO_INDEX)?;
        let main = read_txn.open_table(VIDEO_PODCASTS_TABLE)?;
        let mut out = Vec::new();
        for item in chrono.range("0"..="9")?.rev().take(limit) {
            let (_, id) = item?;
            if let Some(raw) = main.get(id.value())? {
                out.push(decode(raw.value())?);
            }
        }
        Ok(out)
    }

    pub fn all_video_podcasts(&self) -> Result<Vec<VideoPodcast>, Error> {
        let read_txn = self.db.begin_read()?;
        let main = read_txn.open_table(VIDEO_PODCASTS_TABLE)?;
        let mut out: Vec<VideoPodcast> = main
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    pub fn videos_pending_transcode(&self) -> Result<Vec<VideoPodcast>, Error> {
        Ok(self
            .all_video_podcasts()?
            .into_iter()
            .filter(|v| v.status == VideoStatus::Transcoding && v.error.is_none())
            .collect())
    }

    // --- TRACKS ---

    pub fn upsert_track(&self, track: &Track) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TRACKS_TABLE)?;
            let bytes = encode(track)?;
            table.insert(track.id, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_track(&self, id: u64) -> Result<Option<Track>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TRACKS_TABLE)?;
        table.get(id)?.map(|raw| decode(raw.value())).transpose()
    }

    pub fn delete_track(&self, id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TRACKS_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn all_tracks(&self) -> Result<Vec<Track>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TRACKS_TABLE)?;
        let mut out: Vec<Track> = table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    // --- TRANSCRIPTION JOBS ---

    pub fn upsert_transcription_job(&self, job: &TranscriptionJob) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TRANSCRIPTION_JOBS_TABLE)?;
            let bytes = encode(job)?;
            table.insert(job.id, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_transcription_job(&self, id: u64) -> Result<Option<TranscriptionJob>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TRANSCRIPTION_JOBS_TABLE)?;
        table.get(id)?.map(|raw| decode(raw.value())).transpose()
    }

    pub fn delete_transcription_job(&self, id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TRANSCRIPTION_JOBS_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// All jobs sorted newest-first by `updated_at`.
    pub fn all_transcription_jobs(&self) -> Result<Vec<TranscriptionJob>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TRANSCRIPTION_JOBS_TABLE)?;

        let mut out: Vec<TranscriptionJob> = table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    /// All jobs in `Queued` status — used by the Whisper worker to find work.
    pub fn queued_transcription_jobs(&self) -> Result<Vec<TranscriptionJob>, Error> {
        Ok(self
            .all_transcription_jobs()?
            .into_iter()
            .filter(|j| j.status == TranscriptionStatus::Queued)
            .collect())
    }

    // --- NEWSLETTER SUBSCRIBERS ---

    /// Insert or update a subscriber. Email must be unique across rows.
    pub fn upsert_subscriber(&self, sub: &Subscriber) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main = write_txn.open_table(NEWSLETTER_SUBSCRIBERS_TABLE)?;
            let mut idx = write_txn.open_table(NEWSLETTER_EMAIL_INDEX)?;

            if let Some(existing) = idx.get(sub.email.as_str())? {
                if existing.value() != sub.id {
                    return Err(Error::Corrupted(format!(
                        "email '{}' already used by subscriber {}",
                        sub.email,
                        existing.value()
                    )));
                }
            }
            // Drop stale email-index entry if email changed.
            if let Some(prev_raw) = main.get(sub.id)? {
                let prev: Subscriber = decode(prev_raw.value())?;
                if prev.email != sub.email {
                    idx.remove(prev.email.as_str())?;
                }
            }
            let bytes = encode(sub)?;
            main.insert(sub.id, bytes.as_slice())?;
            idx.insert(sub.email.as_str(), sub.id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn delete_subscriber(&self, id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main = write_txn.open_table(NEWSLETTER_SUBSCRIBERS_TABLE)?;
            let mut idx = write_txn.open_table(NEWSLETTER_EMAIL_INDEX)?;
            if let Some(prev_raw) = main.get(id)? {
                let prev: Subscriber = decode(prev_raw.value())?;
                idx.remove(prev.email.as_str())?;
            }
            main.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_subscriber_by_email(&self, email: &str) -> Result<Option<Subscriber>, Error> {
        let read_txn = self.db.begin_read()?;
        let idx = read_txn.open_table(NEWSLETTER_EMAIL_INDEX)?;
        let main = read_txn.open_table(NEWSLETTER_SUBSCRIBERS_TABLE)?;
        let id = match idx.get(email)? {
            Some(v) => v.value(),
            None => return Ok(None),
        };
        main.get(id)?.map(|raw| decode(raw.value())).transpose()
    }

    pub fn all_subscribers(&self) -> Result<Vec<Subscriber>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(NEWSLETTER_SUBSCRIBERS_TABLE)?;
        let mut out: Vec<Subscriber> = table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    // --- ADS ---

    pub fn upsert_ad(&self, ad: &Ad) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ADS_TABLE)?;
            let bytes = encode(ad)?;
            table.insert(ad.id, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_ad(&self, id: u64) -> Result<Option<Ad>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ADS_TABLE)?;
        table.get(id)?.map(|raw| decode(raw.value())).transpose()
    }

    pub fn delete_ad(&self, id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ADS_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn all_ads(&self) -> Result<Vec<Ad>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ADS_TABLE)?;
        let mut out: Vec<Ad> = table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    // --- FEDERATION FOLLOWERS ---

    pub fn upsert_follower(&self, follower: &FederationFollower) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FEDERATION_FOLLOWERS_TABLE)?;
            let bytes = encode(follower)?;
            table.insert(follower.actor_url.as_str(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn delete_follower(&self, actor_url: &str) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FEDERATION_FOLLOWERS_TABLE)?;
            table.remove(actor_url)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn all_followers(&self) -> Result<Vec<FederationFollower>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FEDERATION_FOLLOWERS_TABLE)?;
        let mut out: Vec<FederationFollower> = table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| b.followed_at.cmp(&a.followed_at));
        Ok(out)
    }

    // --- RADIO PLAYLISTS ---

    pub fn upsert_playlist(&self, p: &Playlist) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(PLAYLISTS_TABLE)?;
            let bytes = encode(p)?;
            table.insert(p.id, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_playlist(&self, id: u64) -> Result<Option<Playlist>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PLAYLISTS_TABLE)?;
        table.get(id)?.map(|raw| decode(raw.value())).transpose()
    }

    /// Drops the playlist row AND every PlaylistTracks entry for it.
    pub fn delete_playlist(&self, id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main = write_txn.open_table(PLAYLISTS_TABLE)?;
            main.remove(id)?;
            let mut tracks = write_txn.open_table(PLAYLIST_TRACKS_TABLE)?;
            // Drop every entry whose key starts with this playlist_id.
            tracks.retain(|key, _value| {
                let (pid, _) = key;
                pid != id
            })?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn all_playlists(&self) -> Result<Vec<Playlist>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PLAYLISTS_TABLE)?;
        let mut out: Vec<Playlist> = table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| decode(v.value()))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(out)
    }

    /// Append a track at the end of the playlist.
    pub fn append_to_playlist(&self, playlist_id: u64, track_id: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut tracks = write_txn.open_table(PLAYLIST_TRACKS_TABLE)?;
            // Find the next free position by scanning the playlist's range.
            let mut next_pos: u64 = 0;
            for r in tracks.range((playlist_id, 0)..(playlist_id, u64::MAX))? {
                let (key_guard, _) = r?;
                let (_, pos) = key_guard.value();
                next_pos = next_pos.max(pos + 1);
            }
            tracks.insert((playlist_id, next_pos), track_id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Remove the entry at (playlist_id, position).
    pub fn remove_from_playlist(&self, playlist_id: u64, position: u64) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut tracks = write_txn.open_table(PLAYLIST_TRACKS_TABLE)?;
            tracks.remove((playlist_id, position))?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Return (position, track_id) pairs in playlist order.
    pub fn playlist_track_ids(&self, playlist_id: u64) -> Result<Vec<(u64, u64)>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PLAYLIST_TRACKS_TABLE)?;
        let mut out = Vec::new();
        for r in table.range((playlist_id, 0)..(playlist_id, u64::MAX))? {
            let (key_guard, val_guard) = r?;
            let (_, pos) = key_guard.value();
            out.push((pos, val_guard.value()));
        }
        Ok(out)
    }

    /// Resolve a playlist's tracks to actual `Track` objects, preserving order.
    pub fn playlist_tracks(&self, playlist_id: u64) -> Result<Vec<Track>, Error> {
        let entries = self.playlist_track_ids(playlist_id)?;
        let mut out = Vec::with_capacity(entries.len());
        for (_pos, tid) in entries {
            if let Some(t) = self.get_track(tid)? {
                out.push(t);
            }
        }
        Ok(out)
    }

    // --- LISTENER STATS ---

    fn minute_key(ts: u64) -> u64 {
        ts - (ts % 60)
    }

    /// Record a concurrent-listener sample. `ts` is rounded down to the
    /// minute and the row for that minute is upserted to
    /// `max(existing, listeners)` — each row is the minute's peak, so
    /// callers can sample as often as they like without inflating storage.
    pub fn record_listener_sample(&self, ts: u64, listeners: u32) -> Result<(), Error> {
        let minute = Self::minute_key(ts);
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(LISTENER_STATS_TABLE)?;
            let current = table.get(minute)?.map(|v| v.value()).unwrap_or(0);
            table.insert(minute, current.max(listeners))?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Ascending `(minute_ts, peak_listeners)` samples whose minute key
    /// falls in `[from_ts, to_ts]` (inclusive). Keys are minute-aligned,
    /// so round `from_ts` down if you need the minute a mid-minute
    /// timestamp falls inside.
    pub fn listener_samples(&self, from_ts: u64, to_ts: u64) -> Result<Vec<(u64, u32)>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(LISTENER_STATS_TABLE)?;
        let mut out = Vec::new();
        for r in table.range(from_ts..=to_ts)? {
            let (k, v) = r?;
            out.push((k.value(), v.value()));
        }
        Ok(out)
    }

    /// Delete every sample whose minute key is strictly older than
    /// `older_than_ts`. Returns how many rows were removed. Run this
    /// periodically to keep the stats table bounded.
    pub fn prune_listener_samples(&self, older_than_ts: u64) -> Result<usize, Error> {
        let write_txn = self.db.begin_write()?;
        let removed = {
            let mut table = write_txn.open_table(LISTENER_STATS_TABLE)?;
            let mut removed = 0usize;
            for entry in table.extract_from_if(..older_than_ts, |_, _| true)? {
                entry?;
                removed += 1;
            }
            removed
        };
        write_txn.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ccc-database-test-{label}-{}-{nanos}.redb",
            std::process::id()
        ));
        p
    }

    fn cleanup(path: &PathBuf) {
        let _ = std::fs::remove_file(path);
    }

    // ---- counter / next_id ----

    #[test]
    fn next_id_starts_at_one_and_increments() {
        let path = tmp_path("next_id");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        assert_eq!(db.next_id("a").unwrap(), 1);
        assert_eq!(db.next_id("a").unwrap(), 2);
        assert_eq!(db.next_id("a").unwrap(), 3);
        cleanup(&path);
    }

    #[test]
    fn next_id_namespaces_are_independent() {
        let path = tmp_path("next_id_ns");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        assert_eq!(db.next_id("podcast_episode").unwrap(), 1);
        assert_eq!(db.next_id("podcast_episode").unwrap(), 2);
        assert_eq!(db.next_id("transcription_job").unwrap(), 1);
        assert_eq!(db.next_id("video_podcast").unwrap(), 1);
        assert_eq!(db.next_id("podcast_episode").unwrap(), 3);
        cleanup(&path);
    }

    #[test]
    fn next_id_survives_reopen() {
        let path = tmp_path("next_id_reopen");
        {
            let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
            assert_eq!(db.next_id("k").unwrap(), 1);
            assert_eq!(db.next_id("k").unwrap(), 2);
        }
        // Reopen: counter must keep growing, not reset to 1.
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        assert_eq!(db.next_id("k").unwrap(), 3);
        cleanup(&path);
    }

    // ---- blog post round-trip ----

    fn sample_post(id: u64, slug: &str) -> BlogPost {
        BlogPost {
            id,
            slug: slug.into(),
            title: format!("Title {id}"),
            content: "# hello".into(),
            status: PostStatus::Published,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
            published_at: Some(1_700_000_000),
        }
    }

    #[test]
    fn blog_post_round_trip() {
        let path = tmp_path("blog_roundtrip");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        let post = sample_post(42, "hello-world");
        db.upsert_blog_post(&post).unwrap();
        let loaded = db.get_blog_post(42).unwrap().expect("post exists");
        assert_eq!(loaded.slug, "hello-world");
        assert_eq!(loaded.title, "Title 42");
        assert_eq!(loaded.content, "# hello");
        cleanup(&path);
    }

    #[test]
    fn blog_post_slug_uniqueness_enforced() {
        let path = tmp_path("blog_slug");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        db.upsert_blog_post(&sample_post(1, "shared")).unwrap();
        // Different id, same slug -> must error.
        let err = db.upsert_blog_post(&sample_post(2, "shared")).unwrap_err();
        assert!(matches!(err, Error::Corrupted(_)), "got: {err:?}");
        cleanup(&path);
    }

    // ---- corrupt row no longer panics ----
    //
    // The whole reason we swapped `.unwrap()` for `decode()`/`encode()`
    // was to convert panic on bad data into a clean `Error::Corrupted`.
    // We test the inverse: if we write garbage into a value cell,
    // the read path returns an error instead of panicking.

    #[test]
    fn corrupt_blog_post_row_returns_error_not_panic() {
        let path = tmp_path("blog_corrupt");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        // Open a write txn, write a garbage blob at id=99, commit.
        {
            let write_txn = db.db.begin_write().unwrap();
            {
                let mut t = write_txn.open_table(BLOG_POSTS_TABLE).unwrap();
                t.insert(99u64, b"this is not a bincode BlogPost".as_slice())
                    .unwrap();
            }
            write_txn.commit().unwrap();
        }
        // Reading it must surface a `Corrupted` error, not panic.
        let err = db.get_blog_post(99).unwrap_err();
        assert!(matches!(err, Error::Corrupted(_)), "got: {err:?}");
        cleanup(&path);
    }

    // ---- podcast chrono index is publish-only ----

    #[test]
    fn chrono_index_excludes_drafts() {
        let path = tmp_path("podcast_chrono");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        let draft = PodcastEpisode {
            id: 1,
            slug: "draft".into(),
            title: "D".into(),
            description: String::new(),
            audio_path: "/tmp/x.mp3".into(),
            mime_type: "audio/mpeg".into(),
            size_bytes: 0,
            duration_seconds: 0,
            status: EpisodeStatus::Draft,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
            published_at: None,
            transcript_job_id: None,
            members_only: false,
        };
        let published = PodcastEpisode {
            id: 2,
            status: EpisodeStatus::Published,
            published_at: Some(1_700_000_000),
            slug: "pub".into(),
            title: "P".into(),
            ..draft.clone()
        };
        db.upsert_podcast_episode(&draft).unwrap();
        db.upsert_podcast_episode(&published).unwrap();
        let published_list = db.all_podcast_episodes().unwrap();
        assert_eq!(published_list.len(), 2);
        // The public chrono getter only returns Published.
        let public = db.latest_published_podcast_episodes(10).unwrap();
        assert_eq!(public.len(), 1, "drafts must not be in the chrono index");
        assert_eq!(public[0].id, 2);
        cleanup(&path);
    }

    // ---- listener stats ----

    #[test]
    fn listener_sample_keys_round_down_to_minute() {
        let path = tmp_path("listener_round_down");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        // 120, 130, and 179 all fall inside the minute starting at 120.
        db.record_listener_sample(120, 1).unwrap();
        db.record_listener_sample(130, 2).unwrap();
        db.record_listener_sample(179, 3).unwrap();
        let samples = db.listener_samples(0, 1_000).unwrap();
        assert_eq!(samples, vec![(120, 3)], "all three samples share one key");
        cleanup(&path);
    }

    #[test]
    fn listener_sample_upsert_keeps_max() {
        let path = tmp_path("listener_max");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        db.record_listener_sample(60, 5).unwrap();
        // A lower sample in the same minute must not clobber the peak.
        db.record_listener_sample(61, 3).unwrap();
        assert_eq!(db.listener_samples(60, 60).unwrap(), vec![(60, 5)]);
        // A higher sample raises it.
        db.record_listener_sample(119, 9).unwrap();
        assert_eq!(db.listener_samples(60, 60).unwrap(), vec![(60, 9)]);
        cleanup(&path);
    }

    #[test]
    fn listener_samples_range_is_ascending_and_inclusive() {
        let path = tmp_path("listener_range");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        // Insert out of chronological order — the u64 key ordering
        // must hand them back ascending regardless.
        db.record_listener_sample(240, 4).unwrap();
        db.record_listener_sample(60, 1).unwrap();
        db.record_listener_sample(180, 3).unwrap();
        db.record_listener_sample(120, 2).unwrap();
        // Both bounds are inclusive; 60 and 240 sit outside.
        let samples = db.listener_samples(120, 180).unwrap();
        assert_eq!(samples, vec![(120, 2), (180, 3)]);
        // Full window returns everything, ascending.
        let all = db.listener_samples(0, 10_000).unwrap();
        assert_eq!(all, vec![(60, 1), (120, 2), (180, 3), (240, 4)]);
        cleanup(&path);
    }

    #[test]
    fn prune_listener_samples_removes_older_and_counts() {
        let path = tmp_path("listener_prune");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();
        db.record_listener_sample(60, 1).unwrap();
        db.record_listener_sample(120, 2).unwrap();
        db.record_listener_sample(180, 3).unwrap();
        // Strictly-older-than: the 180 row survives a cutoff of 180.
        let removed = db.prune_listener_samples(180).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(db.listener_samples(0, 10_000).unwrap(), vec![(180, 3)]);
        // Nothing left to prune below the cutoff.
        assert_eq!(db.prune_listener_samples(180).unwrap(), 0);
        cleanup(&path);
    }

    #[test]
    fn multisite_blog_host_lookup_and_reusable_slugs() {
        let path = tmp_path("multisite_blog");
        let db = CccDB::new(path.to_string_lossy().as_ref()).unwrap();

        let site_a = BlogSite {
            id: 1,
            name: "Field Notes".into(),
            slug: "field-notes".into(),
            hosts: vec!["HTTPS://Notes.Example.COM:443/".into(), "fieldnotes.example.org".into()],
            description: "Observations.".into(),
            theme: "signal".into(),
            created_at: 100,
            updated_at: 100,
        };
        let site_b = BlogSite {
            id: 2,
            name: "Studio Journal".into(),
            slug: "studio-journal".into(),
            hosts: vec!["studio.example.com".into()],
            description: "Workshop log.".into(),
            theme: "phosphor".into(),
            created_at: 101,
            updated_at: 101,
        };
        db.upsert_blog_site(&site_a).unwrap();
        db.upsert_blog_site(&site_b).unwrap();

        // Host normalization & lookup
        let found = db.get_blog_site_by_host("notes.example.com:8080").unwrap().unwrap();
        assert_eq!(found.id, 1);
        assert_eq!(db.get_blog_site_by_host("fieldnotes.example.org").unwrap().unwrap().id, 1);
        assert_eq!(db.get_blog_site_by_host("studio.example.com").unwrap().unwrap().id, 2);

        // Duplicate host on another site must fail
        let conflict = BlogSite {
            id: 3,
            name: "Conflict".into(),
            slug: "conflict".into(),
            hosts: vec!["notes.example.com".into()],
            description: String::new(),
            theme: "ink".into(),
            created_at: 102,
            updated_at: 102,
        };
        assert!(db.upsert_blog_site(&conflict).is_err());

        // Same post slug on two different sites succeeds!
        db.upsert_blog_post_for_site(1, &sample_post(10, "welcome")).unwrap();
        db.upsert_blog_post_for_site(2, &sample_post(11, "welcome")).unwrap();
        assert_eq!(db.get_blog_post_by_site_and_slug(1, "welcome").unwrap().unwrap().id, 10);
        assert_eq!(db.get_blog_post_by_site_and_slug(2, "welcome").unwrap().unwrap().id, 11);

        // Duplicate slug within the same site fails!
        assert!(db.upsert_blog_post_for_site(1, &sample_post(12, "welcome")).is_err());

        // Per-site listing isolates posts
        assert_eq!(db.latest_published_blog_posts_for_site(1, 10).unwrap().len(), 1);
        assert_eq!(db.latest_published_blog_posts_for_site(2, 10).unwrap().len(), 1);
        assert_eq!(db.latest_published_blog_posts_for_site(0, 10).unwrap().len(), 0);

        // Deleting site_a removes its posts and frees its hosts
        db.delete_blog_site(1).unwrap();
        assert!(db.get_blog_site_by_host("notes.example.com").unwrap().is_none());
        assert!(db.get_blog_post(10).unwrap().is_none());
        assert!(db.get_blog_post(11).unwrap().is_some());

        cleanup(&path);
    }
}
