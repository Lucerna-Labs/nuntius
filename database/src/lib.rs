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

/// Slug → ID lookup. Slugs must be unique across all posts (drafts included).
const BLOG_POSTS_SLUG_INDEX: TableDefinition<&str, u64> = TableDefinition::new("blog_posts_slug");

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

/// The main Database manager for Creator Command Center (CCC)
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
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

    // --- BLOG POSTS ---

    fn chrono_key(published_at: u64, id: u64) -> String {
        format!("{:020}_{}", published_at, id)
    }

    /// Insert or update a blog post. Maintains slug uniqueness (rejects
    /// inserts whose slug is taken by another id) and keeps the chrono
    /// index synchronized with `status`: Published posts are indexed,
    /// Drafts are not.
    pub fn upsert_blog_post(&self, post: &BlogPost) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main_table = write_txn.open_table(BLOG_POSTS_TABLE)?;
            let mut chrono_table = write_txn.open_table(BLOG_POSTS_CHRONO_INDEX)?;
            let mut slug_table = write_txn.open_table(BLOG_POSTS_SLUG_INDEX)?;

            // Slug uniqueness: if some other id already owns this slug, refuse.
            if let Some(existing_id) = slug_table.get(post.slug.as_str())? {
                if existing_id.value() != post.id {
                    return Err(Error::Corrupted(format!(
                        "blog slug '{}' already in use by post {}",
                        post.slug,
                        existing_id.value()
                    )));
                }
            }

            // Drop any stale row+indexes for this id (handles slug renames
            // and status flips cleanly).
            if let Some(prev_raw) = main_table.get(post.id)? {
                let prev: BlogPost = bincode::deserialize(prev_raw.value()).unwrap();
                if prev.slug != post.slug {
                    slug_table.remove(prev.slug.as_str())?;
                }
                if let Some(ts) = prev.published_at {
                    chrono_table.remove(Self::chrono_key(ts, prev.id).as_str())?;
                }
            }

            let encoded: Vec<u8> = bincode::serialize(post).unwrap();
            main_table.insert(post.id, encoded.as_slice())?;
            slug_table.insert(post.slug.as_str(), post.id)?;

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

            if let Some(prev_raw) = main_table.get(id)? {
                let prev: BlogPost = bincode::deserialize(prev_raw.value()).unwrap();
                slug_table.remove(prev.slug.as_str())?;
                if let Some(ts) = prev.published_at {
                    chrono_table.remove(Self::chrono_key(ts, prev.id).as_str())?;
                }
            }
            main_table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Single post by id. Returns None if not found.
    pub fn get_blog_post(&self, id: u64) -> Result<Option<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(BLOG_POSTS_TABLE)?;
        Ok(table
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
    }

    /// Single post by slug.
    pub fn get_blog_post_by_slug(&self, slug: &str) -> Result<Option<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let slug_table = read_txn.open_table(BLOG_POSTS_SLUG_INDEX)?;
        let main_table = read_txn.open_table(BLOG_POSTS_TABLE)?;

        let id = match slug_table.get(slug)? {
            Some(v) => v.value(),
            None => return Ok(None),
        };
        Ok(main_table
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
    }

    /// Most-recent Published posts for the public reader.
    pub fn latest_published_blog_posts(&self, limit: usize) -> Result<Vec<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let chrono_table = read_txn.open_table(BLOG_POSTS_CHRONO_INDEX)?;
        let main_table = read_txn.open_table(BLOG_POSTS_TABLE)?;

        let mut out = Vec::new();
        for item in chrono_table.range("0"..="9")?.rev().take(limit) {
            let (_, id) = item?;
            if let Some(raw) = main_table.get(id.value())? {
                out.push(bincode::deserialize(raw.value()).unwrap());
            }
        }
        Ok(out)
    }

    /// Every post (drafts + published), newest-row first by id.
    /// Used by the admin list view.
    pub fn all_blog_posts(&self) -> Result<Vec<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let main_table = read_txn.open_table(BLOG_POSTS_TABLE)?;

        let mut out: Vec<BlogPost> = main_table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| bincode::deserialize(v.value()).unwrap())
            .collect();
        // Newest first by updated_at.
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
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
                let prev: PodcastEpisode = bincode::deserialize(prev_raw.value()).unwrap();
                if prev.slug != ep.slug {
                    slug.remove(prev.slug.as_str())?;
                }
                if let Some(ts) = prev.published_at {
                    chrono.remove(Self::podcast_chrono_key(ts, prev.id).as_str())?;
                }
            }

            let bytes = bincode::serialize(ep).unwrap();
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
                let prev: PodcastEpisode = bincode::deserialize(prev_raw.value()).unwrap();
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
                out.push(bincode::deserialize(raw.value()).unwrap());
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
            .map(|(_, v)| bincode::deserialize(v.value()).unwrap())
            .collect();
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    pub fn get_podcast_episode(&self, id: u64) -> Result<Option<PodcastEpisode>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PODCASTS_TABLE)?;
        Ok(table
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
    }

    pub fn get_podcast_episode_by_slug(&self, slug: &str) -> Result<Option<PodcastEpisode>, Error> {
        let read_txn = self.db.begin_read()?;
        let slug_table = read_txn.open_table(PODCASTS_SLUG_INDEX)?;
        let main = read_txn.open_table(PODCASTS_TABLE)?;
        let id = match slug_table.get(slug)? {
            Some(v) => v.value(),
            None => return Ok(None),
        };
        Ok(main
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
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
                let prev: VideoPodcast = bincode::deserialize(prev_raw.value()).unwrap();
                if prev.slug != video.slug {
                    slug.remove(prev.slug.as_str())?;
                }
                if let Some(ts) = prev.published_at {
                    chrono.remove(Self::video_chrono_key(ts, prev.id).as_str())?;
                }
            }

            let bytes = bincode::serialize(video).unwrap();
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
                let prev: VideoPodcast = bincode::deserialize(prev_raw.value()).unwrap();
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
        Ok(table
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
    }

    pub fn get_video_podcast_by_slug(&self, slug: &str) -> Result<Option<VideoPodcast>, Error> {
        let read_txn = self.db.begin_read()?;
        let slug_table = read_txn.open_table(VIDEO_PODCASTS_SLUG_INDEX)?;
        let main = read_txn.open_table(VIDEO_PODCASTS_TABLE)?;
        let id = match slug_table.get(slug)? {
            Some(v) => v.value(),
            None => return Ok(None),
        };
        Ok(main
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
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
                out.push(bincode::deserialize(raw.value()).unwrap());
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
            .map(|(_, v)| bincode::deserialize(v.value()).unwrap())
            .collect();
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
            let bytes = bincode::serialize(track).unwrap();
            table.insert(track.id, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_track(&self, id: u64) -> Result<Option<Track>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TRACKS_TABLE)?;
        Ok(table
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
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
            .map(|(_, v)| bincode::deserialize(v.value()).unwrap())
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    // --- TRANSCRIPTION JOBS ---

    pub fn upsert_transcription_job(&self, job: &TranscriptionJob) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TRANSCRIPTION_JOBS_TABLE)?;
            let bytes = bincode::serialize(job).unwrap();
            table.insert(job.id, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_transcription_job(&self, id: u64) -> Result<Option<TranscriptionJob>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TRANSCRIPTION_JOBS_TABLE)?;
        Ok(table
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
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
            .map(|(_, v)| bincode::deserialize(v.value()).unwrap())
            .collect();
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
                let prev: Subscriber = bincode::deserialize(prev_raw.value()).unwrap();
                if prev.email != sub.email {
                    idx.remove(prev.email.as_str())?;
                }
            }
            let bytes = bincode::serialize(sub).unwrap();
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
                let prev: Subscriber = bincode::deserialize(prev_raw.value()).unwrap();
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
        Ok(main
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
    }

    pub fn all_subscribers(&self) -> Result<Vec<Subscriber>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(NEWSLETTER_SUBSCRIBERS_TABLE)?;
        let mut out: Vec<Subscriber> = table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(_, v)| bincode::deserialize(v.value()).unwrap())
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    // --- ADS ---

    pub fn upsert_ad(&self, ad: &Ad) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ADS_TABLE)?;
            let bytes = bincode::serialize(ad).unwrap();
            table.insert(ad.id, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_ad(&self, id: u64) -> Result<Option<Ad>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ADS_TABLE)?;
        Ok(table
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
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
            .map(|(_, v)| bincode::deserialize(v.value()).unwrap())
            .collect();
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    // --- FEDERATION FOLLOWERS ---

    pub fn upsert_follower(&self, follower: &FederationFollower) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FEDERATION_FOLLOWERS_TABLE)?;
            let bytes = bincode::serialize(follower).unwrap();
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
            .map(|(_, v)| bincode::deserialize(v.value()).unwrap())
            .collect();
        out.sort_by(|a, b| b.followed_at.cmp(&a.followed_at));
        Ok(out)
    }

    // --- RADIO PLAYLISTS ---

    pub fn upsert_playlist(&self, p: &Playlist) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(PLAYLISTS_TABLE)?;
            let bytes = bincode::serialize(p).unwrap();
            table.insert(p.id, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_playlist(&self, id: u64) -> Result<Option<Playlist>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PLAYLISTS_TABLE)?;
        Ok(table
            .get(id)?
            .map(|raw| bincode::deserialize(raw.value()).unwrap()))
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
            .map(|(_, v)| bincode::deserialize(v.value()).unwrap())
            .collect();
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
}
