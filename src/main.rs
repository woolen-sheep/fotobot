use anyhow::{Context, Result, anyhow};
use grammers_client::{
    Client as GramClient,
    types::{Message as GramMessage, Peer as GramPeer},
};
use grammers_mtsender::SenderPool;
use grammers_session::storages::SqliteSession;
use log::LevelFilter;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use teloxide::{
    dispatching::{Dispatcher, UpdateFilterExt},
    prelude::*,
    types::{ChatId, FileMeta, InputFile, MediaKind, Message, MessageKind, Update},
};
use tokio::{fs, task};

mod exif;

rust_i18n::i18n!("locales");

const MAX_INLINE_SIZE: u64 = 20 * 1024 * 1024; // 20 MB telegram download limit.

enum ImageSelection {
    Inline {
        file_id: String,
        media_kind: ReceivedImage,
    },
    TooLarge {
        file_id: String,
        media_kind: ReceivedImage,
        size: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut logger = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("read=trace,http-range=debug"),
    );
    logger.filter_module(env!("CARGO_PKG_NAME"), LevelFilter::Info);
    logger.try_init().ok();

    log::info!("Starting Telegram EXIF bot...");

    let bot_token = bot_token_from_env()?;
    let bot = Bot::new(bot_token.clone());
    let extra_client = init_extra_client(&bot_token).await?;

    Dispatcher::builder(bot, Update::filter_message().endpoint(handle_message))
        .dependencies(dptree::deps![extra_client])
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn handle_message(
    bot: Bot,
    extra_client: GramClient,
    msg: Message,
) -> Result<(), teloxide::RequestError> {
    let chat_id = msg.chat.id;
    let message_id = msg.id.0;
    let username = msg.chat.username().map(|name| name.to_string());
    let user_language = msg.from().and_then(|user| user.language_code.clone());
    let locale = locale_from_language_code(user_language.as_deref());

    log::info!(
        "username {}, language {}",
        msg.chat.username().unwrap_or("<unknown>"),
        user_language.as_deref().unwrap_or("<unknown>")
    );

    if let MessageKind::Common(common) = &msg.kind {
        if matches!(common.media_kind, MediaKind::Photo(_)) {
            bot.send_message(
                chat_id,
                rust_i18n::t!("messages.resend_document", locale = locale),
            )
            .await?;
            return Ok(());
        }
    }

    if let Some(selection) = image_file_id(&msg) {
        let processing_result = match selection {
            ImageSelection::Inline {
                file_id,
                media_kind,
            } => {
                process_image(
                    &bot,
                    chat_id,
                    &file_id,
                    media_kind,
                    user_language.as_deref(),
                )
                .await
            }
            ImageSelection::TooLarge {
                file_id,
                media_kind,
                size,
            } => {
                log::info!(
                    "Image is {size} bytes (> {MAX_INLINE_SIZE}) – using secondary client download"
                );
                process_large_image(
                    &bot,
                    &extra_client,
                    chat_id,
                    message_id,
                    &file_id,
                    media_kind,
                    username.as_deref(),
                    user_language.as_deref(),
                )
                .await
            }
        };

        if let Err(err) = processing_result {
            log::error!("Failed to process image: {err:?}");
            bot.send_message(
                chat_id,
                rust_i18n::t!("messages.process_error", locale = locale),
            )
            .await?;
        }
    } else {
        bot.send_message(
            chat_id,
            rust_i18n::t!("messages.request_image", locale = locale),
        )
        .await?;
    }

    Ok(())
}

async fn process_image(
    bot: &Bot,
    chat_id: ChatId,
    file_id: &str,
    media_kind: ReceivedImage,
    language_code: Option<&str>,
) -> Result<()> {
    let token = bot_token_from_env()?;

    let file = bot
        .get_file(file_id)
        .await
        .context("Failed to fetch file information from Telegram")?;

    let file_url = format!("https://api.telegram.org/file/bot{}/{}", token, file.path);

    let exif_report = {
        let url_for_task = file_url.clone();
        let accept_language = language_code.map(|code| code.to_string());
        task::spawn_blocking(move || {
            exif::summarize_exif(&url_for_task, accept_language.as_deref())
        })
        .await
        .context("Failed to join EXIF parsing task")?
        .context("Failed to parse EXIF data")?
    };

    let caption = enforce_caption_limit(exif_report);

    send_caption_for_media(bot, chat_id, file_id, media_kind, caption).await
}

async fn process_large_image(
    bot: &Bot,
    extra_client: &GramClient,
    chat_id: ChatId,
    message_id: i32,
    file_id: &str,
    media_kind: ReceivedImage,
    username: Option<&str>,
    language_code: Option<&str>,
) -> Result<()> {
    let message = fetch_secondary_message(extra_client, chat_id, message_id, username)
        .await?
        .context("Secondary client did not return the requested message")?;

    let cache_dir = Path::new("cache");
    fs::create_dir_all(cache_dir)
        .await
        .context("Failed to ensure cache directory exists")?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before UNIX_EPOCH")?
        .as_millis();

    let extension = match media_kind {
        ReceivedImage::Document => "bin",
    };

    let local_path = cache_dir.join(format!(
        "tmp-{}-{}-{}.{}",
        chat_id.0, message_id, timestamp, extension
    ));

    let downloaded = message
        .download_media_header()
        .await
        .context("Failed to download large media with secondary client")?;

    let reader = downloaded.ok_or_else(|| {
        anyhow!(
            "Secondary client reported no downloadable media for message {}",
            message.id()
        )
    })?;

    let cursor = reader.into_inner();
    let bytes = cursor.into_inner();

    fs::write(&local_path, &bytes)
        .await
        .context("Failed to persist downloaded media to cache")?;

    let path_for_task = local_path.clone();
    let accept_language = language_code.map(|code| code.to_string());
    let exif_report = task::spawn_blocking(move || {
        exif::summarize_exif_from_file(&path_for_task, accept_language.as_deref())
    })
    .await
    .context("Failed to join EXIF parsing task for local file")??;

    let caption = enforce_caption_limit(exif_report);

    send_caption_for_media(bot, chat_id, file_id, media_kind, caption).await
}

fn bot_token_from_env() -> Result<String> {
    for key in [
        "TELEGRAM_BOT_TOKEN",
        "BOT_TOKEN",
        "TELEGRAM_TOKEN",
        "TELOXIDE_TOKEN",
    ] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                return Ok(value);
            }
        }
    }
    Err(anyhow!("Telegram bot token not found in environment"))
}

