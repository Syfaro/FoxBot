use serde::{Deserialize, Serialize};

#[macro_use]
extern crate failure;

#[derive(Fail, Debug)]
pub enum Error {
    #[fail(display = "telegram error: {}", _0)]
    Telegram(TelegramError),
    #[fail(display = "json parsing error: {}", _0)]
    JSON(serde_json::Error),
    #[fail(display = "http error: {}", _0)]
    Request(reqwest::Error),
}

impl From<reqwest::Error> for Error {
    fn from(item: reqwest::Error) -> Error {
        Error::Request(item)
    }
}

impl From<serde_json::Error> for Error {
    fn from(item: serde_json::Error) -> Error {
        Error::JSON(item)
    }
}

impl From<TelegramError> for Error {
    fn from(item: TelegramError) -> Error {
        Error::Telegram(item)
    }
}

#[derive(Debug, Deserialize)]
pub struct TelegramError {
    /// A HTTP-style error code.
    pub error_code: Option<i32>,
    /// A human readable error description.
    pub description: Option<String>,
    /// Additional information about errors in the request.
    pub parameters: Option<ResponseParameters>,
}

impl std::fmt::Display for TelegramError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Telegram Error {}: {}",
            self.error_code.unwrap_or(-1),
            self.description
                .clone()
                .unwrap_or_else(|| "no description".to_string())
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct Response<T> {
    /// If the request was successful. If true, the result is available.
    /// If false, error contains information about what happened.
    pub ok: bool,
    #[serde(flatten)]
    pub error: TelegramError,

    /// The response data.
    pub result: Option<T>,
}

/// Allow for turning a Response into a more usable Result type.
impl<T> Into<Result<T, TelegramError>> for Response<T> {
    fn into(self) -> Result<T, TelegramError> {
        match self.result {
            Some(result) if self.ok => Ok(result),
            _ => Err(self.error),
        }
    }
}

