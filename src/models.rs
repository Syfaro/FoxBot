use anyhow::Context;
use quaint::pooled::PooledConnection;
use quaint::prelude::*;

static USER_CONFIG: &str = "user_config";
static TWITTER_ACCOUNT: &str = "twitter_account";
static TWITTER_AUTH: &str = "twitter_auth";
static FILE_ID_CACHE: &str = "file_id_cache";
static GROUP_CONFIG: &str = "group_config";

/// Each available site, for configuration usage.
#[derive(Clone, Debug, PartialEq)]
pub enum Sites {
    FurAffinity,
    E621,
    Twitter,
}

impl serde::Serialize for Sites {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug)]
pub struct ParseSitesError;

impl std::str::FromStr for Sites {
    type Err = ParseSitesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "FurAffinity" => Ok(Sites::FurAffinity),
            "e621" => Ok(Sites::E621),
            "Twitter" => Ok(Sites::Twitter),
            _ => Err(ParseSitesError),
        }
    }
}

impl Sites {
    /// Get the user-understandable name of the site.
    pub fn as_str(&self) -> &'static str {
        match *self {
            Sites::FurAffinity => "FurAffinity",
            Sites::E621 => "e621",
            Sites::Twitter => "Twitter",
        }
    }

    /// The bot's default site ordering.
    pub fn default_order() -> Vec<Sites> {
        vec![Sites::FurAffinity, Sites::E621, Sites::Twitter]
    }
}

struct Config;

impl Config {
    /// Execute a query and parse the value field from JSON into `T`.
    async fn get<T: serde::de::DeserializeOwned>(
        conn: &PooledConnection,
        select: quaint::ast::Select<'_>,
    ) -> anyhow::Result<Option<T>> {
        let rows = conn
            .select(select)
            .await
            .context("unable to select config")?;
        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows.into_single()?;
        let value = match row["value"].as_str() {
            Some(val) => val,
            _ => return Ok(None),
        };

        let data: T = serde_json::from_str(&value).context("unable to deserialize config data")?;

        Ok(Some(data))
    }

    async fn delete(
        conn: &PooledConnection,
        delete: quaint::ast::Delete<'_>,
    ) -> anyhow::Result<()> {
        conn.delete(delete)
            .await
            .context("unable to delete config item")?;

        Ok(())
    }
}

pub struct UserConfig;

pub enum UserConfigKey {
    SourceName,
    SiteSortOrder,
}

impl UserConfigKey {
    fn as_str(&self) -> &str {
        match self {
            UserConfigKey::SourceName => "source-name",
            UserConfigKey::SiteSortOrder => "site-sort-order",
        }
    }
}

impl UserConfig {
    /// Get a configuration value from the user_config table.
    ///
    /// If the value does not exist for a given user, returns None.
    pub async fn get<T: serde::de::DeserializeOwned>(
        conn: &PooledConnection,
        key: UserConfigKey,
        user_id: i32,
    ) -> anyhow::Result<Option<T>> {
        let select = Select::from_table(USER_CONFIG)
            .so_that("user_id".equals(user_id).and("name".equals(key.as_str())));

        Config::get(&conn, select).await
    }

    /// Set a configuration value for the user_config table.
    pub async fn set<T: serde::Serialize>(
        conn: &PooledConnection,
        key: &str,
        user_id: i32,
        update: bool,
        data: T,
    ) -> anyhow::Result<()> {
        let value = serde_json::to_string(&data).context("unable to serialize user config item")?;

        if update {
            let update = Update::table(USER_CONFIG)
                .set("value", value)
                .so_that("user_id".equals(user_id).and("name".equals(key)));
            conn.update(update)
                .await
                .context("unable to set user config")?;
        } else {
            let insert = Insert::single_into(USER_CONFIG)
                .value("user_id", user_id)
                .value("name", key)
                .value("value", value)
                .build();
            conn.insert(insert)
                .await
                .context("unable to insert user config")?;
        }

        Ok(())
    }
}

pub struct GroupConfig;

pub enum GroupConfigKey {
    GroupAdd,
    IsAdmin,
    GroupNoPreviews,
}

impl GroupConfigKey {
    fn as_str(&self) -> &str {
        match self {
            GroupConfigKey::GroupAdd => "group_add",
            GroupConfigKey::IsAdmin => "is_admin",
            GroupConfigKey::GroupNoPreviews => "group_no_previews",
        }
    }
}