fn session_path_from_env() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("GRAMMERS_SESSION_FILE") {
        if !path.trim().is_empty() {
            let path = PathBuf::from(path);
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!(
                            "Failed to create session directory at `{}`",
                            parent.display()
                        )
                    })?;
                }
            }
            return Ok(path);
        }
    }

    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .map(PathBuf::from)
        .ok_or_else(|| {
            anyhow!("Unable to determine home directory. Set `GRAMMERS_SESSION_FILE` to override.")
        })?;

    let config_dir = home.join(".config").join("fotobot");
    std::fs::create_dir_all(&config_dir).with_context(|| {
        format!(
            "Failed to create session directory at `{}`",
            config_dir.display()
        )
    })?;

    Ok(config_dir.join("fotobot.session"))
}

async fn init_extra_client(bot_token: &str) -> Result<GramClient> {
    let api_id: i32 = std::env::var("TG_ID")
        .context("`TG_ID` environment variable is required for grammers client initialization")?
        .parse()
        .context("`TG_ID` must be a valid 32-bit integer")?;

    let api_hash = std::env::var("TG_HASH")
        .context("`TG_HASH` environment variable is required for grammers client initialization")?;

    let session_path = session_path_from_env()?;

    let session = Arc::new(SqliteSession::open(&session_path).with_context(|| {
        format!(
            "Failed to open session file at `{}`",
            session_path.display()
        )
    })?);

    let pool = SenderPool::new(Arc::clone(&session), api_id);
    let client = GramClient::new(&pool);
    let SenderPool {
        runner,
        updates: _,
        handle: _,
    } = pool;

    tokio::spawn(async move {
        runner.run().await;
        log::info!("Grammers sender runner stopped.");
    });

    if !client.is_authorized().await? {
        log::info!("Signing in secondary Telegram client...");
        client
            .bot_sign_in(bot_token, &api_hash)
            .await
            .context("Failed to sign in the secondary Telegram client")?;
        log::info!("Secondary Telegram client signed in.");
    }

    Ok(client)
}

