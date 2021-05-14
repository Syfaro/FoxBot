use std::collections::HashMap;
use std::ops::Add;
use std::sync::{Arc, Mutex};

use opentelemetry::propagation::TextMapPropagator;
use tgbotapi::{
    requests::{EditMessageCaption, EditMessageReplyMarkup, ReplyMarkup},
    InlineKeyboardButton, InlineKeyboardMarkup,
};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use foxbot_models::Sites;
use foxbot_sites::BoxedSite;
use foxbot_utils::*;

fn main() {
    use opentelemetry::KeyValue;
    use tracing_subscriber::layer::SubscriberExt;

    let runtime = Arc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    );

    let env = std::env::var("ENVIRONMENT");
    let env = if let Ok(env) = env.as_ref() {
        env.as_str()
    } else if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    opentelemetry::global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());

    let tracer = runtime.block_on(async move {
        opentelemetry_jaeger::new_pipeline()
            .with_agent_endpoint(std::env::var("JAEGER_COLLECTOR").unwrap())
            .with_service_name("foxbot_background_worker")
            .with_tags(vec![
                KeyValue::new("environment", env.to_owned()),
                KeyValue::new("version", env!("CARGO_PKG_VERSION")),
            ])
            .install_batch(opentelemetry::runtime::Tokio)
            .unwrap()
    });

    let trace = tracing_opentelemetry::layer().with_tracer(tracer);
    let env_filter = tracing_subscriber::EnvFilter::from_default_env();

    if matches!(std::env::var("LOG_FMT").as_deref(), Ok("json")) {
        let subscriber = tracing_subscriber::fmt::layer()
            .json()
            .with_timer(tracing_subscriber::fmt::time::ChronoUtc::rfc3339())
            .with_target(true);
        let subscriber = tracing_subscriber::Registry::default()
            .with(env_filter)
            .with(trace)
            .with(subscriber);
        tracing::subscriber::set_global_default(subscriber).unwrap();
    } else {
        let subscriber = tracing_subscriber::fmt::layer();
        let subscriber = tracing_subscriber::Registry::default()
            .with(env_filter)
            .with(trace)
            .with(subscriber);
        tracing::subscriber::set_global_default(subscriber).unwrap();
    }

    tracing::info!("starting channel worker");

    load_env();
    let config = match envy::from_env::<Config>() {
        Ok(config) => config,
        Err(err) => panic!("{:#?}", err),
    };

    let workers: usize = std::env::var("CHANNEL_WORKERS")
        .as_deref()
        .unwrap_or("2")
        .parse()
        .unwrap_or(2);

    tracing::debug!(workers, "got worker count configuration");

    let mut faktory = faktory::ConsumerBuilder::default();
    faktory.workers(workers);

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&config.database_url);
    let pool = runtime
        .block_on(pool)
        .expect("unable to create database pool");

    let sites = runtime.block_on(foxbot_sites::get_all_sites(
        config.fa_a,
        config.fa_b,
        config.fautil_apitoken.clone(),
        config.weasyl_apitoken,
        config.twitter_consumer_key,
        config.twitter_consumer_secret,
        config.inkbunny_username,
        config.inkbunny_password,
        config.e621_login,
        config.e621_api_key,
        pool.clone(),
    ));

    let telegram = tgbotapi::Telegram::new(config.telegram_apitoken);
    let fuzzysearch = fuzzysearch::FuzzySearch::new(config.fautil_apitoken);

    let redis = redis::Client::open(config.redis_dsn).unwrap();
    let redis = runtime
        .block_on(redis::aio::ConnectionManager::new(redis))
        .expect("unable to open redis connection");

    let producer = faktory::Producer::connect(None).unwrap();

    let handler = Arc::new(Handler {
        sites: tokio::sync::Mutex::new(sites),
        telegram: Arc::new(telegram),
        producer: Arc::new(Mutex::new(producer)),
        fuzzysearch,
        conn: pool,
        redis,
        langs: load_langs(),
        best_langs: Default::default(),
    });

    let runtime_clone = runtime.clone();
    let handler_clone = handler.clone();
    faktory.register("channel_update", move |job| -> Result<(), Error> {
        let span = get_custom_span(&job);

        runtime_clone.block_on(handler_clone.process_channel_update(job).instrument(span))?;

        Ok(())
    });

    let runtime_clone = runtime.clone();
    let handler_clone = handler.clone();
    faktory.register("channel_edit", move |job| -> Result<(), Error> {
        let span = get_custom_span(&job);

        runtime_clone.block_on(handler_clone.process_channel_edit(job).instrument(span))?;

        Ok(())
    });

    let runtime_clone = runtime.clone();
    let handler_clone = handler.clone();
    faktory.register("group_photo", move |job| -> Result<(), Error> {
        let span = get_custom_span(&job);

        runtime_clone.block_on(handler_clone.process_group_photo(job).instrument(span))?;

        Ok(())
    });

    faktory.register("group_source", move |job| -> Result<(), Error> {
        let span = get_custom_span(&job);

        runtime.block_on(handler.process_group_source(job).instrument(span))?;

        Ok(())
    });

    let faktory = faktory.connect(None).unwrap();
    faktory.run_to_completion(&["foxbot_background"]);
}

