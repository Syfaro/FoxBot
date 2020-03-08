use super::Status::*;
use crate::needs_field;
use async_trait::async_trait;
use sentry::integrations::failure::capture_fail;
use telegram::*;
use tokio01::runtime::current_thread::block_on_all;

use crate::utils::get_message;

pub struct TextHandler;

#[async_trait]
impl super::Handler for TextHandler {
    fn name(&self) -> &'static str {
        "text"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<super::Status, failure::Error> {
        let message = needs_field!(update, message);
        let text = needs_field!(message, text);

        let now = std::time::Instant::now();

        let from = message.from.clone().unwrap();

        if text.trim().parse::<i32>().is_err() {
            tracing::trace!("got text that wasn't oob, ignoring");
            return Ok(Ignored);
        }

        tracing::trace!("checking if message was Twitter code");

        let data: (String, String) = {
            let lock = handler.db.read().await;

            match lock.get(&format!("authenticate:{}", from.id)) {
                Some(data) => data,
                None => return Ok(Completed),
            }
        };

        tracing::trace!("we had waiting Twitter code");

        let request_token = egg_mode::KeyPair::new(data.0, data.1);
        let con_token = egg_mode::KeyPair::new(
            handler.config.twitter_consumer_key.clone(),
            handler.config.twitter_consumer_secret.clone(),
        );

        let token = match block_on_all(egg_mode::access_token(con_token, &request_token, text)) {
            Err(e) => {
                tracing::warn!("user was unable to verify OOB: {:?}", e);

                handler
                    .report_error(
                        &message,
                        Some(vec![("command", "twitter_auth".to_string())]),
                        || capture_fail(&e),
                    )
                    .await;
                return Ok(Completed);
            }
            Ok(token) => token,
        };

        tracing::trace!("got token");

        let access = match token.0 {
            egg_mode::Token::Access { access, .. } => access,
            _ => unimplemented!(),
        };

        tracing::trace!("got access token");

        {
            let mut lock = handler.db.write().await;

            if let Err(e) = lock.set(
                &format!("credentials:{}", from.id),
                &(access.key, access.secret),
            ) {
                tracing::warn!("unable to save user credentials: {:?}", e);

                handler
                    .report_error(
                        &message,
                        Some(vec![("command", "twitter_auth".to_string())]),
                        || {
                            sentry::integrations::failure::capture_error(&format_err!(
                                "Unable to save to Twitter database: {}",
                                e
                            ))
                        },
                    )
                    .await;
                return Ok(Completed);
            }
        }

        let mut args = fluent::FluentArgs::new();
        args.insert("userName", fluent::FluentValue::from(token.2));

        let text = handler
            .get_fluent_bundle(from.language_code.as_deref(), |bundle| {
                get_message(&bundle, "twitter-welcome", Some(args)).unwrap()
            })
            .await;

        let message = SendMessage {
            chat_id: from.id.into(),
            text,
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        handler.bot.make_request(&message).await?;

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "twitter")
            .add_tag("type", "added")
            .add_field("duration", now.elapsed().as_millis() as i64);

        let _ = handler.influx.query(&point).await;

        Ok(Completed)
    }
}
