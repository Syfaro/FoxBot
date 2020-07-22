use super::Status::*;
use crate::needs_field;
use anyhow::Context;
use async_trait::async_trait;
use tgbotapi::{requests::*, *};

use crate::utils::{continuous_action, find_best_photo, get_message, match_image, sort_results};

pub struct PhotoHandler;

#[async_trait]
impl super::Handler for PhotoHandler {
    fn name(&self) -> &'static str {
        "photo"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<super::Status> {
        let message = needs_field!(update, message);
        let photos = needs_field!(message, photo);

        let now = std::time::Instant::now();

        if message.chat.chat_type != ChatType::Private {
            return Ok(Ignored);
        }

        let action = continuous_action(
            handler.bot.clone(),
            12,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
        );

        let best_photo = find_best_photo(&photos).unwrap();
        let mut matches =
            match_image(&handler.bot, &handler.conn, &handler.fapi, &best_photo).await?;
        sort_results(
            &handler.conn,
            message.from.as_ref().unwrap().id,
            &mut matches,
        )
        .await?;

        let first = match matches.get(0) {
            Some(item) => item,
            _ => {
                no_results(&handler, &message, now).await?;
                return Ok(Completed);
            }
        };

        let similar: Vec<&fuzzysearch::File> = matches
            .iter()
            .skip(1)
            .take_while(|m| m.distance.unwrap() == first.distance.unwrap())
            .collect();
        tracing::debug!(
            distance = first.distance.unwrap(),
            "discovered match distance"
        );

        let name = if first.distance.unwrap() < 5 {
            "reverse-good-result"
        } else {
            "reverse-bad-result"
        };

        let mut args = fluent::FluentArgs::new();
        args.insert(
            "distance",
            fluent::FluentValue::from(first.distance.unwrap()),
        );

        if similar.is_empty() {
            args.insert("link", fluent::FluentValue::from(first.url()));
        } else {
            let mut links = vec![format!("· {}", first.url())];
            links.extend(similar.iter().map(|s| format!("· {}", s.url())));
            let mut s = "\n".to_string();
            s.push_str(&links.join("\n"));
            args.insert("link", fluent::FluentValue::from(s));
        }

        let text = handler
            .get_fluent_bundle(
                message.from.as_ref().unwrap().language_code.as_deref(),
                |bundle| get_message(&bundle, name, Some(args)).unwrap(),
            )
            .await;

        drop(action);

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text,
            disable_web_page_preview: Some(first.distance.unwrap() > 5),
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        handler
            .make_request(&send_message)
            .await
            .context("unable to send photo source reply")?;

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "source")
            .add_tag("good", first.distance.unwrap() < 5)
            .add_field("matches", matches.len() as i64)
            .add_field("duration", now.elapsed().as_millis() as i64);

        let _ = handler.influx.query(&point).await;

        Ok(Completed)
    }
}

async fn no_results(
    handler: &crate::MessageHandler,
    message: &Message,
    start: std::time::Instant,
) -> anyhow::Result<()> {
    let text = handler
        .get_fluent_bundle(
            message.from.as_ref().unwrap().language_code.as_deref(),
            |bundle| get_message(&bundle, "reverse-no-results", None).unwrap(),
        )
        .await;

    let send_message = SendMessage {
        chat_id: message.chat_id(),
        text,
        reply_to_message_id: Some(message.message_id),
        ..Default::default()
    };

    handler
        .make_request(&send_message)
        .await
        .context("unable to send photo no results message")?;

    let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "source")
        .add_field("matches", 0)
        .add_field("duration", start.elapsed().as_millis() as i64);

    let _ = handler.influx.query(&point).await;

    Ok(())
}
