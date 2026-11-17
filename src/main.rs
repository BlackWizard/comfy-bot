// Rustc still passes `-Wl,-O1` to GNU-style linkers, and the LLD bundled with
// zig warns that this optimization setting is deprecated. The message is
// harmless, so keep musl cross-builds warning-free.
#![allow(linker_messages)]

mod aspect;
mod comfy;
mod menu;
mod mode;

use std::{collections::HashMap, env, error::Error, io::Cursor, sync::Arc, time::Duration};

use grammers_client::{
    Client, SenderPool,
    media::{Attribute, Media},
    message::{InputMessage, Message},
    sender::ConnectionParams,
    sender::UpdatesConfiguration,
    update::Update,
};
use grammers_session::{
    storages::SqliteSession,
    types::{PeerId, PeerRef},
};
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::{
    aspect::{AspectRatio, Resolution},
    comfy::{ComfyClient, ComfyError, DEFAULT_BASE_URL, GeneratedFile, InputImage},
    menu::{
        CLEAR_IMAGES_CALLBACK, MENU_CALLBACK, MODE_CALLBACK_PREFIX, RATIO_CALLBACK_PREFIX,
        RESOLUTION_CALLBACK_PREFIX,
    },
    mode::Mode,
};

#[derive(Clone)]
struct Selection {
    mode: Mode,
    ratio: AspectRatio,
    resolution: Resolution,
    images: Vec<InputImage>,
}

struct BotState {
    /// Selection for each chat. The next text message is treated as a prompt.
    pending: Mutex<HashMap<PeerId, Selection>>,
    comfy: ComfyClient,
}