#[cfg(feature = "env")]
fn load_env() {
    dotenv::dotenv().unwrap();
}

#[cfg(not(feature = "env"))]
fn load_env() {}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("telegram error: {0}")]
    Telegram(#[from] tgbotapi::Error),
    #[error("missing data")]
    MissingData,
    #[error("json error")]
    Json(#[from] serde_json::Error),
    #[error("other error: {0}")]
    Other(#[from] anyhow::Error),
}

type BestLangs = std::collections::HashMap<String, LangBundle>;

const MAX_SOURCE_DISTANCE: u64 = 3;
const NOISY_SOURCE_COUNT: usize = 4;

#[derive(serde::Deserialize, Debug, Clone)]
struct Config {
    // Site config
    fa_a: String,
    fa_b: String,
    weasyl_apitoken: String,
    inkbunny_username: String,
    inkbunny_password: String,
    e621_login: String,
    e621_api_key: String,

    // Twitter config
    twitter_consumer_key: String,
    twitter_consumer_secret: String,

    // Telegram config
    telegram_apitoken: String,

    // FuzzySearch config
    fautil_apitoken: String,

    // Worker configuration
    channel_workers: Option<usize>,
    database_url: String,
    redis_dsn: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct MessageEdit {
    chat_id: String,
    message_id: i32,
    media_group_id: Option<String>,
    firsts: Vec<(Sites, String)>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct GroupSource {
    chat_id: String,
    reply_to_message_id: i32,
    text: String,
}

struct Handler {
    sites: tokio::sync::Mutex<Vec<BoxedSite>>,

    langs: Langs,
    best_langs: tokio::sync::RwLock<BestLangs>,

    producer: Arc<Mutex<faktory::Producer<std::net::TcpStream>>>,
    telegram: Arc<tgbotapi::Telegram>,
    fuzzysearch: fuzzysearch::FuzzySearch,
    conn: sqlx::Pool<sqlx::Postgres>,
    redis: redis::aio::ConnectionManager,
}

impl Handler {
    /// Enqueue a new Faktory job by spawning a blocking task.
    async fn enqueue(&self, job: faktory::Job) {
        let producer = self.producer.clone();
        tokio::task::spawn_blocking(move || {
            let mut producer = producer.lock().unwrap();
            producer.enqueue(job).unwrap();
        });
    }

    #[tracing::instrument(skip(self, job), fields(job_id = job.id()))]
    #[deny(clippy::unwrap_used)]
    async fn process_channel_update(&self, job: faktory::Job) -> Result<(), Error> {
        let data = job
            .args()
            .iter()
            .next()
            .ok_or(Error::MissingData)?
            .to_owned();
        let message: tgbotapi::Message = serde_json::value::from_value(data)?;

        tracing::trace!("got enqueued message: {:?}", message);

        // Photos should exist for job to be enqueued.
        let sizes = match &message.photo {
            Some(photo) => photo,
            _ => return Ok(()),
        };

        let file = find_best_photo(&sizes).ok_or(Error::MissingData)?;
        let (searched_hash, mut matches) = match_image(
            &self.telegram,
            &self.conn,
            &self.fuzzysearch,
            &file,
            Some(3),
        )
        .await?;

        // Only keep matches with a distance of 3 or less
        matches.retain(|m| m.distance.unwrap_or(10) <= 3);

        if matches.is_empty() {
            tracing::debug!("unable to find sources for image");
            return Ok(());
        }

        let links = extract_links(&message);

        let mut sites = self.sites.lock().await;

        // If any matches contained a link we found in the message, skip adding
        // a source.
        if matches
            .iter()
            .any(|file| link_was_seen(&sites, &links, &file.url()))
        {
            tracing::trace!("post already contained valid source url");
            return Ok(());
        }

        if !links.is_empty() {
            let mut results: Vec<foxbot_sites::PostInfo> = Vec::new();
            let _ = find_images(&tgbotapi::User::default(), links, &mut sites, &mut |info| {
                results.extend(info.results);
            })
            .await;

            let urls: Vec<_> = results
                .iter()
                .map::<&str, _>(|result| &result.url)
                .collect();
            if has_similar_hash(searched_hash, &urls).await {
                tracing::debug!("url in post contained similar hash");

                return Ok(());
            }
        }

        drop(sites);

        if already_had_source(&self.redis, &message, &matches).await? {
            tracing::trace!("post group already contained source url");
            return Ok(());
        }

        // Keep order of sites consistent.
        sort_results_by(&foxbot_models::Sites::default_order(), &mut matches, true);

        let firsts = first_of_each_site(&matches)
            .into_iter()
            .map(|(site, file)| (site, file.url()))
            .collect();

        let data = serde_json::to_value(&MessageEdit {
            chat_id: message.chat.id.to_string(),
            message_id: message.message_id,
            media_group_id: message.media_group_id,
            firsts,
        })?;

        let mut job = faktory::Job::new("channel_edit", vec![data]).on_queue("foxbot_background");
        job.custom = get_faktory_custom();

        self.enqueue(job).await;

        Ok(())
    }

    #[tracing::instrument(skip(self, job), fields(job_id = job.id()))]
    #[deny(clippy::unwrap_used)]
    async fn process_channel_edit(&self, job: faktory::Job) -> Result<(), Error> {
        let data: serde_json::Value = job
            .args()
            .iter()
            .next()
            .ok_or(Error::MissingData)?
            .to_owned();

        tracing::trace!("got enqueued edit: {:?}", data);

        let MessageEdit {
            chat_id,
            message_id,
            media_group_id,
            firsts,
        } = serde_json::value::from_value(data.clone())?;
        let chat_id: &str = &chat_id;

        if let Some(at) = check_more_time(&self.redis, chat_id).await {
            tracing::trace!("need to wait more time for this chat: {}", at);

            let mut job =
                faktory::Job::new("channel_edit", vec![data]).on_queue("foxbot_background");
            job.at = Some(at);
            job.custom = get_faktory_custom();

            self.enqueue(job).await;

            return Ok(());
        }

        // If this photo was part of a media group, we should set a caption on
        // the image because we can't make an inline keyboard on it.
        let resp = if media_group_id.is_some() {
            let caption = firsts
                .into_iter()
                .map(|(_site, url)| url)
                .collect::<Vec<_>>()
                .join("\n");

            let edit_caption_markup = EditMessageCaption {
                chat_id: chat_id.into(),
                message_id: Some(message_id),
                caption: Some(caption),
                ..Default::default()
            };

            self.telegram.make_request(&edit_caption_markup).await
        // Not a media group, we should create an inline keyboard.
        } else {
            let buttons: Vec<_> = firsts
                .into_iter()
                .map(|(site, url)| InlineKeyboardButton {
                    text: site.as_str().to_string(),
                    url: Some(url),
                    ..Default::default()
                })
                .collect();

            let buttons = if buttons.len() % 2 == 0 {
                buttons.chunks(2).map(|chunk| chunk.to_vec()).collect()
            } else {
                buttons.chunks(1).map(|chunk| chunk.to_vec()).collect()
            };

            let markup = InlineKeyboardMarkup {
                inline_keyboard: buttons,
            };

            let edit_reply_markup = EditMessageReplyMarkup {
                chat_id: chat_id.into(),
                message_id: Some(message_id),
                reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(markup)),
                ..Default::default()
            };

            self.telegram.make_request(&edit_reply_markup).await
        };

        match resp {
            // When we get rate limited, mark the job as successful and enqueue
            // it again after the retry after period.
            Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                parameters:
                    Some(tgbotapi::ResponseParameters {
                        retry_after: Some(retry_after),
                        ..
                    }),
                ..
            })) => {
                tracing::warn!(retry_after, "rate limiting, re-enqueuing");

                let now = chrono::offset::Utc::now();
                let retry_at = now.add(chrono::Duration::seconds(retry_after as i64));

                needs_more_time(&self.redis, chat_id, retry_at).await;

                let mut job =
                    faktory::Job::new("channel_edit", vec![data]).on_queue("foxbot_background");
                job.at = Some(retry_at);
                job.custom = get_faktory_custom();

                self.enqueue(job).await;

                Ok(())
            }
            // It seems like the bot gets updates from channels, but is unable
            // to update them. Telegram often gives us a
            // 'Bad Request: MESSAGE_ID_INVALID' response here.
            //
            // The corresponding updates have inline keyboard markup, suggesting
            // that they were generated by a bot.
            //
            // I'm not sure if there's any way to detect this before processing
            // an update, so ignore these errors.
            Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                error_code: Some(400),
                description,
                ..
            })) => {
                tracing::warn!("got 400 error, ignoring: {:?}", description);

                Ok(())
            }
            // If permissions have changed (bot was removed from channel, etc.)
            // we may no longer be allowed to process this update. There's
            // nothing else we can do so mark it as successful.
            Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                error_code: Some(403),
                description,
                ..
            })) => {
                tracing::warn!("got 403 error, ignoring: {:?}", description);

                Ok(())
            }
            Ok(_) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    #[tracing::instrument(skip(self, job), fields(job_id = job.id()))]
    #[deny(clippy::unwrap_used)]
    async fn process_group_photo(&self, job: faktory::Job) -> Result<(), Error> {
        use foxbot_models::{GroupConfig, GroupConfigKey};

        let data: serde_json::Value = job
            .args()
            .iter()
            .next()
            .ok_or(Error::MissingData)?
            .to_owned();

        let message: tgbotapi::Message = serde_json::value::from_value(data)?;
        let photo_sizes = match &message.photo {
            Some(sizes) => sizes,
            _ => return Ok(()),
        };

        tracing::trace!("got enqueued message: {:?}", message);

        match GroupConfig::get(&self.conn, message.chat.id, GroupConfigKey::GroupAdd).await? {
            Some(val) if val => (),
            _ => return Ok(()),
        }

        let best_photo = find_best_photo(&photo_sizes).unwrap();
        let mut matches = match_image(
            &self.telegram,
            &self.conn,
            &self.fuzzysearch,
            &best_photo,
            Some(3),
        )
        .await?
        .1;
        sort_results(&self.conn, message.from.as_ref().unwrap().id, &mut matches).await?;

        let wanted_matches = matches
            .iter()
            .filter(|m| m.distance.unwrap() <= MAX_SOURCE_DISTANCE)
            .collect::<Vec<_>>();

        if wanted_matches.is_empty() {
            return Ok(());
        }

        let links = extract_links(&message);
        let sites = self.sites.lock().await;

        if wanted_matches
            .iter()
            .any(|m| link_was_seen(&sites, &links, &m.url()))
        {
            return Ok(());
        }

        drop(sites);

        let twitter_matches = wanted_matches
            .iter()
            .filter(|m| matches!(m.site_info, Some(fuzzysearch::SiteInfo::Twitter)))
            .count();
        let other_matches = wanted_matches.len() - twitter_matches;

        // Prevents memes from getting a million links in chat
        if other_matches <= 1 && twitter_matches >= NOISY_SOURCE_COUNT {
            tracing::trace!(
                twitter_matches,
                other_matches,
                "had too many matches, ignoring"
            );
            return Ok(());
        }

        let lang = message
            .from
            .as_ref()
            .and_then(|from| from.language_code.as_deref());

        let text = self
            .get_fluent_bundle(lang, |bundle| {
                if wanted_matches.len() == 1 {
                    let mut args = fluent::FluentArgs::new();
                    let m = wanted_matches.first().unwrap();
                    args.insert("link", m.url().into());
                    let rating =
                        get_message(&bundle, get_rating_bundle_name(&m.rating), None).unwrap();
                    args.insert("rating", rating.into());

                    get_message(bundle, "automatic-single", Some(args)).unwrap()
                } else {
                    let mut buf = String::new();

                    buf.push_str(&get_message(bundle, "automatic-multiple", None).unwrap());
                    buf.push('\n');

                    for result in wanted_matches {
                        let mut args = fluent::FluentArgs::new();
                        args.insert("link", result.url().into());
                        let rating =
                            get_message(&bundle, get_rating_bundle_name(&result.rating), None)
                                .unwrap();
                        args.insert("rating", rating.into());

                        buf.push_str(
                            &get_message(bundle, "automatic-multiple-result", Some(args)).unwrap(),
                        );
                        buf.push('\n');
                    }

                    buf
                }
            })
            .await;

        let data = serde_json::to_value(&GroupSource {
            chat_id: message.chat.id.to_string(),
            reply_to_message_id: message.message_id,
            text,
        })?;

        let mut job = faktory::Job::new("group_source", vec![data]).on_queue("foxbot_background");
        job.custom = get_faktory_custom();

        self.enqueue(job).await;

        Ok(())
    }

    #[tracing::instrument(skip(self, job), fields(job_id = job.id()))]
    #[deny(clippy::unwrap_used)]
    async fn process_group_source(&self, job: faktory::Job) -> Result<(), Error> {
        use tgbotapi::requests::SendMessage;

        let data: serde_json::Value = job
            .args()
            .iter()
            .next()
            .ok_or(Error::MissingData)?
            .to_owned();

        tracing::trace!("got enqueued group source: {:?}", data);

        let GroupSource {
            chat_id,
            reply_to_message_id,
            text,
        } = serde_json::value::from_value(data.clone())?;
        let chat_id: &str = &chat_id;

        if let Some(at) = check_more_time(&self.redis, chat_id).await {
            tracing::trace!("need to wait more time for this chat: {}", at);

            let mut job =
                faktory::Job::new("group_source", vec![data]).on_queue("foxbot_background");
            job.at = Some(at);
            job.custom = get_faktory_custom();

            self.enqueue(job).await;

            return Ok(());
        }

        let message = SendMessage {
            chat_id: chat_id.into(),
            reply_to_message_id: Some(reply_to_message_id),
            disable_web_page_preview: Some(true),
            disable_notification: Some(true),
            text,
            ..Default::default()
        };

        match self.telegram.make_request(&message).await {
            Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                parameters:
                    Some(tgbotapi::ResponseParameters {
                        retry_after: Some(retry_after),
                        ..
                    }),
                ..
            })) => {
                tracing::warn!(retry_after, "rate limiting, re-enqueuing");

                let now = chrono::offset::Utc::now();
                let retry_at = now.add(chrono::Duration::seconds(retry_after as i64));

                needs_more_time(&self.redis, chat_id, retry_at).await;

                let mut job =
                    faktory::Job::new("group_source", vec![data]).on_queue("foxbot_background");
                job.at = Some(retry_at);
                job.custom = get_faktory_custom();

                self.enqueue(job).await;

                Ok(())
            }
            Ok(_)
            | Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                error_code: Some(400),
                ..
            })) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Build a fluent language bundle for a specified language and cache the
    /// result.
    async fn get_fluent_bundle<C, R>(&self, requested: Option<&str>, callback: C) -> R
    where
        C: FnOnce(&fluent::concurrent::FluentBundle<fluent::FluentResource>) -> R,
    {
        let requested = requested.unwrap_or(L10N_LANGS[0]);

        tracing::trace!(lang = requested, "Looking up language bundle");

        {
            let lock = self.best_langs.read().await;
            if let Some(bundle) = lock.get(requested) {
                return callback(bundle);
            }
        }

        tracing::debug!(lang = requested, "Got new language, building bundle");

        let bundle = get_lang_bundle(&self.langs, requested);

        // Lock for writing for as short as possible.
        {
            let mut lock = self.best_langs.write().await;
            lock.insert(requested.to_string(), bundle);
        }

        let lock = self.best_langs.read().await;
        let bundle = lock.get(requested).expect("value just inserted is missing");
        callback(bundle)
    }
}

