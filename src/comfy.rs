//! Minimal client for the ComfyUI HTTP API.
//!
//! The bot submits a workflow in ComfyUI's "API format", polls the history
//! endpoint until the prompt finishes, and downloads the first produced image.

use std::{
    fmt,
    path::PathBuf,
    time::{Duration, Instant},
};

use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};
use tracing::warn;

/// Public ComfyUI instance used by default.
pub const DEFAULT_BASE_URL: &str = "https://comfy.r0o.ru";

/// Workflow used by the "Z-Image Turbo Generation" menu entry.
pub const Z_IMAGE_TURBO_WORKFLOW: &str = "z-image-turbo";

/// Workflow used by the "Z-Image Generation" menu entry.
pub const Z_IMAGE_WORKFLOW: &str = "z-image";

/// Workflow used by the "Qwen 2.1 Turbo Generation" menu entry.
pub const QWEN_TURBO_WORKFLOW: &str = "qwen-turbo";

/// Workflow used by the "Qwen Edit Turbo" menu entry.
pub const QWEN_EDIT_TURBO_WORKFLOW: &str = "qwen-edit-turbo";

/// Workflow used by the "Wan Text to Video" menu entry.
pub const WAN_TEXT_WORKFLOW: &str = "wan-text";

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const GENERATION_TIMEOUT: Duration = Duration::from_secs(8 * 60 * 60);
const QUEUE_MISSING_GRACE: Duration = Duration::from_secs(90);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct ComfyClient {
    base_url: String,
    http: reqwest::Client,
}

#[derive(Debug)]
pub struct ComfyError(String);

/// Result of a completed ComfyUI prompt.
#[derive(Clone, Debug)]
pub struct GeneratedFile {
    pub bytes: Vec<u8>,
    pub file_name: String,
    pub kind: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration: Option<f64>,
}

impl GeneratedFile {
    pub fn is_video(&self) -> bool {
        self.kind == "video"
            || self
                .file_name
                .rsplit_once('.')
                .is_some_and(|(_, extension)| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "mp4" | "webm" | "mov" | "mkv"
                    )
                })
    }
}

/// Local image that should be uploaded to ComfyUI before running an edit workflow.
#[derive(Clone, Debug)]
pub struct InputImage {
    pub path: PathBuf,
    pub file_name: String,
}

/// Current depth of the ComfyUI queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueInfo {
    pub running: usize,
    pub pending: usize,
}

impl QueueInfo {
    pub fn total(self) -> usize {
        self.running + self.pending
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueueState {
    Running,
    Pending,
    Missing,
}

impl fmt::Display for ComfyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ComfyError {}

impl ComfyClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("failed to build the HTTP client");

        Self { base_url, http }
    }

