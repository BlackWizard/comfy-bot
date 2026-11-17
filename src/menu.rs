//! Text and inline keyboards for the bot menu.

use grammers_client::message::{Button, ReplyMarkup};

use crate::{
    aspect::{AspectRatio, Resolution},
    mode::Mode,
};

/// Callback payload that brings the user back to the main menu.
pub const MENU_CALLBACK: &str = "menu";

/// Prefix for callback payloads that select a generation mode.
pub const MODE_CALLBACK_PREFIX: &str = "mode:";

/// Prefix for callback payloads that select an aspect ratio.
pub const RATIO_CALLBACK_PREFIX: &str = "ratio:";

/// Prefix for callback payloads that select a resolution.
pub const RESOLUTION_CALLBACK_PREFIX: &str = "res:";

/// Callback payload that clears uploaded reference images.
pub const CLEAR_IMAGES_CALLBACK: &str = "clear-images";

pub fn menu_text() -> &'static str {
    "Выберите режим генерации:"
}

/// Inline keyboard with one mode per row.
pub fn menu_markup() -> ReplyMarkup {
    let rows: Vec<Vec<Button>> = Mode::ALL
        .into_iter()
        .map(|mode| {
            vec![Button::data(
                mode.label(),
                format!("{MODE_CALLBACK_PREFIX}{}", mode.code()),
            )]
        })
        .collect();

    ReplyMarkup::from_buttons(&rows)
}

/// Text shown after the user picks a mode.
pub fn selected_text(
    mode: Mode,
    ratio: AspectRatio,
    resolution: Resolution,
    image_count: usize,
) -> String {
    if mode.requires_images() {
        return format!(
            "Выбран режим: {}\n\n\
             Отправьте от 1 до 10 изображений (фото или файлы), затем текстовый промпт.\n\
             Изображений: {image_count}/10",
            mode.label()
        );
    }

    if mode.is_video() {
        return format!(
            "Выбран режим: {}\n\n\
             Отправьте текстовый промпт.\n\
             Видео: 640×640, 5 секунд, 16 fps.",
            mode.label()
        );
    }

    if mode.workflow().is_some() {
        let (width, height) = ratio.size_for(resolution);

        return format!(
            "Выбран режим: {}\n\
             Соотношение сторон: {}\n\
             Разрешение (большая сторона): {}\n\
             Итог: {width}×{height}\n\n\
             Выберите параметры или сразу отправьте текстовый промпт.",
            mode.label(),
            ratio.label(),
            resolution.description()
        );
    }

    format!(
        "Выбран режим: {}\n\nГенерация в этом режиме пока не подключена.",
        mode.label()
    )
}

/// Keyboard shown on the selected-mode screen.
pub fn selected_markup(mode: Mode, ratio: AspectRatio, resolution: Resolution) -> ReplyMarkup {
    if mode.requires_images() {
        return ReplyMarkup::from_buttons(&[
            vec![Button::data(
                "🧹 Очистить изображения",
                CLEAR_IMAGES_CALLBACK,
            )],
            vec![Button::data("← Назад", MENU_CALLBACK)],
        ]);
    }

    if !mode.supports_size_selection() {
        return ReplyMarkup::from_buttons_row(&[Button::data("← Назад", MENU_CALLBACK)]);
    }

    if mode.workflow().is_none() {
        return ReplyMarkup::from_buttons_row(&[Button::data("← Назад", MENU_CALLBACK)]);
    }

    let ratio_rows = AspectRatio::ALL.chunks(3).map(|chunk| {
        chunk
            .iter()
            .map(|candidate| ratio_button(*candidate, ratio))
            .collect()
    });

    let resolution_rows = Resolution::ALL.chunks(3).map(|chunk| {
        chunk
            .iter()
            .map(|candidate| resolution_button(*candidate, resolution))
            .collect()
    });

    let mut rows: Vec<Vec<Button>> = ratio_rows.chain(resolution_rows).collect();
    rows.push(vec![Button::data("← Назад", MENU_CALLBACK)]);

    ReplyMarkup::from_buttons(&rows)
}

fn ratio_button(candidate: AspectRatio, selected: AspectRatio) -> Button {
    let text = if candidate == selected {
        format!("✅ {}", candidate.label())
    } else {
        candidate.label().to_owned()
    };

    Button::data(text, format!("{RATIO_CALLBACK_PREFIX}{}", candidate.code()))
}

fn resolution_button(candidate: Resolution, selected: Resolution) -> Button {
    let text = if candidate == selected {
        format!("✅ {}", candidate.label())
    } else {
        candidate.label().to_owned()
    };

    Button::data(
        text,
        format!("{RESOLUTION_CALLBACK_PREFIX}{}", candidate.code()),
    )
}