/// Set that chat needs additional time before another message can be sent.
#[tracing::instrument(skip(conn))]
async fn needs_more_time(
    conn: &redis::aio::ConnectionManager,
    chat_id: &str,
    at: chrono::DateTime<chrono::Utc>,
) {
    use redis::AsyncCommands;

    let mut conn = conn.clone();

    let now = chrono::Utc::now();
    let seconds = (at - now).num_seconds();

    if seconds <= 0 {
        tracing::warn!(seconds, "needed time already happened");
    }

    let key = format!("retry-at:{}", chat_id);
    if let Err(err) = conn
        .set_ex::<_, _, ()>(&key, at.timestamp(), seconds as usize)
        .await
    {
        tracing::error!("unable to set retry-at: {:?}", err);
    }
}

/// Check if a chat needs more time before a message can be sent.
#[tracing::instrument(skip(conn))]
async fn check_more_time(
    conn: &redis::aio::ConnectionManager,
    chat_id: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    use redis::AsyncCommands;

    let mut conn = conn.clone();

    let key = format!("retry-at:{}", chat_id);
    match conn.get::<_, Option<i64>>(&key).await {
        Ok(Some(timestamp)) => {
            let after = chrono::Utc.timestamp(timestamp, 0);
            if after <= chrono::Utc::now() {
                tracing::trace!("retry-at was in past, ignoring");
                None
            } else {
                Some(after)
            }
        }
        Ok(None) => None,
        Err(err) => {
            tracing::error!("unable to get retry-at: {:?}", err);

            None
        }
    }
}