    /// Queue a workflow by name and return the bytes of its first output image.
    pub async fn generate(
        &self,
        workflow: &str,
        prompt: &str,
        width: u32,
        height: u32,
    ) -> Result<GeneratedFile, ComfyError> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(ComfyError("промпт пуст".to_owned()));
        }
        if width < 16 || height < 16 || !width.is_multiple_of(16) || !height.is_multiple_of(16) {
            return Err(ComfyError(format!(
                "некорректный размер изображения: {width}×{height} (нужны значения, кратные 16)"
            )));
        }

        let graph = match workflow {
            Z_IMAGE_TURBO_WORKFLOW => {
                z_image_graph(ZImageKind::Turbo, prompt, rand::random(), width, height)
            }
            Z_IMAGE_WORKFLOW => {
                z_image_graph(ZImageKind::Base, prompt, rand::random(), width, height)
            }
            QWEN_TURBO_WORKFLOW => qwen_turbo_graph(prompt, rand::random(), width, height),
            WAN_TEXT_WORKFLOW => wan_text_graph(prompt, rand::random(), width, height),
            other => {
                return Err(ComfyError(format!(
                    "workflow `{other}` не поддерживается ботом"
                )));
            }
        };

        let prompt_id = self.queue_prompt(graph).await?;
        self.wait_for_result(&prompt_id).await
    }

    /// Upload reference images and run the Qwen Image Edit workflow.
    pub async fn edit(
        &self,
        workflow: &str,
        prompt: &str,
        images: &[InputImage],
    ) -> Result<GeneratedFile, ComfyError> {
        if workflow != QWEN_EDIT_TURBO_WORKFLOW {
            return Err(ComfyError(format!(
                "workflow `{workflow}` не поддерживает редактирование изображений"
            )));
        }

        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(ComfyError("промпт пуст".to_owned()));
        }
        if images.is_empty() {
            return Err(ComfyError("не загружено ни одного изображения".to_owned()));
        }
        if images.len() > 10 {
            return Err(ComfyError(
                "можно загрузить не больше 10 изображений".to_owned(),
            ));
        }

        let mut uploaded = Vec::with_capacity(images.len());
        for image in images {
            uploaded.push(self.upload_image(image).await?);
        }

        let graph = qwen_edit_turbo_graph(prompt, rand::random(), &uploaded);
        let prompt_id = self.queue_prompt(graph).await?;
        self.wait_for_result(&prompt_id).await
    }

    async fn upload_image(&self, image: &InputImage) -> Result<String, ComfyError> {
        let bytes = tokio::fs::read(&image.path)
            .await
            .map_err(|err| ComfyError(format!("не удалось прочитать изображение: {err}")))?;

        let part = Part::bytes(bytes)
            .file_name(image.file_name.clone())
            .mime_str(mime_for_name(&image.file_name))
            .map_err(|err| ComfyError(format!("некорректный MIME-тип изображения: {err}")))?;

        let form = Form::new()
            .part("image", part)
            .text("type", "input")
            .text("overwrite", "true");

        let response = self
            .http
            .post(format!("{}/upload/image", self.base_url))
            .multipart(form)
            .send()
            .await
            .map_err(|err| ComfyError(format!("не удалось загрузить изображение: {err}")))?;

        let status = response.status();
        let body = response.text().await.map_err(|err| {
            ComfyError(format!(
                "не удалось прочитать ответ загрузки изображения: {err}"
            ))
        })?;

        if !status.is_success() {
            return Err(ComfyError(format!(
                "ComfyUI отклонил изображение (HTTP {status}): {}",
                compact(&body)
            )));
        }

        let value: Value = serde_json::from_str(&body)
            .map_err(|err| ComfyError(format!("некорректный ответ загрузки: {err}")))?;
        let name = value.get("name").and_then(Value::as_str).ok_or_else(|| {
            ComfyError(format!("ComfyUI не вернул имя файла: {}", compact(&body)))
        })?;
        let subfolder = value
            .get("subfolder")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if subfolder.is_empty() {
            Ok(name.to_owned())
        } else {
            Ok(format!("{subfolder}/{name}"))
        }
    }

    /// Read how many prompts are currently running or waiting on the server.
    pub async fn queue_info(&self) -> Result<QueueInfo, ComfyError> {
        let value = self.queue_value().await?;
        Ok(parse_queue(&value))
    }

    async fn prompt_queue_state(&self, prompt_id: &str) -> Result<QueueState, ComfyError> {
        let value = self.queue_value().await?;
        Ok(prompt_queue_state(&value, prompt_id))
    }

    async fn queue_value(&self) -> Result<Value, ComfyError> {
        let response = self
            .http
            .get(format!("{}/queue", self.base_url))
            .send()
            .await
            .map_err(|err| ComfyError(format!("не удалось запросить очередь: {err}")))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| ComfyError(format!("не удалось прочитать очередь: {err}")))?;

        if !status.is_success() {
            return Err(ComfyError(format!(
                "ComfyUI вернул ошибку очереди (HTTP {status}): {}",
                compact(&body)
            )));
        }

        serde_json::from_str(&body)
            .map_err(|err| ComfyError(format!("некорректный ответ очереди ComfyUI: {err}")))
    }

    async fn queue_prompt(&self, graph: Value) -> Result<String, ComfyError> {
        let response = self
            .http
            .post(format!("{}/prompt", self.base_url))
            .json(&json!({
                "prompt": graph,
                "client_id": "comfy-telegram-bot",
            }))
            .send()
            .await
            .map_err(|err| ComfyError(format!("не удалось отправить workflow: {err}")))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| ComfyError(format!("не удалось прочитать ответ ComfyUI: {err}")))?;

        if !status.is_success() {
            return Err(ComfyError(format!(
                "ComfyUI отклонил workflow (HTTP {status}): {}",
                compact(&body)
            )));
        }

        let value: Value = serde_json::from_str(&body)
            .map_err(|err| ComfyError(format!("некорректный ответ ComfyUI: {err}")))?;

        value
            .get("prompt_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ComfyError(format!("ComfyUI не вернул prompt_id: {}", compact(&body))))
    }

    async fn wait_for_result(&self, prompt_id: &str) -> Result<GeneratedFile, ComfyError> {
        let started = Instant::now();
        let mut missing_since: Option<Instant> = None;

        loop {
            if started.elapsed() >= GENERATION_TIMEOUT {
                return Err(ComfyError(
                    "генерация заняла слишком много времени".to_owned(),
                ));
            }

            tokio::time::sleep(POLL_INTERVAL).await;

            let response = self
                .http
                .get(format!("{}/history/{prompt_id}", self.base_url))
                .send()
                .await
                .map_err(|err| ComfyError(format!("не удалось проверить историю: {err}")))?;

            let status = response.status();
            let body = response.text().await.map_err(|err| {
                ComfyError(format!("не удалось прочитать историю ComfyUI: {err}"))
            })?;

            if !status.is_success() {
                return Err(ComfyError(format!(
                    "ComfyUI вернул ошибку истории (HTTP {status}): {}",
                    compact(&body)
                )));
            }

            let value: Value = serde_json::from_str(&body)
                .map_err(|err| ComfyError(format!("некорректная история ComfyUI: {err}")))?;

            if let Some(entry) = value.get(prompt_id) {
                missing_since = None;

                if entry
                    .pointer("/status/status_str")
                    .and_then(Value::as_str)
                    .is_some_and(|status| status == "error")
                {
                    let details =
                        execution_error(entry).unwrap_or_else(|| compact(&entry.to_string()));
                    return Err(ComfyError(format!(
                        "workflow завершился с ошибкой: {details}"
                    )));
                }

                if let Some(file) = first_output_file(entry) {
                    return self.download_file(&file).await;
                }

                // The entry can exist while execution is still in progress.
                if entry
                    .pointer("/status/completed")
                    .and_then(Value::as_bool)
                    .is_some_and(|completed| completed)
                {
                    return Err(ComfyError("workflow завершился без результата".to_owned()));
                }
            }

            match self.prompt_queue_state(prompt_id).await {
                Ok(QueueState::Missing) => {
                    let since = *missing_since.get_or_insert_with(Instant::now);

                    if started.elapsed() >= QUEUE_MISSING_GRACE
                        && since.elapsed() >= QUEUE_MISSING_GRACE
                    {
                        return Err(ComfyError(format!(
                            "задача `{prompt_id}` исчезла из очереди ComfyUI и отсутствует в истории — возможно, она была отменена или упала"
                        )));
                    }
                }
                Ok(QueueState::Running | QueueState::Pending) => {
                    missing_since = None;
                }
                Err(err) => {
                    warn!(%err, "failed to check the ComfyUI queue while waiting");
                }
            }
        }
    }

    async fn download_file(&self, file: &OutputFile) -> Result<GeneratedFile, ComfyError> {
        let response = self
            .http
            .get(format!("{}/view", self.base_url))
            .query(&[
                ("filename", file.filename.as_str()),
                ("subfolder", file.subfolder.as_str()),
                ("type", file.storage.as_str()),
            ])
            .send()
            .await
            .map_err(|err| ComfyError(format!("не удалось скачать изображение: {err}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ComfyError(format!(
                "не удалось скачать изображение (HTTP {status}): {}",
                compact(&body)
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|err| ComfyError(format!("не удалось прочитать результат: {err}")))?
            .to_vec();

        let metadata = file.metadata.as_ref();
        let kind = metadata
            .and_then(|metadata| metadata.get("kind"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if is_video_name(&file.filename) {
                    "video".to_owned()
                } else {
                    "image".to_owned()
                }
            });

        Ok(GeneratedFile {
            bytes,
            file_name: file.filename.clone(),
            kind,
            width: metadata_u32(metadata, "width"),
            height: metadata_u32(metadata, "height"),
            duration: metadata
                .and_then(|metadata| metadata.get("duration"))
                .and_then(Value::as_f64),
        })
    }
}

fn metadata_u32(metadata: Option<&Value>, key: &str) -> Option<u32> {
    metadata
        .and_then(|metadata| metadata.get(key))
        .and_then(Value::as_u64)
        .map(|value| value as u32)
}

fn is_video_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "mp4" | "webm" | "mov" | "mkv"
        )
    })
}