impl GroupConfig {
    pub async fn get<T: serde::de::DeserializeOwned>(
        conn: &PooledConnection,
        chat_id: i64,
        name: GroupConfigKey,
    ) -> anyhow::Result<Option<T>> {
        let select = Select::from_table(GROUP_CONFIG)
            .so_that("chat_id".equals(chat_id).and("name".equals(name.as_str())));
        Config::get(&conn, select).await
    }

    pub async fn set<T: serde::Serialize>(
        conn: &PooledConnection,
        key: GroupConfigKey,
        chat_id: i64,
        update: bool,
        data: T,
    ) -> anyhow::Result<()> {
        let value = serde_json::to_string(&data).context("unable to set group config")?;

        if update {
            let update = Update::table(GROUP_CONFIG)
                .set("value", value)
                .so_that("chat_id".equals(chat_id).and("name".equals(key.as_str())));
            conn.update(update)
                .await
                .context("unable to update group config")?;
        } else {
            let insert = Insert::single_into(GROUP_CONFIG)
                .value("chat_id", chat_id)
                .value("name", key.as_str())
                .value("value", value)
                .build();
            conn.insert(insert)
                .await
                .context("unable to insert group config")?;
        }

        Ok(())
    }

    pub async fn delete(
        conn: &PooledConnection,
        key: GroupConfigKey,
        chat_id: i64,
    ) -> anyhow::Result<()> {
        let delete = Delete::from_table(GROUP_CONFIG)
            .so_that("chat_id".equals(chat_id).and("name".equals(key.as_str())));
        Config::delete(&conn, delete)
            .await
            .context("unable to delete group config")?;
        Ok(())
    }
}

/// A Twitter account, as stored within the database.
pub struct TwitterAccount {
    pub consumer_key: String,
    pub consumer_secret: String,
}

pub struct TwitterRequest {
    pub request_key: String,
    pub request_secret: String,
}

pub struct Twitter;

impl Twitter {
    /// Look up a user's Twitter credentials.
    pub async fn get_account(
        conn: &PooledConnection,
        user_id: i32,
    ) -> anyhow::Result<Option<TwitterAccount>> {
        let select = Select::from_table(TWITTER_ACCOUNT)
            .column("consumer_key")
            .column("consumer_secret")
            .so_that("user_id".equals(user_id));
        let rows = conn
            .select(select)
            .await
            .context("unable to select twitter consumer")?;

        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows
            .into_single()
            .context("impossible missing twitter consumer")?;

        Ok(Some(TwitterAccount {
            consumer_key: row["consumer_key"].to_string().unwrap(),
            consumer_secret: row["consumer_secret"].to_string().unwrap(),
        }))
    }

    /// Look up a pending request to sign into a Twitter account.
    pub async fn get_request(
        conn: &PooledConnection,
        user_id: i32,
    ) -> anyhow::Result<Option<TwitterRequest>> {
        let select = Select::from_table(TWITTER_AUTH)
            .column("request_key")
            .column("request_secret")
            .so_that("user_id".equals(user_id));
        let rows = conn
            .select(select)
            .await
            .context("unable to select twitter request")?;

        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows
            .into_single()
            .context("impossible missing twitter request")?;

        Ok(Some(TwitterRequest {
            request_key: row["request_key"].to_string().unwrap(),
            request_secret: row["request_secret"].to_string().unwrap(),
        }))
    }

    /// Update a user's Twitter account with new credentials.
    ///
    /// Takes care of the following housekeeping items:
    /// * Deletes any previous accounts
    /// * Inserts the key and secret for the user
    /// * Deletes the pending request
    pub async fn set_account(
        conn: &PooledConnection,
        user_id: i32,
        creds: TwitterAccount,
    ) -> anyhow::Result<()> {
        let delete = Delete::from_table(TWITTER_ACCOUNT).so_that("user_id".equals(user_id));
        conn.delete(delete)
            .await
            .context("unable to delete previous twitter accounts")?;

        let insert = Insert::single_into(TWITTER_ACCOUNT)
            .value("user_id", user_id)
            .value("consumer_key", creds.consumer_key)
            .value("consumer_secret", creds.consumer_secret)
            .build();
        conn.insert(insert)
            .await
            .context("unable to insert twitter account")?;

        let delete = Delete::from_table(TWITTER_AUTH).so_that("user_id".equals(user_id));
        conn.delete(delete)
            .await
            .context("unable to delete twitter auth")?;

        Ok(())
    }

