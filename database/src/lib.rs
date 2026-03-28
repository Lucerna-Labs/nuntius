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

/// The main Database manager for AzuraCast
pub struct AzuraDB {
    db: Database,
}

impl AzuraDB {
    /// Initialize the database from a file path.
    pub fn new(path: &str) -> Result<Self, Error> {
        let db = Database::create(path)?;

        // Ensure our tables are created
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(BLOG_POSTS_TABLE)?;
            write_txn.open_table(BLOG_POSTS_CHRONO_INDEX)?;
            write_txn.open_table(TRACKS_TABLE)?;
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

            // Serialize the post
            let encoded: Vec<u8> = bincode::serialize(post).unwrap();

            // 1. Save to main table
            main_table.insert(post.id, encoded.as_slice())?;

            // 2. Save to chronological index.
            // Format: "0000000000001700000000_1"
            // We pad the timestamp with zeros so lexicographical sorting works correctly.
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

        // `redb` ranges return values in ascending order by default.
        // We use `.rev()` to read from the end (newest) backwards.
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