/// A file reference returned by ComfyUI's history endpoint.
struct OutputFile {
    filename: String,
    subfolder: String,
    storage: String,
    metadata: Option<Value>,
}

fn first_output_file(entry: &Value) -> Option<OutputFile> {
    let outputs = entry.get("outputs")?.as_object()?;

    for node in outputs.values() {
        let Some(images) = node.get("images").and_then(Value::as_array) else {
            continue;
        };

        for image in images {
            let Some(filename) = image.get("filename").and_then(Value::as_str) else {
                continue;
            };

            return Some(OutputFile {
                filename: filename.to_owned(),
                subfolder: image
                    .get("subfolder")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                storage: image
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("output")
                    .to_owned(),
                metadata: image.get("metadata").cloned(),
            });
        }
    }

    None
}

/// The two Z-Image variants exposed by the bot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ZImageKind {
    Turbo,
    Base,
}

impl ZImageKind {
    fn unet_name(self) -> &'static str {
        match self {
            Self::Turbo => "z_image_turbo_bf16.safetensors",
            Self::Base => "z_image_bf16.safetensors",
        }
    }

    fn steps(self) -> u32 {
        match self {
            Self::Turbo => 8,
            Self::Base => 25,
        }
    }

    fn cfg(self) -> f64 {
        match self {
            Self::Turbo => 1.0,
            Self::Base => 4.0,
        }
    }

    fn filename_prefix(self) -> &'static str {
        match self {
            Self::Turbo => "z-image-turbo",
            Self::Base => "z-image",
        }
    }

    /// Turbo has no negative prompt (zeroed conditioning), while base uses an
    /// empty CLIP text encode as the negative.
    fn negative_node(self) -> Value {
        match self {
            Self::Turbo => json!({
                "class_type": "ConditioningZeroOut",
                "inputs": {
                    "conditioning": ["27", 0]
                }
            }),
            Self::Base => json!({
                "class_type": "CLIPTextEncode",
                "inputs": {
                    "clip": ["30", 0],
                    "text": ""
                }
            }),
        }
    }
}

