#![allow(clippy::result_large_err)]
// `redb::Error` is ~160 bytes; clippy::result_large_err is upstream-driven and
// not actionable without wrapping every call site.

use redb::{Database, Error, TableDefinition};
use serde::{Deserialize, Serialize};

/// The main table where raw blog posts are stored.
/// Key: Post ID (u64)
/// Value: Serialized BlogPost
const BLOG_POSTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("blog_posts");

/// The custom index table for quickly querying the latest blog posts.
/// Key: Composite string of `[timestamp]_[post_id]`
/// Value: Post ID (u64)
const BLOG_POSTS_CHRONO_INDEX: TableDefinition<&str, u64> = TableDefinition::new("blog_posts_chrono");

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

/// Example struct for a Blog Post
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BlogPost {
    pub id: u64,
    pub title: String,
    pub content: String,
    /// Unix timestamp
    pub timestamp: u64,
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

    /// Insert a blog post and update the chronological index.
    pub fn insert_blog_post(&self, post: &BlogPost) -> Result<(), Error> {
        let write_txn = self.db.begin_write()?;
        {
            let mut main_table = write_txn.open_table(BLOG_POSTS_TABLE)?;
            let mut index_table = write_txn.open_table(BLOG_POSTS_CHRONO_INDEX)?;

            let encoded: Vec<u8> = bincode::serialize(post).unwrap();
            main_table.insert(post.id, encoded.as_slice())?;

            let chrono_key = format!("{:020}_{}", post.timestamp, post.id);
            index_table.insert(chrono_key.as_str(), post.id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Retrieve the `limit` most recent blog posts using the custom index.
    pub fn get_latest_blog_posts(&self, limit: usize) -> Result<Vec<BlogPost>, Error> {
        let read_txn = self.db.begin_read()?;
        let index_table = read_txn.open_table(BLOG_POSTS_CHRONO_INDEX)?;
        let main_table = read_txn.open_table(BLOG_POSTS_TABLE)?;

        let mut results = Vec::new();

        for item in index_table.range("0"..="9").unwrap().rev().take(limit) {
            let (_, post_id) = item.unwrap();
            let post_id_val = post_id.value();

            if let Some(encoded) = main_table.get(post_id_val)? {
                let post: BlogPost = bincode::deserialize(encoded.value()).unwrap();
                results.push(post);
            }
        }

        Ok(results)
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
}
