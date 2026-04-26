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
const BLOG_POSTS_CHRONO_INDEX: TableDefinition<&str, u64> = TableDefinition::new("blog_posts_chrono");

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

/// The main table where tracks are stored.
/// Key: Track ID (u64)
/// Value: Serialized Track
const TRACKS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("tracks");

/// The main table where podcast episodes are stored.
/// Key: Episode ID (u64)
/// Value: Serialized PodcastEpisode
const PODCASTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("podcasts");

/// The custom index table for quickly querying the latest podcast episodes.
/// Key: Composite string of `[timestamp]_[episode_id]`
/// Value: Episode ID (u64)
const PODCASTS_CHRONO_INDEX: TableDefinition<&str, u64> = TableDefinition::new("podcasts_chrono");

/// The main table where video podcast metadata is stored.
/// Key: Video ID (u64)
/// Value: Serialized VideoPodcast
const VIDEO_PODCASTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("video_podcasts");

/// The custom index table for quickly querying the latest video podcasts.
/// Key: Composite string of `[timestamp]_[video_id]`
/// Value: Video ID (u64)
const VIDEO_PODCASTS_CHRONO_INDEX: TableDefinition<&str, u64> = TableDefinition::new("video_podcasts_chrono");

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

/// A newsletter subscriber.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Subscriber {
    pub id: u64,
    pub email: String,
    pub active: bool,
    pub created_at: u64,
}

/// Example struct for a Radio Track
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Track {
    pub id: u64,
    pub title: String,
    pub artist: String,
}

/// Example struct for a Podcast Episode
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PodcastEpisode {
    pub id: u64,
    pub title: String,
    pub description: String,
    pub file_path: String,
    /// Unix timestamp of publication
    pub publish_date: u64,
}

/// Example struct for a Video Podcast
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VideoPodcast {
    pub id: u64,
    pub title: String,
    pub description: String,
    /// The directory containing the HLS .m3u8 playlist and .ts chunks
    pub hls_directory: String,
    /// Unix timestamp of publication
    pub publish_date: u64,
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
            write_txn.open_table(TRACKS_TABLE)?;
            write_txn.open_table(PODCASTS_TABLE)?;
            write_txn.open_table(PODCASTS_CHRONO_INDEX)?;
            write_txn.open_table(VIDEO_PODCASTS_TABLE)?;
            write_txn.open_table(VIDEO_PODCASTS_CHRONO_INDEX)?;
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
        Ok(table.get(id)?.map(|raw| bincode::deserialize(raw.value()).unwrap()))
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

    /// Insert a podcast episode and update the chronological index.
    pub fn insert_podcast_episode(&self, episode: &PodcastEpisode) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main_table = write_txn.open_table(PODCASTS_TABLE)?;
            let mut index_table = write_txn.open_table(PODCASTS_CHRONO_INDEX)?;

            let encoded: Vec<u8> = bincode::serialize(episode).unwrap();
            main_table.insert(episode.id, encoded.as_slice())?;

            let chrono_key = format!("{:020}_{}", episode.publish_date, episode.id);
            index_table.insert(chrono_key.as_str(), episode.id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Retrieve the `limit` most recent podcast episodes using the custom index.
    pub fn get_latest_podcast_episodes(&self, limit: usize) -> Result<Vec<PodcastEpisode>, Error> {
        let read_txn = self.db.begin_read()?;
        let index_table = read_txn.open_table(PODCASTS_CHRONO_INDEX)?;
        let main_table = read_txn.open_table(PODCASTS_TABLE)?;

        let mut results = Vec::new();

        for item in index_table.range("0"..="9").unwrap().rev().take(limit) {
            let (_, episode_id) = item.unwrap();
            let ep_id_val = episode_id.value();

            if let Some(encoded) = main_table.get(ep_id_val)? {
                let episode: PodcastEpisode = bincode::deserialize(encoded.value()).unwrap();
                results.push(episode);
            }
        }

        Ok(results)
    }

    /// Get a single podcast episode by its ID.
    pub fn get_podcast_episode(&self, id: u64) -> Result<Option<PodcastEpisode>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PODCASTS_TABLE)?;

        if let Some(encoded) = table.get(id)? {
            let episode: PodcastEpisode = bincode::deserialize(encoded.value()).unwrap();
            Ok(Some(episode))
        } else {
            Ok(None)
        }
    }

    // --- VIDEO PODCASTS ---

    /// Insert a video podcast episode and update the chronological index.
    pub fn insert_video_podcast(&self, video: &VideoPodcast) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main_table = write_txn.open_table(VIDEO_PODCASTS_TABLE)?;
            let mut index_table = write_txn.open_table(VIDEO_PODCASTS_CHRONO_INDEX)?;

            let encoded: Vec<u8> = bincode::serialize(video).unwrap();
            main_table.insert(video.id, encoded.as_slice())?;

            let chrono_key = format!("{:020}_{}", video.publish_date, video.id);
            index_table.insert(chrono_key.as_str(), video.id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Retrieve the `limit` most recent video podcasts using the custom index.
    pub fn get_latest_video_podcasts(&self, limit: usize) -> Result<Vec<VideoPodcast>, Error> {
        let read_txn = self.db.begin_read()?;
        let index_table = read_txn.open_table(VIDEO_PODCASTS_CHRONO_INDEX)?;
        let main_table = read_txn.open_table(VIDEO_PODCASTS_TABLE)?;

        let mut results = Vec::new();

        for item in index_table.range("0"..="9").unwrap().rev().take(limit) {
            let (_, video_id) = item.unwrap();
            let vid_id_val = video_id.value();

            if let Some(encoded) = main_table.get(vid_id_val)? {
                let video: VideoPodcast = bincode::deserialize(encoded.value()).unwrap();
                results.push(video);
            }
        }

        Ok(results)
    }

    /// Get a single video podcast episode by its ID.
    pub fn get_video_podcast(&self, id: u64) -> Result<Option<VideoPodcast>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(VIDEO_PODCASTS_TABLE)?;

        if let Some(encoded) = table.get(id)? {
            let video: VideoPodcast = bincode::deserialize(encoded.value()).unwrap();
            Ok(Some(video))
        } else {
            Ok(None)
        }
    }

    // --- TRACKS ---

    /// Insert a track into the database.
    pub fn insert_track(&self, track: &Track) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TRACKS_TABLE)?;
            let encoded: Vec<u8> = bincode::serialize(track).unwrap();
            table.insert(track.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Get a track by its ID.
    pub fn get_track(&self, id: u64) -> Result<Option<Track>, Error> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TRACKS_TABLE)?;

        if let Some(encoded) = table.get(id)? {
            let track: Track = bincode::deserialize(encoded.value()).unwrap();
            Ok(Some(track))
        } else {
            Ok(None)
        }
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
}