/// Build the API-format version of a Z-Image workflow.
///
/// Values mirror the subgraphs stored on the ComfyUI server. UI-only nodes
/// (Telegram delivery, resolution selector, notes) are not included: the bot
/// passes width and height directly and delivers the image itself.
fn z_image_graph(kind: ZImageKind, prompt: &str, seed: u64, width: u32, height: u32) -> Value {
    let negative = kind.negative_node();

    json!({
        "3": {
            "class_type": "KSampler",
            "inputs": {
                "model": ["11", 0],
                "positive": ["27", 0],
                "negative": ["33", 0],
                "latent_image": ["13", 0],
                "seed": seed,
                "steps": kind.steps(),
                "cfg": kind.cfg(),
                "sampler_name": "res_multistep",
                "scheduler": "simple",
                "denoise": 1.0
            }
        },
        "8": {
            "class_type": "VAEDecode",
            "inputs": {
                "samples": ["3", 0],
                "vae": ["29", 0]
            }
        },
        "9": {
            "class_type": "SaveImage",
            "inputs": {
                "images": ["8", 0],
                "filename_prefix": kind.filename_prefix()
            }
        },
        "11": {
            "class_type": "ModelSamplingAuraFlow",
            "inputs": {
                "model": ["28", 0],
                "shift": 3.0,
                "sampling": "flow"
            }
        },
        "13": {
            "class_type": "EmptySD3LatentImage",
            "inputs": {
                "width": width,
                "height": height,
                "batch_size": 1
            }
        },
        "27": {
            "class_type": "CLIPTextEncode",
            "inputs": {
                "clip": ["30", 0],
                "text": prompt
            }
        },
        "28": {
            "class_type": "UNETLoader",
            "inputs": {
                "unet_name": kind.unet_name(),
                "weight_dtype": "default"
            }
        },
        "29": {
            "class_type": "VAELoader",
            "inputs": {
                "vae_name": "ae.safetensors"
            }
        },
        "30": {
            "class_type": "CLIPLoader",
            "inputs": {
                "clip_name": "qwen_3_4b.safetensors",
                "type": "lumina2",
                "device": "default"
            }
        },
        "33": negative
    })
}

/// Build the API-format version of the Qwen Image 2.1 Turbo workflow.
///
/// The prompt-enhancer branch from the original workflow is intentionally
/// omitted: it is disabled by default there, and the bot feeds the user prompt
/// straight into the image text encoder.
fn qwen_turbo_graph(prompt: &str, seed: u64, width: u32, height: u32) -> Value {
    json!({
        "451": {
            "class_type": "UNETLoader",
            "inputs": {
                "unet_name": "qwen_image_2.1_turbo_int8_convrot.safetensors",
                "weight_dtype": "default"
            }
        },
        "452": {
            "class_type": "TextEncodeQwenImage21",
            "inputs": {
                "clip": ["453", 0],
                "prompt": prompt,
                "negative_prompt": "",
                "resolution": 1024
            }
        },
        "453": {
            "class_type": "CLIPLoader",
            "inputs": {
                "clip_name": "qwen3vl_8b_int8_convrot.safetensors",
                "type": "qwen_image",
                "device": "default"
            }
        },
        "454": {
            "class_type": "VAELoader",
            "inputs": {
                "vae_name": "qwen_image_2.1_vae_bf16.safetensors"
            }
        },
        "456": {
            "class_type": "EmptyLatentImage",
            "inputs": {
                "width": width,
                "height": height,
                "batch_size": 1
            }
        },
        "457": {
            "class_type": "VAEDecode",
            "inputs": {
                "samples": ["486", 0],
                "vae": ["454", 0]
            }
        },
        "461": {
            "class_type": "SaveImage",
            "inputs": {
                "images": ["457", 0],
                "filename_prefix": "qwen-image-2.1-turbo"
            }
        },
        "484": {
            "class_type": "ManualSigmas",
            "inputs": {
                "sigmas": "1.0, 0.978453, 0.95418, 0.926626, 0.89508, 0.845148, 0.704534, 0.414568, 0"
            }
        },
        "486": {
            "class_type": "SamplerCustomAdvanced",
            "inputs": {
                "noise": ["488", 0],
                "guider": ["489", 0],
                "sampler": ["487", 0],
                "sigmas": ["484", 0],
                "latent_image": ["456", 0]
            }
        },
        "487": {
            "class_type": "KSamplerSelect",
            "inputs": {
                "sampler_name": "euler"
            }
        },
        "488": {
            "class_type": "RandomNoise",
            "inputs": {
                "noise_seed": seed
            }
        },
        "489": {
            "class_type": "CFGGuider",
            "inputs": {
                "model": ["490", 0],
                "positive": ["452", 0],
                "negative": ["452", 1],
                "cfg": 1.0
            }
        },
        "490": {
            "class_type": "QwenImage21Cache",
            "inputs": {
                "model": ["451", 0],
                "device": "auto",
                "dtype": "default"
            }
        }
    })
}

