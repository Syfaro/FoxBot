use super::Status::*;
use crate::generate_id;
use crate::models::Video;
use crate::needs_field;
use crate::sites::PostInfo;
use crate::utils::*;
use anyhow::Context;
use async_trait::async_trait;
use tgbotapi::{requests::*, *};

pub struct InlineHandler;

#[derive(PartialEq)]
pub enum ResultType {
    Ready,
    VideoToBeProcessed,
}

impl InlineHandler {
    async fn process_video(
        &self,
        handler: &crate::MessageHandler,
        message: &Message,
    ) -> anyhow::Result<()> {
        let text = message.text.as_ref().unwrap();
        let id = match text.split('-').nth(1) {
            Some(id) => id,
            None => return Ok(()),
        };
        let id: i64 = match id.parse() {
            Ok(id) => id,
            Err(_err) => return Ok(()),
        };

        let conn = handler.conn.check_out().await?;
        let video = Video::lookup_id(&conn, id).await?.expect("missing video");

        handler
            .coconut
            .start_video(&video.url, &video.id.to_string())
            .await?;

        let lang = message
            .from
            .as_ref()
            .and_then(|from| from.language_code.as_deref());

        let video_starting = handler
            .get_fluent_bundle(lang, |bundle| {
                get_message(&bundle, "video-starting", None).unwrap()
            })
            .await;

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text: video_starting,
            ..Default::default()
        };
        let sent = handler.make_request(&send_message).await?;

        Video::set_message_id(&conn, id, sent.chat.id, sent.message_id).await?;

        Ok(())
    }
}

#[async_trait]
impl super::Handler for InlineHandler {
    fn name(&self) -> &'static str {
        "inline"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<super::Status> {
        if let Some(message) = &update.message {
            match message.get_command() {
                Some(cmd) if cmd.name == "/start" => {
                    if let Some(text) = &message.text {
                        if text.contains("process-") {
                            self.process_video(&handler, &message).await?;
                            return Ok(Completed);
                        }
                    }
                }
                _ => (),
            }
        }

        let inline = needs_field!(update, inline_query);

        let links: Vec<_> = handler.finder.links(&inline.query).collect();
        let mut results: Vec<PostInfo> = Vec::new();

        tracing::info!(query = ?inline.query, "got query");
        tracing::debug!(?links, "found links");

        let influx = handler.influx.clone();
        // Lock sites in order to find which of these links are usable
        {
            let mut sites = handler.sites.lock().await;
            let links = links.iter().map(|link| link.as_str()).collect();
            find_images(&inline.from, links, &mut sites, &mut |info| {
                let influx = influx.clone();
                let duration = info.duration;
                let count = info.results.len();
                let name = info.site.name();

                // Log a point to InfluxDB with information about our inline query
                tokio::spawn(async move {
                    let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "inline")
                        .add_tag("site", name.replace(" ", "_"))
                        .add_field("count", count as i32)
                        .add_field("duration", duration);

                    influx.query(&point).await
                });

                results.extend(info.results);
            })
            .await
            .context("unable to find images")?;
        }

        let mut responses: Vec<(ResultType, InlineQueryResult)> = vec![];

        for result in results {
            if let Some(items) = process_result(&handler, &result, &inline.from).await? {
                responses.extend(items);
            }
        }

        // If we had no responses but the query was not empty, there were likely links
        // that we were unable to convert. We need to display that the links had no results.
        if responses.is_empty() && !inline.query.is_empty() {
            let article = handler
                .get_fluent_bundle(inline.from.language_code.as_deref(), |bundle| {
                    InlineQueryResult::article(
                        generate_id(),
                        get_message(&bundle, "inline-no-results-title", None).unwrap(),
                        get_message(&bundle, "inline-no-results-body", None).unwrap(),
                    )
                })
                .await;

            responses.push((ResultType::Ready, article));
        }

        // Check if we need to process any videos contained within the results.
        // These don't get returned as regular inline results.
        let has_video: Option<InlineQueryResult> = responses
            .iter()
            .find(|item| item.0 == ResultType::VideoToBeProcessed)
            .map(|item| item.1.clone());