    pub async fn set_request(
        conn: &PooledConnection,
        user_id: i32,
        creds: TwitterRequest,
    ) -> anyhow::Result<()> {
        let delete = Delete::from_table(TWITTER_AUTH).so_that("user_id".equals(user_id));
        conn.delete(delete)
            .await
            .context("unable to delete pending twitter auths")?;

        let insert = Insert::single_into(TWITTER_AUTH)
            .value("user_id", user_id)
            .value("request_key", creds.request_key)
            .value("request_secret", creds.request_secret)
            .build();
        conn.insert(insert)
            .await
            .context("unable to insert twitter auth")?;

        Ok(())
    }
}

pub struct FileCache;

impl FileCache {
    /// Look up a file's cached hash by its unique ID.
    pub async fn get(conn: &PooledConnection, file_id: &str) -> anyhow::Result<Option<i64>> {
        let select = Select::from_table(FILE_ID_CACHE)
            .column("hash")
            .so_that("file_id".equals(file_id));
        let rows = conn
            .select(select)
            .await
            .context("unable to query file cache")?;

        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows
            .into_single()
            .context("impossible missing file cache")?;
        Ok(Some(row["hash"].as_i64().unwrap()))
    }

    pub async fn set(conn: &PooledConnection, file_id: &str, hash: i64) -> anyhow::Result<()> {
        let insert = Insert::single_into(FILE_ID_CACHE)
            .value("file_id", file_id)
            .value("hash", hash)
            .build();
        let _ = conn
            .insert(insert)
            .await
            .context("unable to insert file cache item");

        Ok(())
    }
}

pub struct Video {
    /// Database identifier of the video.
    pub id: i64,
    /// If the video has already been processed. If this is true, there must
    /// be an mp4_url.
    pub processed: bool,
    /// The original source of the video.
    pub source: String,
    /// The URL of the original video.
    pub url: String,
    /// The URL of the converted video.
    pub mp4_url: Option<String>,
    pub chat_id: Option<i64>,
    pub message_id: Option<i32>,
}

impl Video {
    /// Lookup a Video by ID.
    pub async fn lookup_id(conn: &PooledConnection, id: i64) -> anyhow::Result<Option<Video>> {
        let select = Select::from_table("videos")
            .column("id")
            .column("processed")
            .column("source")
            .column("url")
            .column("mp4_url")
            .column("chat_id")
            .column("message_id")
            .so_that("id".equals(id));
        let rows = conn
            .select(select)
            .await
            .context("unable to query video ids")?;

        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows
            .into_single()
            .context("impossible missing video lookup")?;

        Ok(Some(Video {
            id: row["id"].as_i64().unwrap(),
            processed: row["processed"].as_bool().unwrap_or(false),
            source: row["source"].to_string().unwrap(),
            url: row["url"].to_string().unwrap(),
            mp4_url: row["mp4_url"].to_string(),
            chat_id: row["chat_id"].as_i64(),
            message_id: row["message_id"].as_i64().map(|id| id as i32),
        }))
    }

    /// Lookup a Video by URL.
    pub async fn lookup_url(conn: &PooledConnection, url: &str) -> anyhow::Result<Option<Video>> {
        let select = Select::from_table("videos")
            .column("id")
            .column("processed")
            .column("source")
            .column("url")
            .column("mp4_url")
            .column("chat_id")
            .column("message_id")
            .so_that("url".equals(url));
        let rows = conn
            .select(select)
            .await
            .context("unable to query video urls")?;

        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows
            .into_single()
            .context("impossible missing video lookup")?;

        Ok(Some(Video {
            id: row["id"].as_i64().unwrap(),
            processed: row["processed"].as_bool().unwrap_or(false),
            source: row["source"].to_string().unwrap(),
            url: row["url"].to_string().unwrap(),
            mp4_url: row["mp4_url"].to_string(),
            chat_id: row["chat_id"].as_i64(),
            message_id: row["message_id"].as_i64().map(|id| id as i32),
        }))
    }