/// Build the API-format version of the Qwen Image 2.1 Edit Turbo workflow.
///
/// Reference images are uploaded separately and referenced by `LoadImage`
/// nodes. The prompt enhancer branch is skipped; the prompt goes directly to
/// `TextEncodeQwenImage21`, and its latent output drives the sampler.
fn qwen_edit_turbo_graph(prompt: &str, seed: u64, images: &[String]) -> Value {
    let mut text_inputs = json!({
        "clip": ["453", 0],
        "vae": ["454", 0],
        "prompt": prompt,
        "negative_prompt": "",
        "resolution": 0
    });

    {
        let inputs = text_inputs.as_object_mut().unwrap();
        for (index, _) in images.iter().enumerate() {
            let load_id = format!("{}", 900 + index);
            inputs.insert(format!("images.image_{}", index + 1), json!([load_id, 0]));
        }
    }

    let mut graph = json!({
        "451": {
            "class_type": "UNETLoader",
            "inputs": {
                "unet_name": "qwen_image_2.1_turbo_int8_convrot.safetensors",
                "weight_dtype": "default"
            }
        },
        "452": {
            "class_type": "TextEncodeQwenImage21",
            "inputs": text_inputs
        },
        "453": {
            "class_type": "CLIPLoader",
            "inputs": {
                "clip_name": "qwen3vl_8b_int8_convrot.safetensors",
                "type": "qwen_image",
                "device": "default"
            }
        },
        "454": {
            "class_type": "VAELoader",
            "inputs": {
                "vae_name": "qwen_image_2.1_vae_bf16.safetensors"
            }
        },
        "457": {
            "class_type": "VAEDecode",
            "inputs": {
                "samples": ["486", 0],
                "vae": ["454", 0]
            }
        },
        "461": {
            "class_type": "SaveImage",
            "inputs": {
                "images": ["457", 0],
                "filename_prefix": "qwen-edit-turbo"
            }
        },
        "469": {
            "class_type": "QwenImage21Cache",
            "inputs": {
                "model": ["451", 0],
                "device": "auto",
                "dtype": "default"
            }
        },
        "484": {
            "class_type": "ManualSigmas",
            "inputs": {
                "sigmas": "1.0, 0.978453, 0.95418, 0.926626, 0.89508, 0.845148, 0.704534, 0.414568, 0"
            }
        },
        "486": {
            "class_type": "SamplerCustomAdvanced",
            "inputs": {
                "noise": ["488", 0],
                "guider": ["489", 0],
                "sampler": ["487", 0],
                "sigmas": ["484", 0],
                "latent_image": ["452", 2]
            }
        },
        "487": {
            "class_type": "KSamplerSelect",
            "inputs": {
                "sampler_name": "euler"
            }
        },
        "488": {
            "class_type": "RandomNoise",
            "inputs": {
                "noise_seed": seed
            }
        },
        "489": {
            "class_type": "CFGGuider",
            "inputs": {
                "model": ["469", 0],
                "positive": ["452", 0],
                "negative": ["452", 1],
                "cfg": 1.0
            }
        }
    });

    let nodes = graph.as_object_mut().unwrap();
    for (index, name) in images.iter().enumerate() {
        let load_id = format!("{}", 900 + index);
        nodes.insert(
            load_id,
            json!({
                "class_type": "LoadImage",
                "inputs": {
                    "image": name
                }
            }),
        );
    }

    graph
}

fn mime_for_name(name: &str) -> &'static str {
    match name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("gif") => "image/gif",
        _ => "application/octet-stream",
    }
}

