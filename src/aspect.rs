//! Aspect ratios and resolutions available for the Z-Image Turbo workflow.
//!
//! The resolution is the length of the larger side. The smaller side is
//! derived from the aspect ratio and rounded to 16 pixels, which is required
//! by `EmptySD3LatentImage`.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AspectRatio {
    #[default]
    Square,
    Landscape4x3,
    Portrait3x4,
    Landscape3x2,
    Portrait2x3,
    Landscape16x9,
    Portrait9x16,
}

impl AspectRatio {
    pub const ALL: [AspectRatio; 7] = [
        AspectRatio::Square,
        AspectRatio::Landscape4x3,
        AspectRatio::Portrait3x4,
        AspectRatio::Landscape3x2,
        AspectRatio::Portrait2x3,
        AspectRatio::Landscape16x9,
        AspectRatio::Portrait9x16,
    ];

    /// Stable callback payload value.
    pub fn code(self) -> &'static str {
        match self {
            AspectRatio::Square => "1:1",
            AspectRatio::Landscape4x3 => "4:3",
            AspectRatio::Portrait3x4 => "3:4",
            AspectRatio::Landscape3x2 => "3:2",
            AspectRatio::Portrait2x3 => "2:3",
            AspectRatio::Landscape16x9 => "16:9",
            AspectRatio::Portrait9x16 => "9:16",
        }
    }

    pub fn label(self) -> &'static str {
        self.code()
    }

    /// Width and height units of the ratio, e.g. `(16, 9)`.
    fn units(self) -> (u32, u32) {
        match self {
            AspectRatio::Square => (1, 1),
            AspectRatio::Landscape4x3 => (4, 3),
            AspectRatio::Portrait3x4 => (3, 4),
            AspectRatio::Landscape3x2 => (3, 2),
            AspectRatio::Portrait2x3 => (2, 3),
            AspectRatio::Landscape16x9 => (16, 9),
            AspectRatio::Portrait9x16 => (9, 16),
        }
    }

    /// Concrete image size where the larger side equals `resolution.max_side()`.
    pub fn size_for(self, resolution: Resolution) -> (u32, u32) {
        let (ratio_width, ratio_height) = self.units();
        let max_side = resolution.max_side();

        if ratio_width >= ratio_height {
            let height = max_side as f64 * ratio_height as f64 / ratio_width as f64;
            (max_side, align_to_16(height))
        } else {
            let width = max_side as f64 * ratio_width as f64 / ratio_height as f64;
            (align_to_16(width), max_side)
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|ratio| ratio.code() == code)
    }
}

/// Long side of the generated image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Resolution {
    #[default]
    R1024,
    R1280,
    R1536,
    R1920,
    R2560,
    R3840,
}

impl Resolution {
    pub const ALL: [Resolution; 6] = [
        Resolution::R1024,
        Resolution::R1280,
        Resolution::R1536,
        Resolution::R1920,
        Resolution::R2560,
        Resolution::R3840,
    ];

    /// Stable callback payload value.
    pub fn code(self) -> &'static str {
        match self {
            Resolution::R1024 => "1024",
            Resolution::R1280 => "1280",
            Resolution::R1536 => "1536",
            Resolution::R1920 => "1920",
            Resolution::R2560 => "2560",
            Resolution::R3840 => "3840",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Resolution::R3840 => "4K",
            other => other.code(),
        }
    }

    pub fn max_side(self) -> u32 {
        match self {
            Resolution::R1024 => 1024,
            Resolution::R1280 => 1280,
            Resolution::R1536 => 1536,
            Resolution::R1920 => 1920,
            Resolution::R2560 => 2560,
            Resolution::R3840 => 3840,
        }
    }

    pub fn description(self) -> String {
        match self {
            Resolution::R3840 => "4K (3840 px)".to_owned(),
            other => format!("{} px", other.max_side()),
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|resolution| resolution.code() == code)
    }
}

fn align_to_16(value: f64) -> u32 {
    ((value / 16.0).round() as u32).max(1) * 16
}

#[cfg(test)]
mod tests {
    use super::{AspectRatio, Resolution};

    #[test]
    fn every_ratio_round_trips_through_its_code() {
        for ratio in AspectRatio::ALL {
            assert_eq!(AspectRatio::from_code(ratio.code()), Some(ratio));
        }
    }

    #[test]
    fn every_resolution_round_trips_through_its_code() {
        for resolution in Resolution::ALL {
            assert_eq!(Resolution::from_code(resolution.code()), Some(resolution));
        }
    }

    #[test]
    fn dimensions_are_valid_for_every_combination() {
        for resolution in Resolution::ALL {
            for ratio in AspectRatio::ALL {
                let (width, height) = ratio.size_for(resolution);

                assert!(width.is_multiple_of(16));
                assert!(height.is_multiple_of(16));
                assert_eq!(width.max(height), resolution.max_side());
            }
        }
    }

    #[test]
    fn landscape_size_uses_the_long_side() {
        assert_eq!(
            AspectRatio::Landscape3x2.size_for(Resolution::R1024),
            (1024, 688)
        );
        assert_eq!(
            AspectRatio::Landscape16x9.size_for(Resolution::R3840),
            (3840, 2160)
        );
    }

    #[test]
    fn unknown_codes_are_rejected() {
        assert_eq!(AspectRatio::from_code("5:4"), None);
        assert_eq!(Resolution::from_code("2048"), None);
    }
}
