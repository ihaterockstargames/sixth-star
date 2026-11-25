//! Render size and output size, kept apart as a PC port does.
//!
//! - **Output** (`VICTORY_OUTPUT_MODE=WxH`): the host window and swap chain.
//!   Unset: they follow the title's presented image, as before.
//! - **Render scale** (`VICTORY_RENDER_SCALE`, 0.1 to 2.0): the title is told
//!   a display of the output size times this, so it renders below (or above)
//!   the output and the present scales the image to the window.
//! - **Display mode** (`VICTORY_DISPLAY_MODE=WxH`): the mode the title is
//!   told, given outright; it wins over the computed one.
//!
//! With nothing set, the title is told 1920x1080 and the window follows its
//! image: the behaviour before this module. Every size is rounded to whole
//! pixels and kept within 640..=7680 by 360..=4320. A value that does not
//! parse or is out of range is reported and replaced by its default; nothing
//! here panics. The first version assumes 16:9 (one scale for both axes).

use std::sync::OnceLock;

/// The smallest and largest sizes accepted, (width, height).
pub const MIN: (u32, u32) = (640, 360);
pub const MAX: (u32, u32) = (7680, 4320);
/// The size the title is told when nothing else is given.
pub const DEFAULT_DISPLAY: (u32, u32) = (1920, 1080);
/// The render scale's range.
pub const SCALE_RANGE: std::ops::RangeInclusive<f64> = 0.1..=2.0;

/// Where the display mode came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplaySource {
    /// `VICTORY_DISPLAY_MODE` given outright.
    Explicit,
    /// The output (or the default) times the render scale.
    Computed,
}

/// The resolved sizes.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    /// What the title is told (its display mode).
    pub display: (u32, u32),
    pub display_source: DisplaySource,
    /// The window and swap chain size; `None`: follow the presented image.
    pub output: Option<(u32, u32)>,
    pub scale: f64,
    /// Values that were refused, and why.
    pub warnings: Vec<String>,
}

/// `WxH` (also `W x H`, `WXH`), both parts whole numbers.
pub fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.trim().split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

fn in_range((w, h): (u32, u32)) -> bool {
    (MIN.0..=MAX.0).contains(&w) && (MIN.1..=MAX.1).contains(&h)
}

/// `size` times `scale`, rounded to whole pixels and clamped to the range.
pub fn scaled((w, h): (u32, u32), scale: f64) -> (u32, u32) {
    let one = |v: u32, lo: u32, hi: u32| ((f64::from(v) * scale).round() as u32).clamp(lo, hi);
    (one(w, MIN.0, MAX.0), one(h, MIN.1, MAX.1))
}

/// Resolves the three settings (each the variable's text, if set).
pub fn resolve(display: Option<&str>, output: Option<&str>, scale: Option<&str>) -> Resolution {
    let mut warnings = Vec::new();
    let mut size = |name: &str, text: Option<&str>| -> Option<(u32, u32)> {
        let text = text.map(str::trim).filter(|t| !t.is_empty())?;
        match parse_size(text) {
            Some(s) if in_range(s) => Some(s),
            Some(_) => {
                warnings.push(format!(
                    "{name}={text} is outside {}x{}..{}x{}; ignored",
                    MIN.0, MIN.1, MAX.0, MAX.1
                ));
                None
            }
            None => {
                warnings.push(format!("{name}={text} is not WxH; ignored"));
                None
            }
        }
    };
    let explicit = size("VICTORY_DISPLAY_MODE", display);
    let output = size("VICTORY_OUTPUT_MODE", output);
    let scale = match scale.map(str::trim).filter(|t| !t.is_empty()) {
        None => 1.0,
        Some(text) => match text.parse::<f64>() {
            Ok(s) if SCALE_RANGE.contains(&s) => s,
            _ => {
                warnings.push(format!("VICTORY_RENDER_SCALE={text} is not a number in 0.1..2.0; using 1.0"));
                1.0
            }
        },
    };
    let (display, display_source) = match explicit {
        Some(d) => (d, DisplaySource::Explicit),
        None => (scaled(output.unwrap_or(DEFAULT_DISPLAY), scale), DisplaySource::Computed),
    };
    Resolution { display, display_source, output, scale, warnings }
}

/// The process's resolution, from the environment, resolved and logged once.
pub fn get() -> &'static Resolution {
    static RESOLVED: OnceLock<Resolution> = OnceLock::new();
    RESOLVED.get_or_init(|| {
        let var = |k: &str| std::env::var(k).ok();
        let r = resolve(
            var("VICTORY_DISPLAY_MODE").as_deref(),
            var("VICTORY_OUTPUT_MODE").as_deref(),
            var("VICTORY_RENDER_SCALE").as_deref(),
        );
        for w in &r.warnings {
            tracing::warn!("{w}");
        }
        let output = r.output.map_or("the presented image".to_string(), |(w, h)| format!("{w}x{h}"));
        tracing::info!(
            "resolution: display {}x{} ({}), output {output}, render scale {}",
            r.display.0,
            r.display.1,
            match r.display_source {
                DisplaySource::Explicit => "from VICTORY_DISPLAY_MODE",
                DisplaySource::Computed => "output x render scale",
            },
            r.scale
        );
        r
    })
}