/// Build the API-format version of the Wan 2.2 text-to-video workflow.
///
/// This follows the non-Lightning branch of `wan-text`: 20 steps on the
/// high-noise model and 10 more on the low-noise model. The Lightning LoRA
/// branch is much faster but needs more free VRAM when patching the fp8 models.
fn wan_text_graph(prompt: &str, seed: u64, width: u32, height: u32) -> Value {
    const NEGATIVE: &str = "色调艳丽，过曝，静态，细节模糊不清，字幕，风格，作品，画作，画面，静止，整体发灰，最差质量，低质量，JPEG压缩残留，丑陋的，残缺的，多余的手指，画得不好的手部，画得不好的脸部，畸形的，毁容的，形态畸形的肢体，手指融合，静止不动的画面，杂乱的背景，三条腿，背景人很多，倒着走，裸露，NSFW";

    json!({
        "71": {
            "class_type": "CLIPLoader",
            "inputs": {
                "clip_name": "umt5_xxl_fp8_e4m3fn_scaled.safetensors",
                "type": "wan",
                "device": "default"
            }
        },
        "72": {
            "class_type": "CLIPTextEncode",
            "inputs": {
                "clip": ["71", 0],
                "text": NEGATIVE
            }
        },
        "73": {
            "class_type": "VAELoader",
            "inputs": {
                "vae_name": "wan_2.1_vae.safetensors"
            }
        },
        "74": {
            "class_type": "EmptyHunyuanLatentVideo",
            "inputs": {
                "width": width,
                "height": height,
                "length": 81,
                "batch_size": 1
            }
        },
        "75": {
            "class_type": "UNETLoader",
            "inputs": {
                "unet_name": "wan2.2_t2v_high_noise_14B_fp8_scaled.safetensors",
                "weight_dtype": "default"
            }
        },
        "76": {
            "class_type": "UNETLoader",
            "inputs": {
                "unet_name": "wan2.2_t2v_low_noise_14B_fp8_scaled.safetensors",
                "weight_dtype": "default"
            }
        },
        "78": {
            "class_type": "KSamplerAdvanced",
            "inputs": {
                "model": ["86", 0],
                "add_noise": "disable",
                "noise_seed": 0,
                "steps": 20,
                "cfg": 3.5,
                "sampler_name": "euler",
                "scheduler": "simple",
                "positive": ["89", 0],
                "negative": ["72", 0],
                "latent_image": ["81", 0],
                "start_at_step": 10,
                "end_at_step": 20,
                "return_with_leftover_noise": "disable"
            }
        },
        "80": {
            "class_type": "SaveVideo",
            "inputs": {
                "video": ["88", 0],
                "filename_prefix": "video/wan-text",
                "format": "mp4",
                "codec": {"codec": "h264"}
            }
        },
        "81": {
            "class_type": "KSamplerAdvanced",
            "inputs": {
                "model": ["82", 0],
                "add_noise": "enable",
                "noise_seed": seed,
                "steps": 20,
                "cfg": 3.5,
                "sampler_name": "euler",
                "scheduler": "simple",
                "positive": ["89", 0],
                "negative": ["72", 0],
                "latent_image": ["74", 0],
                "start_at_step": 0,
                "end_at_step": 10,
                "return_with_leftover_noise": "enable"
            }
        },
        "82": {
            "class_type": "ModelSamplingSD3",
            "inputs": {
                "model": ["75", 0],
                "shift": 5.0
            }
        },
        "86": {
            "class_type": "ModelSamplingSD3",
            "inputs": {
                "model": ["76", 0],
                "shift": 5.0
            }
        },
        "87": {
            "class_type": "VAEDecode",
            "inputs": {
                "samples": ["78", 0],
                "vae": ["73", 0]
            }
        },
        "88": {
            "class_type": "CreateVideo",
            "inputs": {
                "images": ["87", 0],
                "fps": 16,
                "bit_depth": "auto",
                "color_space": "sRGB",
                "chroma_subsampling": "auto",
                "codec": "none"
            }
        },
        "89": {
            "class_type": "CLIPTextEncode",
            "inputs": {
                "clip": ["71", 0],
                "text": prompt
            }
        }
    })
}

/// Parse `/queue` into running and pending counts.
fn parse_queue(value: &Value) -> QueueInfo {
    let len = |key: &str| value.get(key).and_then(Value::as_array).map_or(0, Vec::len);

    QueueInfo {
        running: len("queue_running"),
        pending: len("queue_pending"),
    }
}

fn prompt_queue_state(value: &Value, prompt_id: &str) -> QueueState {
    let contains = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry
                        .get(1)
                        .and_then(Value::as_str)
                        .is_some_and(|id| id == prompt_id)
                })
            })
    };

    if contains("queue_running") {
        QueueState::Running
    } else if contains("queue_pending") {
        QueueState::Pending
    } else {
        QueueState::Missing
    }
}

/// Extract a concise error from ComfyUI's `execution_error` status message.
fn execution_error(entry: &Value) -> Option<String> {
    let messages = entry.pointer("/status/messages")?.as_array()?;

    for message in messages {
        let pair = message.as_array()?;
        if pair.first().and_then(Value::as_str) != Some("execution_error") {
            continue;
        }

        let details = pair.get(1)?;
        let node_type = details
            .get("node_type")
            .and_then(Value::as_str)
            .unwrap_or("node");
        let node_id = details
            .get("node_id")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let exception = details
            .get("exception_message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");

        return Some(format!("{node_type} #{node_id}: {}", exception.trim()));
    }

    None
}