        // Get the rest of the ready results which should still be displayed.
        let cleaned_responses = responses
            .into_iter()
            .filter(|item| item.0 == ResultType::Ready)
            .map(|item| item.1)
            .collect();

        let mut answer_inline = AnswerInlineQuery {
            inline_query_id: inline.id.to_owned(),
            results: cleaned_responses,
            is_personal: Some(true), // Everything is personal because of config
            ..Default::default()
        };

        // If the query was empty, display a help button to make it easy to get
        // started using the bot.
        if inline.query.is_empty() {
            answer_inline.switch_pm_text = Some("Help".to_string());
            answer_inline.switch_pm_parameter = Some("help".to_string());
        }

        // If we had a video that needed to be processed, replace the switch pm
        // parameters to go and process that video.
        if let Some(video) = has_video {
            answer_inline.switch_pm_text = Some("Process video".to_string());
            answer_inline.switch_pm_parameter = Some(video.id.to_owned());

            // Do not cache! We quickly want to change this result after
            // processing is completed.
            answer_inline.cache_time = Some(0);
        }

        handler
            .make_request(&answer_inline)
            .await
            .context("unable to answer inline query")?;

        Ok(Completed)
    }
}

/// Convert a [PostInfo] struct into an InlineQueryResult.
///
/// It adds an inline keyboard for the direct link and source if available.
async fn process_result(
    handler: &crate::MessageHandler,
    result: &PostInfo,
    from: &User,
) -> anyhow::Result<Option<Vec<(ResultType, InlineQueryResult)>>> {
    let (direct, source) = handler
        .get_fluent_bundle(from.language_code.as_deref(), |bundle| {
            (
                get_message(&bundle, "inline-direct", None).unwrap(),
                get_message(&bundle, "inline-source", None).unwrap(),
            )
        })
        .await;

    let mut row = vec![InlineKeyboardButton {
        text: direct,
        url: Some(result.url.clone()),
        callback_data: None,
        ..Default::default()
    }];

    if let Some(source_link) = &result.source_link {
        let use_name = use_source_name(&handler.conn, from.id)
            .await
            .unwrap_or(false);

        let text = if use_name {
            result.site_name.to_string()
        } else {
            source
        };

        row.push(InlineKeyboardButton {
            text,
            url: Some(source_link.clone()),
            callback_data: None,
            ..Default::default()
        })
    }

    let keyboard = InlineKeyboardMarkup {
        inline_keyboard: vec![row],
    };

    let thumb_url = result.thumb.clone().unwrap_or_else(|| result.url.clone());

    match result.file_type.as_ref() {
        "png" | "jpeg" | "jpg" => Ok(Some(
            build_image_result(&handler, &result, thumb_url, &keyboard).await?,
        )),
        "webm" => {
            let source = match &result.source_link {
                Some(link) => link.to_owned(),
                None => result.url.clone(),
            };

            let results = build_webm_result(&handler.conn, &result, thumb_url, &keyboard, &source)
                .await
                .expect("unable to process webm results");

            Ok(Some(results))
        }
        "gif" => Ok(Some(build_gif_result(&result, thumb_url, &keyboard))),
        other => {
            tracing::warn!(file_type = other, "got unusable type");
            Ok(None)
        }
    }
}