/// How a frame reaches the swap chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentPath {
    /// One whole texture of a swap chain format, copied as it is.
    Copy,
    /// Layers drawn at the size of the image (several layers, a part of a
    /// texture, or a format that needs converting).
    Draw,
    /// Drawn, scaled to an output of another size.
    Scale,
}

impl PresentPath {
    pub fn name(self) -> &'static str {
        match self {
            PresentPath::Copy => "copy",
            PresentPath::Draw => "draw",
            PresentPath::Scale => "scale",
        }
    }
}

/// The path and the swap chain size for a frame whose image is `source`.
/// `copyable`: one whole texture of a swap chain format. A copy needs the
/// output to be the image's own size.
pub fn present_path(copyable: bool, source: (u32, u32), output: Option<(u32, u32)>) -> (PresentPath, (u32, u32)) {
    let out = output.unwrap_or(source);
    let path = if out != source {
        PresentPath::Scale
    } else if copyable {
        PresentPath::Copy
    } else {
        PresentPath::Draw
    };
    (path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(display: Option<&str>, output: Option<&str>, scale: Option<&str>) -> Resolution {
        resolve(display, output, scale)
    }

    /// Nothing set: 1080p told, the window follows the image (as before).
    #[test]
    fn nothing_set_keeps_the_old_behaviour() {
        let x = r(None, None, None);
        assert_eq!((x.display, x.output, x.scale), ((1920, 1080), None, 1.0));
        assert!(x.warnings.is_empty());
        let x = r(Some(""), Some("  "), Some(""));
        assert_eq!((x.display, x.output, x.scale), ((1920, 1080), None, 1.0), "empty is unset");
    }

    /// The table of the feature's description.
    #[test]
    fn the_documented_examples() {
        let cases = [
            ("1920x1080", "1.0", (1920, 1080)),
            ("1920x1080", "0.6667", (1280, 720)),
            ("2560x1440", "0.75", (1920, 1080)),
            ("3840x2160", "0.5", (1920, 1080)),
        ];
        for (output, scale, told) in cases {
            let x = r(None, Some(output), Some(scale));
            assert_eq!(x.display, told, "{output} x {scale}");
            assert_eq!(x.output, parse_size(output));
            assert_eq!(x.display_source, DisplaySource::Computed);
            assert!(x.warnings.is_empty());
        }
    }

    /// An explicit display mode wins over the computed one.
    #[test]
    fn an_explicit_display_mode_wins() {
        let x = r(Some("2560x1440"), Some("1920x1080"), Some("0.5"));
        assert_eq!((x.display, x.display_source), ((2560, 1440), DisplaySource::Explicit));
        assert_eq!(x.output, Some((1920, 1080)));
        let x = r(Some("1280X720"), None, None);
        assert_eq!(x.display, (1280, 720), "upper-case X accepted");
    }

    /// A scale without an output scales the default display.
    #[test]
    fn a_scale_alone_scales_the_default() {
        assert_eq!(r(None, None, Some("0.5")).display, (960, 540));
    }

    /// Rounding is to the nearest pixel; results are clamped to the range.
    #[test]
    fn sizes_are_rounded_and_clamped() {
        assert_eq!(scaled((1920, 1080), 0.6667), (1280, 720));
        assert_eq!(scaled((1366, 768), 0.75), (1025, 576));
        assert_eq!(scaled((640, 360), 0.1), MIN, "never below the minimum");
        assert_eq!(scaled((7680, 4320), 2.0), MAX, "never above the maximum");
    }

    /// Bad values warn and fall back; nothing panics.
    #[test]
    fn bad_values_warn_and_fall_back() {
        for (display, output, scale) in [
            (Some("big"), None, None),
            (Some("100x100"), None, None),
            (None, Some("1920*1080"), None),
            (None, Some("99999x1080"), None),
            (None, None, Some("fast")),
            (None, None, Some("0")),
            (None, None, Some("3")),
            (None, None, Some("NaN")),
            (None, None, Some("-1")),
        ] {
            let x = r(display, output, scale);
            assert_eq!(x.warnings.len(), 1, "{display:?} {output:?} {scale:?}: {:?}", x.warnings);
            assert_eq!(x.display, (1920, 1080), "falls back to the default");
            assert_eq!(x.scale, 1.0);
        }
        assert_eq!(parse_size("1920x"), None);
        assert_eq!(parse_size("x1080"), None);
        assert_eq!(parse_size(" 1920 x 1080 "), Some((1920, 1080)));
    }

    /// Copy only when the output is the image's size; else scale.
    #[test]
    fn copy_or_scale() {
        use PresentPath::*;
        assert_eq!(present_path(true, (1920, 1080), None), (Copy, (1920, 1080)), "no output: as before");
        assert_eq!(present_path(true, (1920, 1080), Some((1920, 1080))), (Copy, (1920, 1080)));
        assert_eq!(present_path(true, (1280, 720), Some((1920, 1080))), (Scale, (1920, 1080)));
        assert_eq!(present_path(false, (1920, 1080), Some((1920, 1080))), (Draw, (1920, 1080)));
        assert_eq!(present_path(false, (1920, 1080), Some((2560, 1440))), (Scale, (2560, 1440)));
        assert_eq!(present_path(false, (1280, 720), None), (Draw, (1280, 720)));
    }
}