impl<T> Into<Result<T, Error>> for Response<T> {
    fn into(self) -> Result<T, Error> {
        match self.result {
            Some(result) if self.ok => Ok(result),
            _ => Err(Error::Telegram(self.error)),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i32,
    pub message: Option<Message>,
    pub inline_query: Option<InlineQuery>,
    pub chosen_inline_result: Option<ChosenInlineResult>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub id: i32,
    pub is_bot: bool,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub language_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MessageEntity {
    #[serde(rename = "type")]
    pub entity_type: MessageEntityType,
    pub offset: i32,
    pub length: i32,
    pub url: Option<String>,
    pub user: Option<User>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageEntityType {
    Mention,
    Hashtag,
    Cashtag,
    BotCommand,
    #[serde(rename = "url")]
    URL,
    Email,
    PhoneNumber,
    Bold,
    Italic,
    Code,
    Pre,
    TextLink,
    TextMention,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Message {
    pub message_id: i32,
    pub from: Option<User>,
    pub date: i64,
    pub chat: Chat,
    pub forward_from: Option<User>,
    pub forward_from_chat: Option<Chat>,
    pub forward_from_message_id: Option<i32>,
    pub forward_signature: Option<String>,
    pub forward_sender_name: Option<String>,
    pub forward_date: Option<i64>,
    pub reply_to_message: Option<Box<Message>>,
    pub edit_date: Option<i64>,
    pub media_group_id: Option<String>,
    pub author_signature: Option<String>,
    pub text: Option<String>,
    pub entities: Option<Vec<MessageEntity>>,
    pub photo: Option<Vec<PhotoSize>>,
    pub caption: Option<String>,
    pub new_chat_members: Option<Vec<User>>,
    pub left_chat_member: Option<User>,
    pub new_chat_title: Option<String>,
    pub migrate_to_chat_id: Option<i64>,
    pub migrate_from_chat_id: Option<i64>,
    pub reply_markup: Option<InlineKeyboardMarkup>,
}

impl Message {
    pub fn chat_id(&self) -> ChatID {
        ChatID::Identifier(self.chat.id)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub width: i32,
    pub height: i32,
    pub file_size: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub data: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseParameters {
    pub migrate_to_chat_id: Option<i64>,
    pub retry_after: Option<i32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InlineQuery {
    pub id: String,
    pub from: User,
    pub query: String,
    pub offset: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChosenInlineResult {
    pub result_id: String,
    pub from: User,
    pub inline_message_id: Option<String>,
    pub query: String,
}

/// A trait for all Telegram requests. It has as many default methods as
/// possible but still requires some additions.
pub trait TelegramRequest: serde::Serialize {
    /// Response is the type used when Deserializing Telegram's result field.
    ///
    /// For convenience of debugging, it must implement [Debug](std::fmt::Debug).
    type Response: serde::de::DeserializeOwned + std::fmt::Debug;

    /// Endpoint to use for the request.
    fn endpoint(&self) -> &str;

    /// A JSON-compatible serialization of the data to send with the request.
    /// The default works for most methods.
    fn values(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(&self)
    }

    /// Files that are sent with the request.
    fn files(&self) -> Option<Vec<(String, reqwest::multipart::Part)>> {
        None
    }
}

/// ChatID represents a possible type of value for requests.
#[derive(Serialize, Debug)]
#[serde(untagged)]
pub enum ChatID {
    /// A chat's numeric ID.
    Identifier(i64),
    /// A username for a channel.
    Username(String),
}

impl From<Message> for ChatID {
    fn from(item: Message) -> Self {
        ChatID::Identifier(item.chat.id)
    }
}

impl From<i64> for ChatID {
    fn from(item: i64) -> Self {
        ChatID::Identifier(item)
    }
}

impl From<i32> for ChatID {
    fn from(item: i32) -> Self {
        ChatID::Identifier(item as i64)
    }
}

impl From<String> for ChatID {
    fn from(item: String) -> Self {
        ChatID::Username(item)
    }
}

impl From<&str> for ChatID {
    fn from(item: &str) -> Self {
        ChatID::Username(item.to_string())
    }
}

impl Default for ChatID {
    fn default() -> Self {
        ChatID::Identifier(0)
    }
}

/// GetMe is a request that returns [User] information for the current bot.
#[derive(Serialize, Debug)]
pub struct GetMe;

impl TelegramRequest for GetMe {
    type Response = User;

    fn endpoint(&self) -> &str {
        "getMe"
    }
}

/// GetUpdates is a request that returns any available [Updates](Update).
#[derive(Serialize, Default, Debug)]
pub struct GetUpdates {
    /// ID for the first update to return. This must be set to one higher
    /// than previous IDs in order to confirm previous updates and clear them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i32>,
    /// Maximum number of [Updates](Update) to retrieve. May be set 1-100,
    /// defaults to 100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
    /// Number of seconds for long polling. This should be set to a reasonable
    /// value in production to avoid unneeded requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<i32>,
    /// Which update types to receive. May be set to any available types.
    /// * `message`
    /// * `edited_message`
    /// * `channel_post`
    /// * `edited_channel_post`
    /// * `inline_query`
    /// * `chosen_inline_result`
    /// * `callback_query`
    /// * `shipping_query`
    /// * `pre_checkout_query`
    /// * `poll`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_updates: Option<Vec<String>>,
}

impl TelegramRequest for GetUpdates {
    type Response = Vec<Update>;

    fn endpoint(&self) -> &str {
        "getUpdates"
    }
}

/// ForceReply allows you to default users to replying to your message.
#[derive(Serialize, Debug)]
pub struct ForceReply {
    /// This must be set to `true` to operate correctly.
    pub force_reply: bool,
    /// If only the user you are mentioning or replying to should be defaulted
    /// to replying to your message, or if it should default to replying for all
    /// members of the chat.
    pub selective: bool,
}

impl ForceReply {
    /// Create a [ForceReply] with selectivity.
    pub fn selective() -> Self {
        Self {
            force_reply: true,
            selective: true,
        }
    }
}

impl Default for ForceReply {
    /// Create a [ForceReply] without selectivity.
    fn default() -> Self {
        Self {
            force_reply: true,
            selective: false,
        }
    }
}

/// ReplyMarkup is additional data sent with a [Message] to enhance the bot
/// user experience.
///
/// You may add one of the following:
/// * [InlineKeyboardMarkup]
/// * <s>ReplyKeyboardMarkup</s> // TODO
/// * <s>ReplyKeyboardRemove</s> // TODO
/// * [ForceReply]
#[derive(Serialize, Debug)]
#[serde(untagged)]
pub enum ReplyMarkup {
    InlineKeyboardMarkup(InlineKeyboardMarkup),
    ForceReply(ForceReply),
}

/// SendMessage sends a message.
#[derive(Serialize, Default, Debug)]
pub struct SendMessage {
    /// The ID of the chat to send a message to.
    pub chat_id: ChatID,
    /// The text of the message.
    pub text: String,
    /// The ID of the [Message] this Message is in reply to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i32>,
    /// The [ReplyMarkup], if desired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<ReplyMarkup>,
    /// If Telegram should not generate a web page preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_web_page_preview: Option<bool>,
}

impl TelegramRequest for SendMessage {
    type Response = Message;

    fn endpoint(&self) -> &str {
        "sendMessage"
    }
}

/// FileType is a possible file for a Telegram request.
#[derive(Serialize, Clone)]
#[serde(untagged)]
pub enum FileType {
    /// URL contains a String with the URL to pass to Telegram.
    URL(String),
    /// FileID is an ID about a file already on Telegram's servers.
    FileID(String),
    /// Attach is a specific type used to attach multiple files to a request.
    Attach(String),
    /// Bytes requires a filename in addition to the file bytes, which it
    /// then uploads to Telegram.
    Bytes(String, Vec<u8>),
    /// Missing means that a file should have been specified but has not.
    Missing,
}

impl Default for FileType {
    fn default() -> Self {
        FileType::Missing
    }
}

impl std::fmt::Debug for FileType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            FileType::URL(url) => write!(f, "FileType URL: {}", url),
            FileType::FileID(file_id) => write!(f, "FileType FileID: {}", file_id),
            FileType::Attach(attach) => write!(f, "FileType Attach: {}", attach),
            FileType::Bytes(name, bytes) => {
                write!(f, "FileType Bytes: {} with len {}", name, bytes.len())
            }
            FileType::Missing => write!(f, "FileType Missing!!"),
        }
    }
}

impl FileType {
    /// Returns if this file is a type that gets uploaded to Telegram.
    /// Most types are simply passed through as strings.
    pub fn needs_upload(&self) -> bool {
        match self {
            FileType::Bytes(_, _) => true,
            _ => false,
        }
    }

    /// Get a multipart Part for the file.
    pub fn file(&self) -> Option<reqwest::multipart::Part> {
        match self {
            FileType::Bytes(file_name, bytes) => {
                Some(reqwest::multipart::Part::bytes(bytes.clone()).file_name(file_name.clone()))
            }
            _ => None,
        }
    }
}

/// SendPhoto sends a photo.
#[derive(Serialize, Debug, Default)]
pub struct SendPhoto {
    /// The ID of the chat to send a photo to.
    pub chat_id: ChatID,
    /// The file that makes up this photo.
    #[serde(skip_serializing_if = "FileType::needs_upload")]
    pub photo: FileType,
    /// A caption for the photo, if desired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<ReplyMarkup>,
}

impl TelegramRequest for SendPhoto {
    type Response = Message;

    fn endpoint(&self) -> &str {
        "sendPhoto"
    }

    fn files(&self) -> Option<Vec<(String, reqwest::multipart::Part)>> {
        // Check if the photo needs to be uploaded. If the photo does need to
        // be uploaded, we specify the field name and get the file. This unwrap
        // is safe because `needs_upload` only returns true when it exists.
        if self.photo.needs_upload() {
            Some(vec![("photo".to_owned(), self.photo.file().unwrap())])
        } else {
            None
        }
    }
}

/// GetFile retrieves information about a file.
///
/// This will not download the file! It only returns a [File] containing
/// the path which is needed to download the file. This returned ID lasts at
/// least one hour.
#[derive(Serialize, Debug)]
pub struct GetFile {
    /// The ID of the file to fetch.
    pub file_id: String,
}

#[derive(Deserialize, Debug)]
pub struct File {
    /// The ID for this file, specific to this bot.
    pub file_id: String,
    /// The size of the file, if known.
    pub file_size: Option<usize>,
    /// A path which is required to download the file. It is unclear
    /// when this would ever be `None`.
    pub file_path: Option<String>,
}

impl TelegramRequest for GetFile {
    type Response = File;

    fn endpoint(&self) -> &str {
        "getFile"
    }
}

/// SendChatAction allows you to indicate to users that the bot is performing
/// an action.
///
/// Actions last for 5 seconds or until a message is sent,
/// whichever comes first.
#[derive(Serialize, Debug)]
pub struct SendChatAction {
    /// The ID of the chat to send an action to.
    pub chat_id: ChatID,
    /// The action to indicate.
    pub action: ChatAction,
}

/// ChatAction is the action that the bot is indicating.
#[derive(Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ChatAction {
    Typing,
    UploadPhoto,
}

impl TelegramRequest for SendChatAction {
    type Response = bool;

    fn endpoint(&self) -> &str {
        "sendChatAction"
    }
}

#[derive(Serialize, Clone)]
pub struct InputMediaPhoto {
    #[serde(rename = "type")]
    pub media_type: String,
    pub media: FileType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl Default for InputMediaPhoto {
    fn default() -> Self {
        Self {
            media_type: "photo".to_string(),
            media: Default::default(),
            caption: None,
        }
    }
}

#[derive(Serialize, Clone)]
pub struct InputMediaVideo {
    #[serde(rename = "type")]
    pub media_type: String,
    pub media: FileType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum InputMedia {
    Photo(InputMediaPhoto),
    Video(InputMediaVideo),
}

impl InputMedia {
    /// Replaces the media within an InputMedia without caring about the type.
    fn update_media(&self, media: FileType) -> Self {
        match self {
            InputMedia::Photo(photo) => InputMedia::Photo(InputMediaPhoto {
                media,
                ..photo.clone()
            }),
            InputMedia::Video(video) => InputMedia::Video(InputMediaVideo {
                media,
                ..video.clone()
            }),
        }
    }
}

impl InputMedia {
    /// Get the file out of an InputMedia value.
    pub fn get_file(&self) -> &FileType {
        match self {
            InputMedia::Photo(photo) => &photo.media,
            InputMedia::Video(video) => &video.media,
        }
    }
}

#[derive(Serialize, Default)]
pub struct SendMediaGroup {
    pub chat_id: ChatID,
    #[serde(serialize_with = "clean_input_media")]
    pub media: Vec<InputMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_notification: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i32>,
}

/// Attempt to remove body for types that are getting uploaded.
/// It also converts files into attachments with names based on filenames.
///
/// This will panic if you attempt to upload a Missing file.
fn clean_input_media<S>(input_media: &[InputMedia], s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;

    let mut seq = s.serialize_seq(Some(input_media.len()))?;

    for elem in input_media {
        let file = elem.get_file();

        if let FileType::Missing = file {
            panic!("tried to uploading missing file");
        }

        if file.needs_upload() {
            let new_file = match file {
                FileType::Bytes(file_name, _) => {
                    FileType::Attach(format!("attach://{}", file_name))
                }
                _ => unimplemented!(),
            };

            let new_elem = elem.update_media(new_file);

            seq.serialize_element(&new_elem)?;
        } else {
            seq.serialize_element(elem)?;
        }
    }

    seq.end()
}

impl TelegramRequest for SendMediaGroup {
    type Response = Vec<Message>;

    fn endpoint(&self) -> &str {
        "sendMediaGroup"
    }

    fn files(&self) -> Option<Vec<(String, reqwest::multipart::Part)>> {
        if !self.media.iter().any(|item| item.get_file().needs_upload()) {
            return None;
        }

        let mut items = Vec::new();

        for item in &self.media {
            let file = item.get_file();

            let part = match file {
                FileType::Bytes(file_name, bytes) => {
                    let file =
                        reqwest::multipart::Part::bytes(bytes.clone()).file_name(file_name.clone());

                    (file_name.to_owned(), file)
                }
                _ => continue,
            };

            items.push(part);
        }

        Some(items)
    }
}

#[derive(Serialize, Default)]
pub struct AnswerInlineQuery {
    pub inline_query_id: String,
    pub results: Vec<InlineQueryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_time: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_personal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_pm_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_pm_parameter: Option<String>,
}

impl TelegramRequest for AnswerInlineQuery {
    type Response = bool;

    fn endpoint(&self) -> &str {
        "answerInlineQuery"
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct InlineKeyboardButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_inline_query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_inline_query_current_chat: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

#[derive(Serialize, Debug, Clone)]
pub struct InlineQueryResult {
    #[serde(rename = "type")]
    pub result_type: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<InlineKeyboardMarkup>,
    #[serde(flatten)]
    pub content: InlineQueryType,
}

#[derive(Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum InlineQueryType {
    Article(InlineQueryResultArticle),
    Photo(InlineQueryResultPhoto),
    GIF(InlineQueryResultGIF),
}

#[derive(Serialize, Debug, Clone)]
pub struct InlineQueryResultArticle {
    pub title: String,
    #[serde(flatten)]
    pub input_message_content: InputMessageType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
pub struct InlineQueryResultPhoto {
    pub photo_url: String,
    pub thumb_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
pub struct InlineQueryResultGIF {
    pub gif_url: String,
    pub thumb_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl InlineQueryResult {
    pub fn article(id: String, title: String, text: String) -> InlineQueryResult {
        InlineQueryResult {
            result_type: "article".into(),
            id,
            reply_markup: None,
            content: InlineQueryType::Article(InlineQueryResultArticle {
                title,
                description: None,
                input_message_content: InputMessageType::Text(InputMessageText {
                    message_text: text,
                    parse_mode: None,
                }),
            }),
        }
    }

    pub fn photo(id: String, photo_url: String, thumb_url: String) -> InlineQueryResult {
        InlineQueryResult {
            result_type: "photo".into(),
            id,
            reply_markup: None,
            content: InlineQueryType::Photo(InlineQueryResultPhoto {
                photo_url,
                thumb_url,
                caption: None,
            }),
        }
    }

    pub fn gif(id: String, gif_url: String, thumb_url: String) -> InlineQueryResult {
        InlineQueryResult {
            result_type: "gif".into(),
            id,
            reply_markup: None,
            content: InlineQueryType::GIF(InlineQueryResultGIF {
                gif_url,
                thumb_url,
                caption: None,
            }),
        }
    }
}

#[derive(Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum InputMessageType {
    Text(InputMessageText),
}

#[derive(Serialize, Debug, Clone)]
pub struct InputMessageText {
    pub message_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetWebhook {
    pub url: String,
}

impl TelegramRequest for SetWebhook {
    type Response = bool;

    fn endpoint(&self) -> &str {
        "setWebhook"
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DeleteWebhook;

impl TelegramRequest for DeleteWebhook {
    type Response = bool;

    fn endpoint(&self) -> &str {
        "deleteWebhook"
    }
}

pub struct Telegram {
    api_key: String,
    client: reqwest::Client,
}

impl Telegram {
    /// Create a new Telegram instance with a specified API key.
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder().build().unwrap();

        Self { api_key, client }
    }

    /// Make a request for a [TelegramRequest] item and parse the response
    /// into the requested output type if the request succeeded.
    pub async fn make_request<T>(&self, request: &T) -> Result<T::Response, Error>
    where
        T: TelegramRequest,
    {
        let endpoint = request.endpoint();

        let url = format!("https://api.telegram.org/bot{}/{}", self.api_key, endpoint);
        let values = request.values()?;

        log::debug!("Making request to {} with data {:?}", endpoint, values);

        let resp: Response<T::Response> = if let Some(files) = request.files() {
            // If our request has a file that needs to be uploaded, use
            // a multipart upload. Works by converting each JSON value into
            // a string and putting it into a field with the same name as the
            // original object.

            let mut form_values = serde_json::Map::new();
            form_values = values.as_object().unwrap_or_else(|| &form_values).clone();

            let form =
                form_values
                    .iter()
                    .fold(reqwest::multipart::Form::new(), |form, (name, value)| {
                        if let Ok(value) = serde_json::to_string(value) {
                            form.text(name.to_owned(), value)
                        } else {
                            log::warn!("Skipping field {} due to invalid data: {:?}", name, value);
                            form
                        }
                    });

            let form = files
                .into_iter()
                .fold(form, |form, (name, part)| form.part(name, part));

            let resp = self.client.post(&url).multipart(form).send().await?;

            log::trace!("Got response from {} with data {:?}", endpoint, resp);

            resp.json().await?
        } else {
            // No files to upload, use a JSON body in a POST request to the
            // requested endpoint.

            self.client
                .post(&url)
                .json(&values)
                .send()
                .await?
                .json()
                .await?
        };

        log::debug!("Got response from {} with data {:?}", endpoint, resp);

        resp.into()
    }

    /// Download a file from Telegram's servers.
    ///
    /// It requires a file path which can be obtained with [GetFile].
    pub async fn download_file(&self, file_path: String) -> Result<Vec<u8>, Error> {
        // Start by sending the request and fetching the Content-Length.
        // If the length is greater than 20MB, something is wrong and
        // we will ignore it. Allocate a Vec with the length, if available.
        // Return this Vec as it contains the file data.

        let url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.api_key, file_path
        );

        let bytes = self.client.get(&url).send().await?.bytes().await?;

        Ok(bytes.to_vec())
    }
}
