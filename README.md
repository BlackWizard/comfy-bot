# comfy-bot

Telegram-бот на Rust поверх MTProto-библиотеки [grammers](https://codeberg.org/Lonami/grammers).
При `/start` показывает меню режимов генерации:

- Z-Image Turbo Generation
- Z-Image Generation
- Qwen 2.1 Turbo Generation
- Qwen Edit Turbo
- Qwen 2.1 Generation
- Wan Text to Video

Кнопки инлайновые. Для **Z-Image Turbo**, **Z-Image** и **Qwen 2.1 Turbo** бот
просит текстовый промпт и позволяет опционально выбрать соотношение сторон и
разрешение (по большей стороне, от 1024 до 4K), затем запускает соответствующий
workflow (`z-image-turbo`, `z-image` или `qwen-turbo`) на
<https://comfy.r0o.ru/> через HTTP API ComfyUI, дожидается результата и отправляет
готовое изображение в чат.

Для **Qwen Edit Turbo** можно загрузить от 1 до 10 изображений и отправить
промпт — бот запустит workflow `qwen-edit-turbo`.

Для **Wan Text to Video** достаточно промпта: бот запускает `wan-text` и
присылает готовый MP4. Остальные режимы пока показывают заглушку.

## Требования

- Rust 1.85+ (проект использует edition 2024)
- `API_ID` и `API_HASH` с <https://my.telegram.org> — grammers работает через MTProto,
  поэтому они нужны даже для бота
- токен бота от [@BotFather](https://t.me/BotFather)

## Настройка

```sh
cp .env.example .env
```

Заполните `.env`:

```dotenv
API_ID=123456
API_HASH=0123456789abcdef0123456789abcdef
BOT_TOKEN=123456789:AAExampleTokenFromBotFather
SESSION_PATH=bot.session
# COMFY_URL=https://comfy.r0o.ru
# PROXY_URL=socks5://127.0.0.1:1080
```

`SESSION_PATH`, `COMFY_URL` и `PROXY_URL` опциональны. Файл сессии создаётся
автоматически и хранит авторизацию, чтобы не логиниться при каждом запуске.

## Генерация через ComfyUI

По умолчанию используется `COMFY_URL=https://comfy.r0o.ru`. Имя workflow совпадает
с кодом режима: для кнопки Z-Image Turbo это `z-image-turbo`.

Как это работает:

1. Пользователь нажимает «Z-Image Turbo Generation», «Z-Image Generation» или
   «Qwen 2.1 Turbo Generation».
2. Бот сохраняет выбранный режим для этого чата и открывает второй шаг: можно
   выбрать соотношение сторон и/или разрешение, либо сразу отправить промпт.
   По умолчанию — `1:1` и `1024 px` по большей стороне.
3. Следующее текстовое сообщение отправляется в `POST /prompt` в API-формате
   выбранного workflow с выбранными шириной и высотой. Перед постановкой бот
   запрашивает `GET /queue` и сообщает, сколько заданий сейчас выполняется и
   ожидает на сервере.
4. Бот опрашивает `GET /history/{prompt_id}` и `GET /queue`, скачивает первое
   изображение через `GET /view`. Таймаут ожидания — 8 часов. Если задача пропала
   из очереди и не появилась в истории дольше 90 секунд, бот возвращает ошибку
   вместо бесконечного ожидания.
5. Изображение загружается в Telegram: сначала отправляется обычное фото
   (Telegram сжимает его), затем тот же файл отправляется документом как
   оригинальный PNG без пережатия. Если фото отправить не удалось, оригинал
   всё равно будет доставлен.

### Qwen Edit Turbo

1. Пользователь нажимает «Qwen Edit Turbo».
2. Отправляет от 1 до 10 изображений: обычные фото или файлы PNG/JPEG/WebP/BMP/GIF.
   Бот скачивает их во временную папку и показывает счётчик принятых файлов.
3. Отправляет текстовый промпт. Кнопка «🧹 Очистить изображения» сбрасывает набор.
4. Бот загружает референсы через `POST /upload/image`, подставляет их в
   `LoadImage`-узлы и запускает `TextEncodeQwenImage21` + `SamplerCustomAdvanced`.
5. Результат отправляется так же: сжатое фото и оригинальный PNG.

В режиме редактирования не показываются выборы соотношения и разрешения:
размер результата берётся из референсов (с округлением до 32 пикселей).

### Wan Text to Video

1. Пользователь нажимает «Wan Text to Video» и отправляет промпт.
2. Бот запускает `wan-text` с фиксированными параметрами: 640×640, 81 кадр
   (≈5 секунд при 16 fps), 20 шагов high-noise + 10 шагов low-noise, CFG 3.5.
3. Готовый MP4 отправляется в Telegram как видеофайл с корректными атрибутами
   (ширина, высота, длительность, streaming).

Используется ветка без Lightning LoRA: вариант с LoRA быстрее, но на сервере
упирается в VRAM при патчинге fp8-моделей. Видео генерируется заметно дольше
изображений; общий таймаут ожидания — 8 часов, состояние очереди проверяется
каждые 5 секунд.

Соотношения: `1:1`, `4:3`, `3:4`, `3:2`, `2:3`, `16:9`, `9:16`.
Разрешения (большая сторона): `1024`, `1280`, `1536`, `1920`, `2560`, `4K (3840)`.

Меньшая сторона вычисляется из соотношения, округляется до кратного 16 и не
превышает выбранную большую сторону. Примеры для `1024`:

| Соотношение | Размер |
| --- | --- |
| `1:1` | 1024×1024 |
| `4:3` | 1024×768 |
| `3:4` | 768×1024 |
| `3:2` | 1024×688 |
| `2:3` | 688×1024 |
| `16:9` | 1024×576 |
| `9:16` | 576×1024 |

Параметры соответствуют workflow на сервере:

| Workflow | Модель | Сэмплер | Особенности |
| --- | --- | --- | --- |
| `z-image-turbo` | `z_image_turbo_bf16.safetensors` | 8 шагов, CFG 1, `res_multistep` + `simple` | negative zero-out |
| `z-image` | `z_image_bf16.safetensors` | 25 шагов, CFG 4, `res_multistep` + `simple` | negative — пустой промпт |
| `qwen-turbo` | `qwen_image_2.1_turbo_int8_convrot.safetensors` | 8 шагов, CFG 1, `euler` + `ManualSigmas` | `qwen3vl_8b_int8_convrot` + `qwen_image_2.1_vae_bf16` |
| `qwen-edit-turbo` | `qwen_image_2.1_turbo_int8_convrot.safetensors` | 8 шагов, CFG 1, `euler` + `ManualSigmas` | 1–10 референсов, `TextEncodeQwenImage21` |
| `wan-text` | `wan2.2_t2v_high/low_noise_14B_fp8_scaled` | 20+10 шагов, CFG 3.5, `euler` + `simple` | 640×640, 81 кадр, 16 fps, MP4 |

Общие параметры: для изображений разрешение берётся из выбранных соотношения и
большей стороны, seed выбирается случайно при каждом запросе. Z-Image-графы
используют `qwen_3_4b.safetensors`, `ae.safetensors` и `shift=3`; Qwen-граф —
`qwen3vl_8b_int8_convrot.safetensors` и `qwen_image_2.1_vae_bf16.safetensors`;
Wan-граф — `umt5_xxl_fp8_e4m3fn_scaled.safetensors` и `wan_2.1_vae.safetensors`.
У Qwen-графов prompt enhancer отключён; пользовательский промпт сразу попадает в
`TextEncodeQwenImage21`. UI-узлы из исходных workflow (Telegram, resolution
selector, заметки) не запускаются — доставкой занимается сам бот.

### Если Z-Image Base даёт чёрные изображения

Не запускайте ComfyUI с флагом `--force-fp16`: Z-Image base в fp16 схлопывается
в почти чёрную картинку, хотя Turbo с этим флагом работает. Флаг `--fp16-vae`
оставлять можно. Рабочий вариант запуска:

```sh
CUDA_VISIBLE_DEVICES=1 /data/.venv/bin/python main.py \
  --fp16-vae --use-pytorch-cross-attention --enable-manager --listen 0.0.0.0
```


Проверить связку с сервером можно реальными тестами (изображения — ~20–60
секунд, видео — дольше):

```sh
cargo test turbo_generates_an_image_with_the_requested_size -- --ignored --nocapture
cargo test base_generates_an_image_with_the_requested_size -- --ignored --nocapture
cargo test qwen_turbo_generates_an_image_with_the_requested_size -- --ignored --nocapture
cargo test qwen_edit_generates_from_uploaded_images -- --ignored --nocapture
cargo test wan_generates_a_video -- --ignored --nocapture
```

## Прокси

Бот умеет подключаться к Telegram через SOCKS5-прокси. Укажите `PROXY_URL`:

```dotenv
PROXY_URL=socks5://127.0.0.1:1080
```

С аутентификацией:

```dotenv
PROXY_URL=socks5://username:password@proxy.example.com:1080
```

Особенности:

- поддерживается только схема `socks5://` — HTTP/HTTPS-прокси grammers не умеет;
- если в URL указан домен, он резолвится локально, и используется первый полученный IP;
- при пустом или отсутствующем `PROXY_URL` бот подключается напрямую;
- URL с некорректной схемой отклоняется при старте с понятной ошибкой.

Строка `PROXY_URL` не логируется, чтобы не раскрывать логин и пароль.

## Запуск

```sh
cargo run --release
```

Затем отправьте боту `/start` в Telegram. Повторный `/start` (или `/menu`)
открывает меню заново. `Ctrl+C` корректно останавливает бота.

## Кросс-сборка под Linux (musl)

Собирается статический бинарь под Linux, которому не нужны glibc и системный
SQLite. На macOS сборка идёт через `zig cc`, отдельный Linux-тулчейн не нужен.

Установите инструменты один раз:

```sh
brew install zig
cargo install cargo-zigbuild
rustup target add x86_64-unknown-linux-musl
```

Соберите:

```sh
./scripts/build-linux-musl.sh
```

Результат:

```text
dist/comfy-bot-x86_64-unknown-linux-musl
```

Это `ELF 64-bit, statically linked, stripped` (~13 МБ). Для ARM64-сервера:

```sh
TARGET=aarch64-unknown-linux-musl ./scripts/build-linux-musl.sh
```

На сервере рядом с бинарём нужны `.env` и (после первого запуска) `bot.session`.
Сборка и загрузка одной командой:

```sh
make linux
```

Команда собирает бинарь и копирует его через SSH на `rig` в `/data/comfy-bot/comfy-bot`
(загрузка идёт во временный `.new`, затем атомарно переименовывается).
Параметры можно переопределить:

```sh
make linux REMOTE=rig REMOTE_DIR=/data/comfy-bot TARGET=aarch64-unknown-linux-musl
```

Только собрать, без сервера:

```sh
make build-linux
```

## Структура

| Файл | Назначение |
| --- | --- |
| `src/main.rs` | Telegram, состояние выбора, отправка результата |
| `src/menu.rs` | тексты и инлайн-клавиатуры меню |
| `src/mode.rs` | режимы, callback-коды и имена workflow |
| `src/aspect.rs` | соотношения сторон, разрешения и итоговые размеры |
| `src/comfy.rs` | клиент ComfyUI: очередь, polling истории, скачивание |

## Что дальше

Чтобы подключить оставшуюся кнопку Qwen 2.1, достаточно дать режиму `workflow()`
и добавить настройки графа в `ComfyClient::generate`. Имя workflow совпадает с
callback-кодом: `qwen`.