/// Telegram only shows a caption on a media group if there is a single caption
/// anywhere in the group. When users upload a group, we need to check if we can
/// only set a single source to make the link more visible. This can be done by
/// ensuring our source has been previouly used in the media group.
///
/// For our implementation, this is done by maintaining a Redis set of every
/// source previously displayed. If adding our source links returns fewer
/// inserted than we had, it means a link was previously used and therefore we
/// do not have to set a source.
///
/// Because Telegram doesn't send media groups at once, we have to store these
/// values until we're sure the group is over. In this case, we will store
/// values for 300 seconds.
///
/// No link normalization is required here because all links are already
/// normalized when coming from FuzzySearch.
async fn already_had_source(
    conn: &redis::aio::ConnectionManager,
    message: &tgbotapi::Message,
    matches: &[fuzzysearch::File],
) -> anyhow::Result<bool> {
    use redis::AsyncCommands;

    let group_id = match &message.media_group_id {
        Some(id) => id,
        _ => return Ok(false),
    };

    let key = format!("group-sources:{}", group_id);

    let mut urls: Vec<_> = matches.iter().map(|m| m.url()).collect();
    urls.sort();
    urls.dedup();
    let source_count = urls.len();

    tracing::trace!(%group_id, "adding new sources: {:?}", urls);

    let mut conn = conn.clone();
    let added_links: usize = conn.sadd(&key, urls).await?;
    conn.expire(&key, 300).await?;

    tracing::debug!(
        source_count,
        added_links,
        "determined existing and new source links"
    );

    Ok(source_count > added_links)
}