async fn build_image_result(
    handler: &crate::MessageHandler,
    result: &crate::sites::PostInfo,
    thumb_url: String,
    keyboard: &InlineKeyboardMarkup,
) -> anyhow::Result<Vec<(ResultType, InlineQueryResult)>> {
    let mut result = result.to_owned();
    result.thumb = Some(thumb_url);

    let conn = handler
        .conn
        .check_out()
        .await
        .context("unable to check out database")?;

    // Check if we should cache images, top priority option. If not, check if
    // we should size them (this should probably always be true). And finally,
    // if no other options are set we should pass the result through untouched.
    let result = if handler.config.cache_images.unwrap_or(false) {
        cache_post(
            &conn,
            &handler.s3,
            &handler.config.s3_bucket,
            &handler.config.s3_url,
            &result,
        )
        .await?
    } else if handler.config.size_images.unwrap_or(false) {
        size_post(&result).await?
    } else {
        result
    };

    let mut photo = InlineQueryResult::photo(
        generate_id(),
        result.url.to_owned(),
        result.thumb.clone().unwrap(),
    );
    photo.reply_markup = Some(keyboard.clone());

    if let Some(dims) = result.image_dimensions {
        if let InlineQueryType::Photo(ref mut photo) = photo.content {
            photo.photo_width = Some(dims.0);
            photo.photo_height = Some(dims.1);
        }
    }

    let mut results = vec![(ResultType::Ready, photo)];

    if let Some(message) = &result.extra_caption {
        let mut photo = InlineQueryResult::photo(generate_id(), result.url, result.thumb.unwrap());
        photo.reply_markup = Some(keyboard.clone());

        if let InlineQueryType::Photo(ref mut photo) = photo.content {
            photo.caption = Some(message.to_string());

            if let Some(dims) = result.image_dimensions {
                photo.photo_width = Some(dims.0);
                photo.photo_height = Some(dims.1);
            }
        }

        results.push((ResultType::Ready, photo));
    };

    Ok(results)
}

async fn build_webm_result(
    conn: &quaint::pooled::Quaint,
    result: &crate::sites::PostInfo,
    thumb_url: String,
    keyboard: &InlineKeyboardMarkup,
    source_link: &str,
) -> anyhow::Result<Vec<(ResultType, InlineQueryResult)>> {
    let conn = conn.check_out().await?;

    let video = match Video::lookup_url(&conn, &result.url).await? {
        None => {
            let id = Video::insert_url(&conn, &result.url, &source_link).await?;
            return Ok(vec![(
                ResultType::VideoToBeProcessed,
                InlineQueryResult::article(format!("process-{}", id), "".into(), "".into()),
            )]);
        }
        Some(video) if !video.processed => {
            return Ok(vec![(
                ResultType::VideoToBeProcessed,
                InlineQueryResult::article(format!("process-{}", video.id), "".into(), "".into()),
            )]);
        }
        Some(video) => video,
    };

    let (full_url, thumb_url) = (video.mp4_url.unwrap(), thumb_url);

    let mut video = InlineQueryResult::video(
        generate_id(),
        full_url.to_owned(),
        "video/mp4".to_owned(),
        thumb_url.to_owned(),
        result.url.clone(),
    );
    video.reply_markup = Some(keyboard.clone());

    let mut results = vec![(ResultType::Ready, video)];

    if let Some(message) = &result.extra_caption {
        let mut video = InlineQueryResult::video(
            generate_id(),
            full_url,
            "video/mp4".to_owned(),
            thumb_url,
            result.url.clone(),
        );
        video.reply_markup = Some(keyboard.clone());

        if let InlineQueryType::Video(ref mut result) = video.content {
            result.caption = Some(message.to_string());
        }

        results.push((ResultType::Ready, video));
    };

    Ok(results)
}

fn build_gif_result(
    result: &crate::sites::PostInfo,
    thumb_url: String,
    keyboard: &InlineKeyboardMarkup,
) -> Vec<(ResultType, InlineQueryResult)> {
    let (full_url, thumb_url) = (result.url.clone(), thumb_url);

    let mut gif = InlineQueryResult::gif(generate_id(), full_url.to_owned(), thumb_url.to_owned());
    gif.reply_markup = Some(keyboard.clone());

    let mut results = vec![(ResultType::Ready, gif)];

    if let Some(message) = &result.extra_caption {
        let mut gif = InlineQueryResult::gif(generate_id(), full_url, thumb_url);
        gif.reply_markup = Some(keyboard.clone());

        if let InlineQueryType::GIF(ref mut result) = gif.content {
            result.caption = Some(message.to_string());
        }

        results.push((ResultType::Ready, gif));
    };

    results
}