    /// Insert a new URL into the database and return the ID.
    #[cfg(feature = "sqlite")]
    pub async fn insert_url(
        conn: &PooledConnection,
        url: &str,
        source: &str,
    ) -> anyhow::Result<u64> {
        let insert = Insert::single_into("videos")
            .value("url", url)
            .value("source", source)
            .build();
        let res = conn.insert(insert).await?;

        let id = res.last_insert_id().unwrap();

        Ok(id)
    }

    /// Insert a new URL into the database and return the ID.
    #[cfg(feature = "postgres")]
    pub async fn insert_url(
        conn: &PooledConnection,
        url: &str,
        source: &str,
    ) -> anyhow::Result<u64> {
        let insert = Insert::single_into("videos")
            .value("url", url)
            .value("source", source)
            .build()
            .returning(vec!["id"]);
        let res = conn.insert(insert).await?;

        let id = res.into_single()?["id"].as_i64().unwrap() as u64;

        Ok(id)
    }

    /// Update a video's mp4_url when it has been processed.
    pub async fn set_processed_url(
        conn: &PooledConnection,
        id: i64,
        mp4_url: &str,
    ) -> anyhow::Result<()> {
        let update = Update::table("videos")
            .set("processed", true)
            .set("mp4_url", mp4_url)
            .so_that("id".equals(id));
        conn.update(update).await?;

        Ok(())
    }

    pub async fn set_message_id(
        conn: &PooledConnection,
        id: i64,
        chat_id: i64,
        message_id: i32,
    ) -> anyhow::Result<()> {
        let update = Update::table("videos")
            .set("chat_id", chat_id)
            .set("message_id", message_id)
            .so_that("id".equals(id));
        conn.update(update).await?;

        Ok(())
    }
}

pub struct CachedPost {
    pub id: i64,
    pub post_url: String,
    pub thumb: bool,
    pub cdn_url: String,
    pub dimensions: (u32, u32),
}

impl CachedPost {
    pub async fn get(
        conn: &PooledConnection,
        post_url: &str,
        thumb: bool,
    ) -> anyhow::Result<Option<CachedPost>> {
        let select = Select::from_table("cached_post")
            .column("id")
            .column("post_url")
            .column("thumb")
            .column("cdn_url")
            .column("width")
            .column("height")
            .so_that("post_url".equals(post_url).and("thumb".equals(thumb)));
        let rows = conn
            .select(select)
            .await
            .context("unable to query cached posts")?;

        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows
            .into_single()
            .context("impossible missing cached post lookup")?;

        Ok(Some(CachedPost {
            id: row["id"].as_i64().unwrap(),
            post_url: row["post_url"].to_string().unwrap(),
            thumb: row["thumb"].as_bool().unwrap(),
            cdn_url: row["cdn_url"].to_string().unwrap(),
            dimensions: (
                row["width"].as_i64().unwrap() as u32,
                row["height"].as_i64().unwrap() as u32,
            ),
        }))
    }

    #[cfg(feature = "sqlite")]
    pub async fn save(
        conn: &PooledConnection,
        post_url: &str,
        cdn_url: &str,
        thumb: bool,
        dimensions: (u32, u32),
    ) -> anyhow::Result<u64> {
        let insert = Insert::single_into("cached_post")
            .value("post_url", post_url)
            .value("thumb", thumb)
            .value("cdn_url", cdn_url)
            .value("width", dimensions.0 as i64)
            .value("height", dimensions.1 as i64)
            .build();
        let res = conn.insert(insert).await?;

        let id = res.last_insert_id().unwrap();

        Ok(id)
    }

    #[cfg(feature = "postgres")]
    pub async fn save(
        conn: &PooledConnection,
        post_url: &str,
        cdn_url: &str,
        thumb: bool,
        dimensions: (u32, u32),
    ) -> anyhow::Result<u64> {
        let insert = Insert::single_into("cached_post")
            .value("post_url", post_url)
            .value("thumb", thumb)
            .value("cdn_url", cdn_url)
            .value("width", dimensions.0 as i64)
            .value("height", dimensions.1 as i64)
            .build()
            .returning(vec!["id"]);
        let res = conn.insert(insert).await?;

        let id = res.into_single()?["id"].as_i64().unwrap() as u64;

        Ok(id)
    }
}