/// Check if any of the provided image URLs have a hash similar to the given
/// input.
#[tracing::instrument(skip(urls))]
async fn has_similar_hash(to: i64, urls: &[&str]) -> bool {
    let to = to.to_be_bytes();

    for url in urls {
        let check_size = CheckFileSize::new(url, 50_000_000);
        let bytes = match check_size.into_bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!("unable to download image: {:?}", err);

                continue;
            }
        };

        let hash = tokio::task::spawn_blocking(move || {
            use std::convert::TryInto;

            let hasher = fuzzysearch::get_hasher();

            let im = match image::load_from_memory(&bytes) {
                Ok(im) => im,
                Err(err) => {
                    tracing::warn!("unable to load image: {:?}", err);

                    return None;
                }
            };

            let hash = hasher.hash_image(&im);
            let bytes: [u8; 8] = hash.as_bytes().try_into().unwrap_or_default();

            Some(bytes)
        })
        .in_current_span()
        .await
        .unwrap_or_default();

        let hash = match hash {
            Some(hash) => hash,
            _ => continue,
        };

        if hamming::distance_fast(&to, &hash).unwrap() <= 3 {
            tracing::debug!(url, hash = i64::from_be_bytes(hash), "hashes were similar");

            return true;
        }
    }

    false
}

