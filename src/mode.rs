//! Generation modes exposed by the bot menu.

/// A generation mode the user can pick from the main menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    ZImageTurbo,
    ZImage,
    QwenTurbo,
    QwenEditTurbo,
    Qwen,
    Wan,
}

impl Mode {
    /// Modes in the order they are displayed in the menu.
    pub const ALL: [Mode; 6] = [
        Mode::ZImageTurbo,
        Mode::ZImage,
        Mode::QwenTurbo,
        Mode::QwenEditTurbo,
        Mode::Qwen,
        Mode::Wan,
    ];

    /// Stable identifier used in callback button payloads.
    ///
    /// For supported modes it is also the name of the ComfyUI workflow.
    pub fn code(self) -> &'static str {
        match self {
            Mode::ZImageTurbo => "z-image-turbo",
            Mode::ZImage => "z-image",
            Mode::QwenTurbo => "qwen-turbo",
            Mode::QwenEditTurbo => "qwen-edit-turbo",
            Mode::Qwen => "qwen",
            Mode::Wan => "wan-text",
        }
    }

    /// Human-readable title shown in the menu.
    pub fn label(self) -> &'static str {
        match self {
            Mode::ZImageTurbo => "Z-Image Turbo",
            Mode::ZImage => "Z-Image",
            Mode::QwenTurbo => "Qwen 2.1 Turbo",
            Mode::QwenEditTurbo => "Qwen Edit Turbo",
            Mode::Qwen => "Qwen 2.1",
            Mode::Wan => "Wan Text to Video",
        }
    }

    /// Parse a callback payload back into a mode.
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.code() == code)
    }

    /// ComfyUI workflow executed for this mode, if the bot supports it.
    pub fn workflow(self) -> Option<&'static str> {
        match self {
            Mode::ZImageTurbo
            | Mode::ZImage
            | Mode::QwenTurbo
            | Mode::QwenEditTurbo
            | Mode::Wan => Some(self.code()),
            Mode::Qwen => None,
        }
    }

    /// Whether the mode expects reference images before the prompt.
    pub fn requires_images(self) -> bool {
        matches!(self, Mode::QwenEditTurbo)
    }

    /// Whether the mode generates a video instead of a still image.
    pub fn is_video(self) -> bool {
        matches!(self, Mode::Wan)
    }

    /// Whether the aspect ratio and resolution keyboard applies.
    pub fn supports_size_selection(self) -> bool {
        matches!(self, Mode::ZImageTurbo | Mode::ZImage | Mode::QwenTurbo)
    }
}

#[cfg(test)]
mod tests {
    use super::Mode;

    #[test]
    fn every_mode_round_trips_through_its_code() {
        for mode in Mode::ALL {
            assert_eq!(Mode::from_code(mode.code()), Some(mode));
        }
    }

    #[test]
    fn unknown_code_is_rejected() {
        assert_eq!(Mode::from_code("nope"), None);
    }
}
