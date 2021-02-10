use anyhow::Context;
use async_trait::async_trait;
use redis::AsyncCommands;
use tgbotapi::{requests::*, *};

use super::Status::{self, *};
use crate::utils::{extract_links, find_best_photo, get_message, link_was_seen, match_image};
use crate::{needs_field, potential_return};

pub struct ChannelPhotoHandler;

#[async_trait]
impl super::Handler for ChannelPhotoHandler {
    fn name(&self) -> &'static str {
        "channel"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<super::Status> {
        // Ensure we have a channel_post Message and a photo within.
        let message = needs_field!(update, channel_post);
        let sizes = needs_field!(&message, photo);

        potential_return!(initial_filter(&message));

        let file = find_best_photo(&sizes).unwrap();
        let mut matches = match_image(&handler.bot, &handler.conn, &handler.fapi, &file)
            .await
            .context("unable to get matches")?;

        // Only keep matches with a distance of 3 or less
        matches.retain(|m| m.distance.unwrap() <= 3);

        let first = match matches.first() {
            Some(first) => first,
            _ => return Ok(Completed),
        };

        let sites = handler.sites.lock().await;

        // If this link was already in the message, we can ignore it.
        if link_was_seen(&sites, &extract_links(&message), &first.url) {
            return Ok(Completed);
        }

        drop(sites);

        potential_return!(filter_existing(&handler.redis, &message, &matches).await);

        // If this photo was part of a media group, we should set a caption on
        // the image because we can't make an inline keyboard on it.
        let resp = if message.media_group_id.is_some() {
            let edit_caption_markup = edited_caption(&message, first.url());

            handler.make_request(&edit_caption_markup).await
        // Not a media group, we should create an inline keyboard.
        } else {
            let text = handler
                .get_fluent_bundle(None, |bundle| {
                    get_message(&bundle, "inline-source", None).unwrap()
                })
                .await;

            let edit_reply_markup = edited_markup(&message, text, first.url());

            handler.make_request(&edit_reply_markup).await
        };

        check_response(resp)
    }
}

/// Filter updates to ignore any non-channel type messages and flag completed
/// for forwarded messages (can't edit) or messages with reply markup
/// (likely from a bot and unable to be edited).
#[allow(clippy::unnecessary_wraps)]
fn initial_filter(message: &tgbotapi::Message) -> anyhow::Result<Option<Status>> {
    // We only want messages from channels. I think this is always true
    // because this came from a channel_post.
    if message.chat.chat_type != ChatType::Channel {
        return Ok(Some(Ignored));
    }

    // We can't edit forwarded messages, so we have to ignore.
    if message.forward_date.is_some() {
        return Ok(Some(Completed));
    }

    // See comment on [`check_response`], but this likely means we can't edit
    // this message. Might be worth testing more in the future, but for now, we
    // should just ignore it.
    if message.reply_markup.is_some() {
        return Ok(Some(Completed));
    }

    Ok(None)
}

/// Construct an editMessageCaption request for a message with given caption.
fn edited_caption(message: &tgbotapi::Message, caption: String) -> EditMessageCaption {
    EditMessageCaption {
        chat_id: message.chat_id(),
        message_id: Some(message.message_id),
        caption: Some(caption),
        ..Default::default()
    }
}

/// Construct an editMessageReplyMarkup request for a message with a given
/// button text and URL.
fn edited_markup(message: &tgbotapi::Message, text: String, url: String) -> EditMessageReplyMarkup {
    let markup = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text,
            url: Some(url),
            ..Default::default()
        }]],
    };

    EditMessageReplyMarkup {
        chat_id: message.chat_id(),
        message_id: Some(message.message_id),
        reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(markup)),
        ..Default::default()
    }
}