fn get_custom_span(job: &faktory::Job) -> tracing::Span {
    let custom: HashMap<String, String> = job
        .custom
        .iter()
        .filter_map(|(key, value)| match value.as_str() {
            Some(s) => Some((key.to_owned(), s.to_owned())),
            _ => None,
        })
        .collect();
    let propagator = opentelemetry::sdk::propagation::TraceContextPropagator::new();
    let context = propagator.extract(&custom);

    let span = tracing::info_span!("faktory_job");
    span.set_parent(context);

    span
}

#[cfg(test)]
mod tests {
    async fn get_redis() -> redis::aio::ConnectionManager {
        let redis_client =
            redis::Client::open(std::env::var("REDIS_DSN").expect("Missing REDIS_DSN")).unwrap();
        redis::aio::ConnectionManager::new(redis_client)
            .await
            .expect("unable to open Redis connection")
    }

    #[tokio::test]
    #[ignore]
    async fn test_already_had_source() {
        let _ = tracing_subscriber::fmt::try_init();

        use super::already_had_source;

        let mut conn = get_redis().await;
        let _: () = redis::cmd("FLUSHDB").query_async(&mut conn).await.unwrap();

        let site_info = Some(fuzzysearch::SiteInfo::FurAffinity(
            fuzzysearch::FurAffinityFile { file_id: 123 },
        ));

        let message = tgbotapi::Message {
            media_group_id: Some("test-group".to_string()),
            ..Default::default()
        };

        let sources = vec![fuzzysearch::File {
            site_id: 123,
            site_info: site_info.clone(),
            ..Default::default()
        }];

        let resp = already_had_source(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            false,
            "filtering with no results should have no status flag"
        );

        let resp = already_had_source(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            true,
            "filtering with same media group id and source should flag completed"
        );

        let sources = vec![fuzzysearch::File {
            site_id: 456,
            site_info: site_info.clone(),
            ..Default::default()
        }];

        let resp = already_had_source(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            false,
            "filtering same group with new source should have no status flag"
        );

        let message = tgbotapi::Message {
            media_group_id: Some("test-group-2".to_string()),
            ..Default::default()
        };

        let resp = already_had_source(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            false,
            "different group should not be affected by other group sources"
        );

        let sources = vec![
            fuzzysearch::File {
                site_id: 456,
                site_info: site_info.clone(),
                ..Default::default()
            },
            fuzzysearch::File {
                site_id: 789,
                site_info: site_info.clone(),
                ..Default::default()
            },
        ];

        let resp = already_had_source(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            true,
            "adding a new with an old source should set a completed flag"
        );
    }
}