/// Keep error bodies readable in Telegram messages.
fn compact(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() <= 500 {
        body.to_owned()
    } else {
        let mut shortened: String = body.chars().take(500).collect();
        shortened.push('…');
        shortened
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ZImageKind, qwen_edit_turbo_graph, qwen_turbo_graph, wan_text_graph, z_image_graph,
    };

    #[test]
    fn qwen_edit_graph_connects_uploaded_images() {
        let images = vec!["input-1.png".to_owned(), "input-2.jpg".to_owned()];
        let graph = qwen_edit_turbo_graph("make the apple green", 5, &images);

        assert_eq!(graph["452"]["inputs"]["prompt"], "make the apple green");
        assert_eq!(graph["452"]["inputs"]["negative_prompt"], "");
        assert_eq!(
            graph["452"]["inputs"]["images.image_1"],
            serde_json::json!(["900", 0])
        );
        assert_eq!(
            graph["452"]["inputs"]["images.image_2"],
            serde_json::json!(["901", 0])
        );
        assert_eq!(graph["900"]["inputs"]["image"], "input-1.png");
        assert_eq!(graph["901"]["inputs"]["image"], "input-2.jpg");
        assert_eq!(
            graph["486"]["inputs"]["latent_image"],
            serde_json::json!(["452", 2])
        );
    }

    #[test]
    fn qwen_edit_graph_supports_ten_images() {
        let images: Vec<String> = (1..=10).map(|index| format!("input-{index}.png")).collect();
        let graph = qwen_edit_turbo_graph("edit all", 1, &images);

        for index in 1..=10 {
            let load_id = format!("{}", 899 + index);
            let key = format!("images.image_{index}");
            assert_eq!(
                graph["452"]["inputs"][&key],
                serde_json::json!([load_id, 0])
            );
            assert_eq!(
                graph[&load_id]["inputs"]["image"],
                format!("input-{index}.png")
            );
        }
    }

    #[test]
    fn wan_graph_uses_text_to_video_models_and_saves_mp4() {
        let graph = wan_text_graph("test prompt", 9, 640, 640);

        assert_eq!(graph["89"]["inputs"]["text"], "test prompt");
        assert_eq!(graph["74"]["inputs"]["width"], 640);
        assert_eq!(graph["74"]["inputs"]["length"], 81);
        assert_eq!(graph["81"]["inputs"]["steps"], 20);
        assert_eq!(graph["81"]["inputs"]["end_at_step"], 10);
        assert_eq!(graph["78"]["inputs"]["start_at_step"], 10);
        assert_eq!(
            graph["75"]["inputs"]["unet_name"],
            "wan2.2_t2v_high_noise_14B_fp8_scaled.safetensors"
        );
        assert_eq!(graph["80"]["class_type"], "SaveVideo");
        assert_eq!(graph["80"]["inputs"]["format"], "mp4");
    }

    #[test]
    fn mime_types_follow_the_file_extension() {
        assert_eq!(super::mime_for_name("a.png"), "image/png");
        assert_eq!(super::mime_for_name("a.JPG"), "image/jpeg");
        assert_eq!(super::mime_for_name("a.webp"), "image/webp");
        assert_eq!(
            super::mime_for_name("a.unknown"),
            "application/octet-stream"
        );
    }

    #[test]
    fn queue_counts_are_parsed() {
        let value = serde_json::json!({
            "queue_running": [[1, "running-prompt"]],
            "queue_pending": [[2, "pending-1"], [3, "pending-2"]]
        });

        let queue = super::parse_queue(&value);
        assert_eq!(queue.running, 1);
        assert_eq!(queue.pending, 2);
        assert_eq!(queue.total(), 3);
        assert_eq!(
            super::prompt_queue_state(&value, "running-prompt"),
            super::QueueState::Running
        );
        assert_eq!(
            super::prompt_queue_state(&value, "pending-2"),
            super::QueueState::Pending
        );
        assert_eq!(
            super::prompt_queue_state(&value, "missing"),
            super::QueueState::Missing
        );
    }

    #[test]
    fn execution_error_is_extracted_from_status_messages() {
        let entry = serde_json::json!({
            "status": {
                "messages": [
                    ["execution_start", {"prompt_id": "x"}],
                    ["execution_error", {
                        "node_type": "UNETLoader",
                        "node_id": "28",
                        "exception_message": "broken file\n"
                    }]
                ]
            }
        });

        assert_eq!(
            super::execution_error(&entry).as_deref(),
            Some("UNETLoader #28: broken file")
        );
    }

    #[test]
    fn turbo_graph_uses_the_prompt_and_turbo_settings() {
        let graph = z_image_graph(ZImageKind::Turbo, "test prompt", 42, 1248, 832);

        assert_eq!(graph["27"]["inputs"]["text"], "test prompt");
        assert_eq!(graph["3"]["inputs"]["seed"], 42);
        assert_eq!(graph["3"]["inputs"]["steps"], 8);
        assert_eq!(graph["3"]["inputs"]["cfg"], 1.0);
        assert_eq!(
            graph["28"]["inputs"]["unet_name"],
            "z_image_turbo_bf16.safetensors"
        );
        assert_eq!(graph["33"]["class_type"], "ConditioningZeroOut");
        assert_eq!(graph["13"]["inputs"]["width"], 1248);
        assert_eq!(graph["13"]["inputs"]["height"], 832);
        assert_eq!(graph["9"]["inputs"]["images"], serde_json::json!(["8", 0]));
    }

    #[test]
    fn base_graph_uses_an_empty_negative_prompt_and_base_settings() {
        let graph = z_image_graph(ZImageKind::Base, "test prompt", 7, 1024, 576);

        assert_eq!(graph["3"]["inputs"]["steps"], 25);
        assert_eq!(graph["3"]["inputs"]["cfg"], 4.0);
        assert_eq!(
            graph["28"]["inputs"]["unet_name"],
            "z_image_bf16.safetensors"
        );
        assert_eq!(graph["33"]["class_type"], "CLIPTextEncode");
        assert_eq!(graph["33"]["inputs"]["text"], "");
        assert_eq!(graph["9"]["inputs"]["filename_prefix"], "z-image");
    }

    #[test]
    fn qwen_turbo_graph_uses_the_prompt_and_turbo_schedule() {
        let graph = qwen_turbo_graph("test prompt", 11, 1024, 688);

        assert_eq!(graph["452"]["inputs"]["prompt"], "test prompt");
        assert_eq!(graph["452"]["inputs"]["negative_prompt"], "");
        assert_eq!(graph["456"]["inputs"]["width"], 1024);
        assert_eq!(graph["456"]["inputs"]["height"], 688);
        assert_eq!(graph["488"]["inputs"]["noise_seed"], 11);
        assert_eq!(graph["487"]["inputs"]["sampler_name"], "euler");
        assert_eq!(
            graph["451"]["inputs"]["unet_name"],
            "qwen_image_2.1_turbo_int8_convrot.safetensors"
        );
        assert!(
            graph["484"]["inputs"]["sigmas"]
                .as_str()
                .unwrap()
                .starts_with("1.0, 0.978453")
        );
    }

    #[tokio::test]
    #[ignore = "calls the public ComfyUI server and runs a real generation"]
    async fn turbo_generates_an_image_with_the_requested_size() {
        let client = super::ComfyClient::new(super::DEFAULT_BASE_URL);
        let file = client
            .generate(
                super::Z_IMAGE_TURBO_WORKFLOW,
                "a red apple on a wooden table",
                1536,
                864,
            )
            .await
            .expect("generation failed");

        assert_png_size(&file.bytes, 1536, 864);
    }

    #[tokio::test]
    #[ignore = "calls the public ComfyUI server and runs a real generation"]
    async fn base_generates_an_image_with_the_requested_size() {
        let client = super::ComfyClient::new(super::DEFAULT_BASE_URL);
        let file = client
            .generate(
                super::Z_IMAGE_WORKFLOW,
                "a red apple on a wooden table",
                1024,
                576,
            )
            .await
            .expect("generation failed");

        assert_png_size(&file.bytes, 1024, 576);
    }

    #[tokio::test]
    #[ignore = "calls the public ComfyUI server and runs a real generation"]
    async fn qwen_turbo_generates_an_image_with_the_requested_size() {
        let client = super::ComfyClient::new(super::DEFAULT_BASE_URL);
        let file = client
            .generate(
                super::QWEN_TURBO_WORKFLOW,
                "a red apple on a wooden table",
                1024,
                688,
            )
            .await
            .expect("generation failed");

        assert_png_size(&file.bytes, 1024, 688);
    }

    #[tokio::test]
    #[ignore = "calls the public ComfyUI server and runs real generations"]
    async fn qwen_edit_generates_from_uploaded_images() {
        let client = super::ComfyClient::new(super::DEFAULT_BASE_URL);
        let base = client
            .generate(
                super::Z_IMAGE_TURBO_WORKFLOW,
                "a red apple on a wooden table",
                1024,
                688,
            )
            .await
            .expect("base generation failed");

        let dir = std::env::temp_dir().join("comfy-bot-tests");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join(format!("qwen-edit-{}.png", std::process::id()));
        tokio::fs::write(&path, base.bytes).await.unwrap();

        let images = vec![
            super::InputImage {
                path: path.clone(),
                file_name: "qwen-edit-1.png".to_owned(),
            },
            super::InputImage {
                path: path.clone(),
                file_name: "qwen-edit-2.png".to_owned(),
            },
        ];

        let result = client
            .edit(
                super::QWEN_EDIT_TURBO_WORKFLOW,
                "make the apple green",
                &images,
            )
            .await;

        let _ = tokio::fs::remove_file(&path).await;
        let file = result.expect("edit generation failed");

        assert!(file.bytes.len() > 10_000);
        assert_eq!(&file.bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[tokio::test]
    #[ignore = "calls the public ComfyUI server and renders a real video"]
    async fn wan_generates_a_video() {
        let client = super::ComfyClient::new(super::DEFAULT_BASE_URL);
        let file = client
            .generate(
                super::WAN_TEXT_WORKFLOW,
                "a red apple on a wooden table, cinematic",
                256,
                256,
            )
            .await
            .expect("video generation failed");

        assert!(file.is_video());
        assert!(file.file_name.ends_with(".mp4"));
        assert!(file.bytes.len() > 10_000);
        assert_eq!(file.width, Some(256));
        assert_eq!(file.height, Some(256));
    }

    fn assert_png_size(bytes: &[u8], expected_width: u32, expected_height: u32) {
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());

        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!((width, height), (expected_width, expected_height));
    }
}