const MAX_IMAGES: usize = 10;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    if let Err(err) = run().await {
        eprintln!("Ошибка: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
    let api_id: i32 = env::var("API_ID")
        .map_err(|_| "API_ID is not set (get it at https://my.telegram.org)")?
        .parse()
        .map_err(|_| "API_ID must be a number")?;
    let api_hash = env::var("API_HASH").map_err(|_| "API_HASH is not set")?;
    let token = env::var("BOT_TOKEN").map_err(|_| "BOT_TOKEN is not set")?;
    let session_path = env::var("SESSION_PATH").unwrap_or_else(|_| "bot.session".to_owned());
    let comfy_url = env::var("COMFY_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());

    let proxy_url = env::var("PROXY_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());

    if let Some(proxy) = &proxy_url {
        if !proxy.starts_with("socks5://") {
            return Err(
                "PROXY_URL must start with socks5:// (for example socks5://127.0.0.1:1080)".into(),
            );
        }
        info!("SOCKS5 proxy enabled");
    } else {
        info!("proxy disabled");
    }

    info!(%comfy_url, "ComfyUI endpoint configured");

    let state = Arc::new(BotState {
        pending: Mutex::new(HashMap::new()),
        comfy: ComfyClient::new(comfy_url),
    });

    let session = Arc::new(SqliteSession::open(&session_path).await?);
    let connection_params = ConnectionParams {
        proxy_url,
        ..Default::default()
    };
    let SenderPool {
        runner,
        handle,
        updates,
    } = SenderPool::with_configuration(session, api_id, connection_params);
    tokio::spawn(runner.run());

    let client = Client::new(handle);

    if client.is_authorized().await? {
        info!("reusing the saved session");
    } else {
        let user = client.bot_sign_in(&token, &api_hash).await?;
        info!(username = user.username(), "signed in as bot");
    }

    let mut updates = client
        .stream_updates(updates, UpdatesConfiguration::default())
        .await?;

    info!("bot is running — send /start in Telegram");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("shutting down");
                client.disconnect();
                break;
            }
            update = updates.next() => {
                match update {
                    Ok(update) => {
                        if let Err(err) = handle_update(&client, &state, update).await {
                            error!(%err, "failed to handle update");
                        }
                    }
                    Err(err) => {
                        error!(%err, "failed to receive update");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Handle a single Telegram update.
async fn handle_update(
    client: &Client,
    state: &BotState,
    update: Update,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match update {
        Update::NewMessage(message) if !message.outgoing() => {
            handle_message(client, state, message.into_inner()).await?;
        }
        Update::CallbackQuery(query) => {
            let data = std::str::from_utf8(query.data()).unwrap_or_default();
            let peer = query.peer_id();

            if let Some(code) = data.strip_prefix(MODE_CALLBACK_PREFIX) {
                match Mode::from_code(code) {
                    Some(mode) => {
                        let ratio = AspectRatio::default();
                        let resolution = Resolution::default();
                        let selection = Selection {
                            mode,
                            ratio,
                            resolution,
                            images: Vec::new(),
                        };

                        if mode.workflow().is_some() {
                            let old = state.pending.lock().await.insert(peer, selection);
                            if let Some(old) = old {
                                cleanup_images(old.images).await;
                            }
                        } else {
                            let old = state.pending.lock().await.remove(&peer);
                            if let Some(old) = old {
                                cleanup_images(old.images).await;
                            }
                        }

                        query
                            .answer()
                            .text(format!("Режим: {}", mode.label()))
                            .edit(
                                InputMessage::new()
                                    .text(menu::selected_text(mode, ratio, resolution, 0))
                                    .reply_markup(menu::selected_markup(mode, ratio, resolution)),
                            )
                            .await?;
                    }
                    None => {
                        query.answer().text("Неизвестный режим").send().await?;
                    }
                }
            } else if let Some(code) = data.strip_prefix(RATIO_CALLBACK_PREFIX) {
                let Some(ratio) = AspectRatio::from_code(code) else {
                    query
                        .answer()
                        .text("Неизвестное соотношение")
                        .send()
                        .await?;
                    return Ok(());
                };

                let selection = {
                    let mut pending = state.pending.lock().await;
                    match pending.get_mut(&peer) {
                        Some(selection) if selection.mode.workflow().is_some() => {
                            selection.ratio = ratio;
                            Some((selection.mode, selection.resolution, selection.images.len()))
                        }
                        _ => None,
                    }
                };

                match selection {
                    Some((mode, resolution, image_count)) => {
                        query
                            .answer()
                            .text(format!("Соотношение: {}", ratio.label()))
                            .edit(
                                InputMessage::new()
                                    .text(menu::selected_text(mode, ratio, resolution, image_count))
                                    .reply_markup(menu::selected_markup(mode, ratio, resolution)),
                            )
                            .await?;
                    }
                    None => {
                        query.answer().text("Сначала выберите режим").send().await?;
                    }
                }
            } else if let Some(code) = data.strip_prefix(RESOLUTION_CALLBACK_PREFIX) {
                let Some(resolution) = Resolution::from_code(code) else {
                    query.answer().text("Неизвестное разрешение").send().await?;
                    return Ok(());
                };

                let selection = {
                    let mut pending = state.pending.lock().await;
                    match pending.get_mut(&peer) {
                        Some(selection) if selection.mode.workflow().is_some() => {
                            selection.resolution = resolution;
                            Some((selection.mode, selection.ratio, selection.images.len()))
                        }
                        _ => None,
                    }
                };

                match selection {
                    Some((mode, ratio, image_count)) => {
                        query
                            .answer()
                            .text(format!("Разрешение: {}", resolution.description()))
                            .edit(
                                InputMessage::new()
                                    .text(menu::selected_text(mode, ratio, resolution, image_count))
                                    .reply_markup(menu::selected_markup(mode, ratio, resolution)),
                            )
                            .await?;
                    }
                    None => {
                        query.answer().text("Сначала выберите режим").send().await?;
                    }
                }
            } else if data == CLEAR_IMAGES_CALLBACK {
                let selection = {
                    let mut pending = state.pending.lock().await;
                    match pending.get_mut(&peer) {
                        Some(selection) if selection.mode.requires_images() => {
                            let images = std::mem::take(&mut selection.images);
                            Some((
                                images,
                                selection.mode,
                                selection.ratio,
                                selection.resolution,
                            ))
                        }
                        _ => None,
                    }
                };

                match selection {
                    Some((images, mode, ratio, resolution)) => {
                        cleanup_images(images).await;
                        query
                            .answer()
                            .text("Изображения очищены")
                            .edit(
                                InputMessage::new()
                                    .text(menu::selected_text(mode, ratio, resolution, 0))
                                    .reply_markup(menu::selected_markup(mode, ratio, resolution)),
                            )
                            .await?;
                    }
                    None => {
                        query.answer().text("Сначала выберите режим").send().await?;
                    }
                }
            } else if data == MENU_CALLBACK {
                let old = state.pending.lock().await.remove(&peer);
                if let Some(old) = old {
                    cleanup_images(old.images).await;
                }

                query
                    .answer()
                    .edit(
                        InputMessage::new()
                            .text(menu::menu_text())
                            .reply_markup(menu::menu_markup()),
                    )
                    .await?;
            } else {
                query.answer().send().await?;
            }
        }
        _ => {}
    }

    Ok(())
}

async fn handle_message(
    client: &Client,
    state: &BotState,
    message: Message,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let peer = message.peer_id();
    let text = message.text().trim().to_owned();

    if is_command(&text, "/start") || is_command(&text, "/menu") {
        let old = state.pending.lock().await.remove(&peer);
        if let Some(old) = old {
            cleanup_images(old.images).await;
        }

        message
            .respond(
                InputMessage::new()
                    .text(menu::menu_text())
                    .reply_markup(menu::menu_markup()),
            )
            .await?;
        return Ok(());
    }

    let pending_selection = state.pending.lock().await.get(&peer).cloned();
    let Some(mut selection) = pending_selection else {
        if !text.is_empty() {
            message
                .respond("Чтобы открыть меню, отправьте /start.")
                .await?;
        }
        return Ok(());
    };

    if selection.mode.requires_images()
        && let Some(extension) = image_extension(&message)
    {
        if selection.images.len() >= MAX_IMAGES {
            message
                .respond(format!(
                    "Уже загружено {MAX_IMAGES} изображений — это максимум. Отправьте текстовый промпт."
                ))
                .await?;
            return Ok(());
        }

        let image = match download_reference_image(&message, &extension).await {
            Ok(image) => image,
            Err(err) => {
                message
                    .respond(format!("❌ Не удалось скачать изображение:\n{err}"))
                    .await?;
                return Ok(());
            }
        };
        let mut pending = state.pending.lock().await;
        let Some(current) = pending.get_mut(&peer) else {
            drop(pending);
            cleanup_image(image).await;
            return Ok(());
        };
        current.images.push(image);
        selection = current.clone();
        drop(pending);

        message
            .respond(format!(
                "📎 Изображение {}/{} принято.\nОтправьте ещё или пришлите текстовый промпт.",
                selection.images.len(),
                MAX_IMAGES
            ))
            .await?;
    }

    if selection.mode.requires_images() {
        if selection.images.is_empty() {
            if !text.is_empty() {
                message
                    .respond("Сначала отправьте хотя бы одно изображение.")
                    .await?;
            }
            return Ok(());
        }

        if text.is_empty() {
            return Ok(());
        }
    } else if text.is_empty() {
        return Ok(());
    }

    start_generation(client, state, &message, selection, text).await
}

async fn start_generation(
    client: &Client,
    state: &BotState,
    message: &Message,
    selection: Selection,
    prompt: String,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mode = selection.mode;
    let workflow = mode.workflow().ok_or("this mode has no ComfyUI workflow")?;
    let peer = message
        .peer_ref()
        .await?
        .ok_or("Telegram did not return a peer reference")?;
    let (width, height) = if mode.is_video() {
        (640, 640)
    } else {
        selection.ratio.size_for(selection.resolution)
    };

    let images = if mode.requires_images() {
        let mut pending = state.pending.lock().await;
        match pending.get_mut(&message.peer_id()) {
            Some(current) => std::mem::take(&mut current.images),
            None => selection.images.clone(),
        }
    } else {
        Vec::new()
    };

    if mode.requires_images() && images.is_empty() {
        message
            .respond("Сначала отправьте хотя бы одно изображение.")
            .await?;
        return Ok(());
    }

    let queue_line = match state.comfy.queue_info().await {
        Ok(queue) if queue.total() == 0 => "Очередь ComfyUI пуста.".to_owned(),
        Ok(queue) => format!(
            "Заданий в очереди: {} (выполняется: {}).",
            queue.total(),
            queue.running
        ),
        Err(err) => {
            warn!(%err, "failed to read the ComfyUI queue");
            "Не удалось узнать состояние очереди ComfyUI.".to_owned()
        }
    };

    let (params, details) = if mode.requires_images() {
        (
            format!("изображений: {}", images.len()),
            format!("изображений: {}", images.len()),
        )
    } else if mode.is_video() {
        (
            "видео 640×640 · 5 с".to_owned(),
            "видео · 640×640 · 5 с".to_owned(),
        )
    } else {
        (
            format!("{} · {width}×{height}", selection.ratio.label()),
            format!("{} · {width}×{height}", selection.ratio.label()),
        )
    };

    message
        .respond(InputMessage::new().text(format!(
            "⏳ Запускаю {} ({params})…\n{queue_line}\n\nПромпт: {}",
            mode.label(),
            prompt
        )))
        .await?;

    let client = client.clone();
    let comfy = state.comfy.clone();
    let prompt_for_task = prompt.clone();
    let workflow = workflow.to_owned();

    tokio::spawn(async move {
        let result = if mode.requires_images() {
            comfy.edit(&workflow, &prompt_for_task, &images).await
        } else {
            comfy
                .generate(&workflow, &prompt_for_task, width, height)
                .await
        };

        for image in &images {
            let _ = tokio::fs::remove_file(&image.path).await;
        }

        if let Err(err) = deliver_result(client, peer, details, &prompt_for_task, result).await {
            error!(%err, "failed to deliver generation result");
        }
    });

    Ok(())
}

async fn deliver_result(
    client: Client,
    peer: PeerRef,
    details: String,
    prompt: &str,
    result: Result<GeneratedFile, ComfyError>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match result {
        Ok(file) => {
            let is_video = file.is_video();
            let width = file.width.unwrap_or(640) as i32;
            let height = file.height.unwrap_or(640) as i32;
            let duration = Duration::from_secs_f64(file.duration.unwrap_or(5.0).max(0.1));
            let size = file.bytes.len();
            let mut cursor = Cursor::new(file.bytes);
            let uploaded = client
                .upload_stream(&mut cursor, size, file.file_name)
                .await?;

            let caption = format!("✅ Готово · {details}\n\n{}", caption(prompt));

            if is_video {
                client
                    .send_message(
                        peer,
                        InputMessage::new()
                            .text(caption)
                            .document(uploaded)
                            .attribute(Attribute::Video {
                                round_message: false,
                                supports_streaming: true,
                                duration,
                                w: width,
                                h: height,
                            }),
                    )
                    .await?;

                return Ok(());
            }

            let photo = InputMessage::new().text(caption).photo(uploaded.clone());

            if let Err(err) = client.send_message(peer, photo).await {
                error!(%err, "failed to send compressed photo; sending the original file");

                client
                    .send_message(
                        peer,
                        format!("⚠️ Не удалось отправить сжатое фото: {err}\nОтправляю оригинал."),
                    )
                    .await?;
            }

            // Telegram compresses photos, so also send the untouched file as a document.
            client
                .send_message(
                    peer,
                    InputMessage::new()
                        .text(format!("🗂 Оригинал · {details}"))
                        .file(uploaded),
                )
                .await?;
        }
        Err(err) => {
            client
                .send_message(
                    peer,
                    format!("❌ Не удалось сгенерировать результат:\n{err}"),
                )
                .await?;
        }
    }

    Ok(())
}

async fn download_reference_image(
    message: &Message,
    extension: &str,
) -> Result<InputImage, Box<dyn Error + Send + Sync>> {
    let dir = env::temp_dir().join("comfy-bot");
    tokio::fs::create_dir_all(&dir).await?;

    let file_name = format!(
        "{}-{:016x}.{}",
        message.id(),
        rand::random::<u64>(),
        extension
    );
    let path = dir.join(&file_name);
    let downloaded = message.download_media(&path).await;

    if !matches!(downloaded, Ok(true)) {
        let _ = tokio::fs::remove_file(&path).await;
    }
    if !downloaded? {
        return Err("Telegram did not return the image data".into());
    }

    Ok(InputImage { path, file_name })
}

async fn cleanup_image(image: InputImage) {
    let _ = tokio::fs::remove_file(image.path).await;
}

async fn cleanup_images(images: Vec<InputImage>) {
    for image in images {
        cleanup_image(image).await;
    }
}

fn image_extension(message: &Message) -> Option<String> {
    match message.media()? {
        Media::Photo(_) => Some("jpg".to_owned()),
        Media::Document(document) => {
            let from_name = document.name().and_then(extension_from_name);
            let from_mime = document.mime_type().and_then(extension_from_mime);

            if from_name.is_none() && from_mime.is_none() {
                return None;
            }

            from_name.or(from_mime)
        }
        _ => None,
    }
}

fn extension_from_name(name: &str) -> Option<String> {
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();

    matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif"
    )
    .then_some(extension)
}

fn extension_from_mime(mime: &str) -> Option<String> {
    match mime {
        "image/jpeg" => Some("jpg".to_owned()),
        "image/png" => Some("png".to_owned()),
        "image/webp" => Some("webp".to_owned()),
        "image/bmp" => Some("bmp".to_owned()),
        "image/gif" => Some("gif".to_owned()),
        _ => None,
    }
}

/// Whether `text` starts with `command`, allowing a `@botname` suffix.
fn is_command(text: &str, command: &str) -> bool {
    let first = text.split_whitespace().next().unwrap_or_default();
    first.split('@').next() == Some(command)
}

/// Telegram captions are limited to 1024 characters.
fn caption(prompt: &str) -> String {
    const MAX: usize = 900;

    if prompt.chars().count() <= MAX {
        prompt.to_owned()
    } else {
        let mut shortened: String = prompt.chars().take(MAX).collect();
        shortened.push('…');
        shortened
    }
}