/// Check the response to see how we should flag this handler's result. We're
/// ignoring bad requests here because it seems like there's nothing we can do
/// to prevent them.
fn check_response(
    resp: Result<tgbotapi::requests::MessageOrBool, tgbotapi::Error>,
) -> anyhow::Result<Status> {
    match resp {
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
            ..
        })) => Ok(Completed),
        Ok(_) => Ok(Completed),
        Err(e) => Err(e).context("unable to update channel message"),
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
async fn filter_existing(
    conn: &redis::aio::ConnectionManager,
    message: &tgbotapi::Message,
    matches: &[fuzzysearch::File],
) -> anyhow::Result<Option<Status>> {
    if let Some(group_id) = &message.media_group_id {
        let key = format!("group-sources:{}", group_id);

        let mut urls = matches.iter().map(|m| m.url()).collect::<Vec<_>>();
        urls.sort();
        urls.dedup();
        let source_count = urls.len();

        let mut conn = conn.clone();
        let added_links: usize = conn.sadd(&key, urls).await?;
        conn.expire(&key, 300).await?;

        if source_count > added_links {
            tracing::debug!(
                source_count,
                added_links,
                "media group already contained source"
            );
            return Ok(Some(Completed));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_initial_filter() {
        use super::initial_filter;
        use crate::handlers::Status::*;

        let message = tgbotapi::Message {
            chat: tgbotapi::Chat {
                chat_type: tgbotapi::ChatType::Private,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            initial_filter(&message).unwrap(),
            Some(Ignored),
            "should filter out non-channel updates"
        );

        let message = tgbotapi::Message {
            chat: tgbotapi::Chat {
                chat_type: tgbotapi::ChatType::Channel,
                ..Default::default()
            },
            forward_date: Some(0),
            ..Default::default()
        };
        assert_eq!(
            initial_filter(&message).unwrap(),
            Some(Completed),
            "should mark forwarded messages as complete"
        );

        let message = tgbotapi::Message {
            chat: tgbotapi::Chat {
                chat_type: tgbotapi::ChatType::Channel,
                ..Default::default()
            },
            reply_markup: Some(tgbotapi::InlineKeyboardMarkup {
                inline_keyboard: Default::default(),
            }),
            ..Default::default()
        };
        assert_eq!(
            initial_filter(&message).unwrap(),
            Some(Completed),
            "should mark messages with reply markup as complete"
        );

        let message = tgbotapi::Message {
            chat: tgbotapi::Chat {
                chat_type: tgbotapi::ChatType::Channel,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            initial_filter(&message).unwrap(),
            None,
            "should not mark typical channel messages"
        );
    }

    #[test]
    fn test_edited_caption() {
        use super::edited_caption;

        let message = tgbotapi::Message {
            message_id: 12345,
            chat: tgbotapi::Chat {
                id: 54321,
                ..Default::default()
            },
            ..Default::default()
        };
        let edit_request = edited_caption(&message, "caption".to_string());
        assert_eq!(
            edit_request.message_id,
            Some(12345),
            "message id should be set"
        );
        assert_eq!(
            edit_request.chat_id,
            tgbotapi::requests::ChatID::Identifier(54321),
            "chat id should be set"
        );
        assert_eq!(
            edit_request.caption,
            Some("caption".to_string()),
            "caption should be set"
        );
    }

    #[test]
    fn test_edited_markup() {
        use super::edited_markup;

        let message = tgbotapi::Message {
            message_id: 12345,
            chat: tgbotapi::Chat {
                id: 54321,
                ..Default::default()
            },
            ..Default::default()
        };
        let reply_request = edited_markup(&message, "text".to_string(), "url".to_string());
        assert_eq!(
            reply_request.message_id,
            Some(12345),
            "message id should be set"
        );
        assert_eq!(
            reply_request.chat_id,
            tgbotapi::requests::ChatID::Identifier(54321),
            "chat id should be set"
        );
        assert!(
            reply_request.reply_markup.is_some(),
            "reply markup should be set"
        );
        let markup = match reply_request.reply_markup.unwrap() {
            tgbotapi::requests::ReplyMarkup::InlineKeyboardMarkup(kbd) => kbd,
            _ => panic!("reply markup should be inline keyboard markup"),
        };
        assert_eq!(
            markup.inline_keyboard.len(),
            1,
            "keyboard should have one row"
        );
        assert_eq!(
            markup.inline_keyboard[0].len(),
            1,
            "keyboard should have one column"
        );
        let btn = &markup.inline_keyboard[0][0];
        assert_eq!(btn.text, "text".to_string(), "button text should be set");
        assert_eq!(btn.url, Some("url".to_string()), "button url should be set");
    }

    #[test]
    fn test_check_response() {
        use super::check_response;
        use crate::handlers::Status::*;

        let resp = Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
            error_code: Some(123),
            description: None,
            parameters: None,
        }));
        let check = check_response(resp);
        assert!(
            check.is_err(),
            "unknown error code should be flagged as error"
        );

        let resp = Ok(tgbotapi::requests::MessageOrBool::Bool(true));
        let check = check_response(resp);
        assert!(check.is_ok(), "successful response should not be an error");
        assert_eq!(
            check.unwrap(),
            Completed,
            "successful response should be flagged as completed"
        );

        let resp = Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
            error_code: Some(400),
            description: None,
            parameters: None,
        }));
        let check = check_response(resp);
        assert!(check.is_ok(), "400 response should be flagged as ok");
        assert_eq!(
            check.unwrap(),
            Completed,
            "400 response should be flagged as completed"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_filter_existing() {
        use super::filter_existing;
        use crate::handlers::Status::*;

        let mut conn = crate::test_helpers::get_redis().await;
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

        let resp = filter_existing(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            None,
            "filtering with no results should have no status flag"
        );

        let resp = filter_existing(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            Some(Completed),
            "filtering with same media group id and source should flag completed"
        );

        let sources = vec![fuzzysearch::File {
            site_id: 456,
            site_info: site_info.clone(),
            ..Default::default()
        }];

        let resp = filter_existing(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            None,
            "filtering same group with new source should have no status flag"
        );

        let message = tgbotapi::Message {
            media_group_id: Some("test-group-2".to_string()),
            ..Default::default()
        };

        let resp = filter_existing(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            None,
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

        let resp = filter_existing(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            Some(Completed),
            "adding a new with an old source should set a completed flag"
        );
    }
}