async fn fetch_secondary_message(
    extra_client: &GramClient,
    chat_id: ChatId,
    message_id: i32,
    username: Option<&str>,
) -> Result<Option<GramMessage>> {
    let peer = resolve_peer_for_chat(extra_client, chat_id, username).await?;
    let messages = extra_client.get_messages_by_id(peer, &[message_id]).await?;

    Ok(messages.into_iter().next().flatten())
}

async fn resolve_peer_for_chat(
    extra_client: &GramClient,
    chat_id: ChatId,
    username: Option<&str>,
) -> Result<GramPeer> {
    if let Some(username) = username {
        match extra_client.resolve_username(username).await? {
            Some(peer) => {
                log::info!(
                    "Resolved username {} to peer {}",
                    username,
                    peer.id().bot_api_dialog_id()
                );
                return Ok(peer);
            }
            None => {
                log::info!("Secondary client could not resolve username {}", username);
            }
        }
    }

    let mut dialogs = extra_client.iter_dialogs();
    while let Some(dialog) = dialogs.next().await? {
        let peer = dialog.peer().clone();
        if peer.id().bot_api_dialog_id() == chat_id.0 {
            return Ok(peer);
        }
    }

    Err(anyhow!(
        "Peer with chat_id {} not found in secondary client dialogs",
        chat_id.0
    ))
}

#[derive(Clone, Copy)]
enum ReceivedImage {
    Document,
}

fn locale_from_language_code(language_code: Option<&str>) -> &'static str {
    let Some(code) = language_code
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return "en";
    };

    let normalized = code.replace('_', "-").to_ascii_lowercase();

    if is_simplified_chinese_code(&normalized) {
        "zh-CN"
    } else {
        "en"
    }
}

fn is_simplified_chinese_code(code: &str) -> bool {
    matches!(code,"zh") || code.starts_with("zh-")
}

fn image_file_id(msg: &Message) -> Option<ImageSelection> {
    if let MessageKind::Common(common) = &msg.kind {
        match &common.media_kind {
            MediaKind::Photo(_) => None,
            MediaKind::Document(doc) => {
                let is_image = doc
                    .document
                    .mime_type
                    .as_ref()
                    .map(|mime| mime.essence_str().starts_with("image/"))
                    .unwrap_or(false);

                if !is_image {
                    return None;
                }

                let file_id = doc.document.file.id.clone();
                let size = document_size_bytes(&doc.document);
                Some(select_image(file_id, ReceivedImage::Document, size))
            }
            _ => None,
        }
    } else {
        None
    }
}

fn select_image(file_id: String, media_kind: ReceivedImage, size: Option<u64>) -> ImageSelection {
    if let Some(size) = size {
        if size > MAX_INLINE_SIZE {
            return ImageSelection::TooLarge {
                file_id,
                media_kind,
                size,
            };
        }
    }

    ImageSelection::Inline {
        file_id,
        media_kind,
    }
}

fn file_meta_size_bytes(meta: &FileMeta) -> Option<u64> {
    Some(meta.size as u64)
}

fn document_size_bytes(document: &teloxide::types::Document) -> Option<u64> {
    file_meta_size_bytes(&document.file)
}

fn enforce_caption_limit(mut caption: String) -> String {
    const CAPTION_LIMIT: usize = 1000; // stay below Telegram's 1024 char limit.
    if caption.len() > CAPTION_LIMIT {
        caption.truncate(CAPTION_LIMIT);
        caption.push_str("... [truncated]");
    }
    caption
}

async fn send_caption_for_media(
    bot: &Bot,
    chat_id: ChatId,
    file_id: &str,
    media_kind: ReceivedImage,
    caption: String,
) -> Result<()> {
    match media_kind {
        ReceivedImage::Document => {
            bot.send_document(chat_id, InputFile::file_id(file_id.to_owned()))
                .caption(caption)
                .await
                .context("Failed to send EXIF summary document")?;
        }
    }

    Ok(())
}
