//! The single overlay renderer, shared by the layer-shell and X11 presenters so
//! the indicator looks identical on every session.
//!
//! [`render`] is a pure function of `(frame, scale)`: a rounded card with the
//! state line, the streaming transcript and a level bar, rasterised with
//! `tiny-skia` and shaped with `cosmic-text` the same way on every call. The font
//! is the embedded Inter Regular (no runtime font lookup), the text is shaped with
//! a process-wide [`FontSystem`] that is created once, and every glyph is blended
//! into the pixmap by hand so the source-over arithmetic is premultiplied-correct
//! for `wl_shm`/`ZPixmap` uploads.

use anyhow::{Context, Result};
use cosmic_text::{Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Metrics};
use cosmic_text::{Color, Shaping, SwashCache, Wrap};
use std::sync::{LazyLock, Mutex};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

use crate::overlay::{OverlayFrame, OverlayState};
use crate::ui::{metrics, palette};

/// Logical overlay size in device-independent pixels.
pub const OVERLAY_W: u32 = 560;
pub const OVERLAY_H: u32 = 112;
/// Distance between the bottom edge of the monitor and the card.
pub const BOTTOM_MARGIN: i32 = 48;
/// Corner radius of the card: the floating-overlay step of the shared radius
/// system.
pub const CORNER_RADIUS: f32 = metrics::RADIUS_OVERLAY;

/// The embedded UI font (Inter Regular, SIL OFL 1.1 — see `assets/Inter-LICENSE.txt`).
pub const FONT: &[u8] = include_bytes!("../assets/Inter-Regular.ttf");

/// Padding between the card edge and its content.
const PAD: f32 = 20.0;
/// Alpha the card is drawn with over whatever is behind it (`0.90 * 255`).
const CARD_ALPHA: u8 = 230;
/// The card fill: the app surface role, mostly opaque.
fn card_rgba() -> (u8, u8, u8, u8) {
    let (r, g, b) = palette::rgb8(palette::SURFACE);
    (r, g, b, CARD_ALPHA)
}
/// The status line: the muted text role.
fn status_rgb() -> (u8, u8, u8) {
    palette::rgb8(palette::MUTED)
}
/// The streaming transcript: the primary text role.
fn text_rgb() -> (u8, u8, u8) {
    palette::rgb8(palette::TEXT)
}
/// The level bar: the accent role, the error role while the state is an error.
fn level_rgb(state: &OverlayState) -> (u8, u8, u8) {
    match state {
        OverlayState::Error(_) => palette::rgb8(palette::ERROR),
        _ => palette::rgb8(palette::ACCENT),
    }
}

const STATUS_FONT_SIZE: f32 = 15.0;
const STATUS_LINE_HEIGHT: f32 = 18.0;
const TEXT_FONT_SIZE: f32 = 17.0;
const TEXT_LINE_HEIGHT: f32 = 20.0;
const MAX_TEXT_LINES: usize = 2;
/// Top edge of the status line box.
const STATUS_TOP: f32 = 16.0;
/// Top edge of the transcript box (two lines of [`TEXT_LINE_HEIGHT`]).
const TEXT_TOP: f32 = 40.0;
const LEVEL_HEIGHT: f32 = 8.0;
/// Top edge of the level bar, `PAD` above the bottom edge.
const LEVEL_TOP: f32 = OVERLAY_H as f32 - PAD - LEVEL_HEIGHT;

/// Smallest renderable scale factor.
const MIN_SCALE: i32 = 1;
/// Guard against absurd buffer sizes if a caller passes a nonsense scale.
const MAX_SCALE: i32 = 8;
/// Family name declared by the embedded font; only a fallback for the name read
/// back from the font database.
const FONT_FAMILY: &str = "Inter";

/// The status line shown at the top of the card. Every literal here is a
/// user-visible contract string.
pub fn state_line(state: &OverlayState, elapsed: std::time::Duration) -> String {
    match state {
        OverlayState::Recording => {
            let seconds = elapsed.as_secs();
            format!("Listening… {}:{:02}", seconds / 60, seconds % 60)
        }
        OverlayState::Transcribing => "Transcribing…".to_owned(),
        OverlayState::Queued => "Queued…".to_owned(),
        OverlayState::Inserting => "Inserting…".to_owned(),
        OverlayState::NoSpeech => "No speech detected".to_owned(),
        OverlayState::Notice(message) => message.clone(),
        OverlayState::Error(message) => message.clone(),
    }
}

/// Render one frame into a device-pixel pixmap of `OVERLAY_W * scale` by
/// `OVERLAY_H * scale`. `scale` is clamped to `1..=8` and `frame.level` to
/// `0.0..=1.0`; empty text, very long text and out-of-range levels are all
/// handled without panicking.
pub fn render(frame: &OverlayFrame, scale: i32) -> Result<Pixmap> {
    let scale = scale.clamp(MIN_SCALE, MAX_SCALE);
    let scale_px = scale as f32;
    let width = OVERLAY_W * scale as u32;
    let height = OVERLAY_H * scale as u32;
    let mut pixmap = Pixmap::new(width, height)
        .with_context(|| format!("cannot allocate a {width}×{height} overlay pixmap"))?;

    draw_card(&mut pixmap, scale_px);

    let content_width = width as f32 - 2.0 * PAD * scale_px;
    let mut cache = FONT_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let family = cache.family.clone();

    // Status line: a single line, elided rather than overflowing the card.
    let status = state_line(&frame.state, frame.elapsed);
    let status_attrs = text_attrs(&family, status_rgb());
    let status_metrics = Metrics::new(STATUS_FONT_SIZE * scale_px, STATUS_LINE_HEIGHT * scale_px);
    let mut status_buffer = Buffer::new(&mut cache.font_system, status_metrics);
    status_buffer.set_size(Some(content_width), None);
    status_buffer.set_wrap(Wrap::None);
    status_buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
    status_buffer.set_text(&status, &status_attrs, Shaping::Advanced, None);
    let status_at = (PAD * scale_px, STATUS_TOP * scale_px);
    cache.blit(
        &mut status_buffer,
        &mut pixmap,
        status_at,
        rgb(status_rgb()),
    );

    // Streaming transcript: word-wrapped to two lines, elided with "…" beyond.
    if !frame.text.is_empty() {
        let stream_attrs = text_attrs(&family, text_rgb());
        let stream_metrics = Metrics::new(TEXT_FONT_SIZE * scale_px, TEXT_LINE_HEIGHT * scale_px);
        let mut stream_buffer = Buffer::new(&mut cache.font_system, stream_metrics);
        stream_buffer.set_size(Some(content_width), None);
        stream_buffer.set_wrap(Wrap::WordOrGlyph);
        stream_buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(MAX_TEXT_LINES)));
        stream_buffer.set_text(&frame.text, &stream_attrs, Shaping::Advanced, None);
        let stream_at = (PAD * scale_px, TEXT_TOP * scale_px);
        cache.blit(&mut stream_buffer, &mut pixmap, stream_at, rgb(text_rgb()));
    }

    draw_level_bar(&mut pixmap, frame, scale_px, content_width);

    Ok(pixmap)
}

/// The pixmap as an `ARGB8888`/`ZPixmap` buffer: premultiplied BGRA bytes
/// (little-endian 32-bit with alpha in the high byte).
pub fn premultiplied_bgra(pixmap: &Pixmap) -> Vec<u8> {
    let mut buffer = pixmap.data().to_vec();
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    buffer
}

fn rgb(color: (u8, u8, u8)) -> Color {
    Color::rgb(color.0, color.1, color.2)
}

fn text_attrs(family: &str, color: (u8, u8, u8)) -> Attrs<'_> {
    Attrs::new().family(Family::Name(family)).color(rgb(color))
}

fn draw_card(pixmap: &mut Pixmap, scale: f32) {
    let width = pixmap.width() as f32;
    let height = pixmap.height() as f32;
    let Some(rect) = Rect::from_xywh(0.0, 0.0, width, height) else {
        return;
    };
    let Some(path) = rounded_rect_path(rect, CORNER_RADIUS * scale) else {
        return;
    };
    let mut paint = Paint::default();
    let (r, g, b, a) = card_rgba();
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn draw_level_bar(pixmap: &mut Pixmap, frame: &OverlayFrame, scale: f32, content_width: f32) {
    let width = content_width * frame.level.clamp(0.0, 1.0);
    if width <= 0.0 {
        return;
    }
    let height = LEVEL_HEIGHT * scale;
    let Some(rect) = Rect::from_xywh(PAD * scale, LEVEL_TOP * scale, width, height) else {
        return;
    };
    let Some(path) = rounded_rect_path(rect, height / 2.0) else {
        return;
    };
    let (r, g, b) = level_rgb(&frame.state);
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, 255);
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// Source-over blend of a straight-alpha source onto a premultiplied pixel.
fn blend_pixel(data: &mut [u8], index: usize, source: (u8, u8, u8, u8)) {
    let (r, g, b, a) = source;
    let source_alpha = f32::from(a) / 255.0;
    let inverse = 1.0 - source_alpha;
    let destination_alpha = f32::from(data[index + 3]) / 255.0;
    for (offset, channel) in [r, g, b].into_iter().enumerate() {
        let destination = f32::from(data[index + offset]) / 255.0;
        let blended = f32::from(channel) / 255.0 * source_alpha + destination * inverse;
        data[index + offset] = to_byte(blended);
    }
    data[index + 3] = to_byte(source_alpha + destination_alpha * inverse);
}

fn to_byte(value: f32) -> u8 {
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

/// A rectangle with quadratic corners. Zero-sized rectangles are rejected so
/// callers can treat `None` as "draw nothing".
fn rounded_rect_path(rect: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let radius = radius.clamp(0.0, rect.width().min(rect.height()) / 2.0);
    if radius <= 0.0 {
        return Some(PathBuilder::from_rect(rect));
    }
    let (left, top, right, bottom) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let mut builder = PathBuilder::new();
    builder.move_to(left + radius, top);
    builder.line_to(right - radius, top);
    builder.quad_to(right, top, right, top + radius);
    builder.line_to(right, bottom - radius);
    builder.quad_to(right, bottom, right - radius, bottom);
    builder.line_to(left + radius, bottom);
    builder.quad_to(left, bottom, left, bottom - radius);
    builder.line_to(left, top + radius);
    builder.quad_to(left, top, left + radius, top);
    builder.close();
    builder.finish()
}

/// Text shaping state shared by every frame: one [`FontSystem`] holding the
/// embedded font plus a [`SwashCache`] of rasterised glyphs. Creating the font
/// system per frame would rescan the installed fonts ten times a second.
struct FontCache {
    font_system: FontSystem,
    swash: SwashCache,
    /// Family name of the embedded face, read back from the font database.
    family: String,
}

static FONT_CACHE: LazyLock<Mutex<FontCache>> =
    LazyLock::new(|| Mutex::new(FontCache::load_embedded_font()));

impl FontCache {
    fn load_embedded_font() -> Self {
        let mut font_system = FontSystem::new();
        let family = embedded_family(&mut font_system);
        tracing::debug!(family = %family, "overlay font ready");
        Self {
            font_system,
            swash: SwashCache::new(),
            family,
        }
    }

    /// Shape and blit `buffer` with its top-left line box at `at`, blending every
    /// glyph into `pixmap` through `color` (overridden per glyph by its attributes).
    fn blit(&mut self, buffer: &mut Buffer, pixmap: &mut Pixmap, at: (f32, f32), color: Color) {
        let Self {
            font_system, swash, ..
        } = self;
        let offset_x = at.0.round() as i32;
        let offset_y = at.1.round() as i32;
        let width = pixmap.width() as i32;
        let height = pixmap.height() as i32;
        let data = pixmap.data_mut();
        buffer.draw(font_system, swash, color, |x, y, w, h, glyph_color| {
            let (r, g, b, a) = glyph_color.as_rgba_tuple();
            if a == 0 {
                return;
            }
            for row in 0..h as i32 {
                let target_y = y + offset_y + row;
                if target_y < 0 || target_y >= height {
                    continue;
                }
                for column in 0..w as i32 {
                    let target_x = x + offset_x + column;
                    if target_x < 0 || target_x >= width {
                        continue;
                    }
                    let index = (target_y as usize * width as usize + target_x as usize) * 4;
                    blend_pixel(data, index, (r, g, b, a));
                }
            }
        });
    }
}

/// Load the embedded font and read its family name back from the database, so the
/// name is never hard-coded twice.
fn embedded_family(font_system: &mut FontSystem) -> String {
    let database = font_system.db_mut();
    database.load_font_data(FONT.to_vec());
    let mut embedded = None;
    let mut last = None;
    for face in database.faces() {
        let Some((name, _)) = face.families.first() else {
            continue;
        };
        if name == FONT_FAMILY {
            embedded = Some(name.clone());
        }
        last = Some(name.clone());
    }
    embedded.or(last).unwrap_or_else(|| FONT_FAMILY.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_frame(state: OverlayState, elapsed: Duration, level: f32, text: &str) -> OverlayFrame {
        OverlayFrame {
            state,
            elapsed,
            level,
            text: text.to_owned(),
        }
    }

    /// Count opaque pixels of exactly `color` in a premultiplied RGBA buffer.
    fn opaque_pixels(data: &[u8], color: (u8, u8, u8)) -> usize {
        let (r, g, b) = color;
        data.as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] == r && pixel[1] == g && pixel[2] == b && pixel[3] == 255)
            .count()
    }

    #[test]
    fn state_line_renderings_are_exact() {
        let listening = |secs| state_line(&OverlayState::Recording, Duration::from_secs(secs));
        assert_eq!(listening(0), "Listening… 0:00");
        assert_eq!(listening(7), "Listening… 0:07");
        assert_eq!(listening(63), "Listening… 1:03");
        assert_eq!(listening(599), "Listening… 9:59");
        assert_eq!(listening(3600), "Listening… 60:00");
        assert_eq!(state_line(&OverlayState::Queued, Duration::ZERO), "Queued…");
        assert_eq!(
            state_line(&OverlayState::Transcribing, Duration::ZERO),
            "Transcribing…"
        );
        assert_eq!(
            state_line(&OverlayState::Inserting, Duration::ZERO),
            "Inserting…"
        );
        assert_eq!(
            state_line(&OverlayState::NoSpeech, Duration::ZERO),
            "No speech detected"
        );
    }

    #[test]
    fn error_state_line_passes_the_message_through() {
        let message = "Auto-paste blocked: /dev/uinput (Permission denied)";
        let state = OverlayState::Error(message.to_owned());
        assert_eq!(state_line(&state, Duration::from_secs(4)), message);
        assert_eq!(state_line(&state, Duration::ZERO), message);
    }

    #[test]
    fn notice_line_passes_the_message_through_without_error_styling() {
        let message = "Downloading Nemotron 3.5 ASR Streaming 0.6B — Q8_0 (751 MB), \
                       quality note: Multilingual — now 42%";
        let state = OverlayState::Notice(message.to_owned());
        assert_eq!(state_line(&state, Duration::from_secs(4)), message);
        assert_eq!(state_line(&state, Duration::ZERO), message);

        // The level bar of a notice keeps the normal colour: progress and
        // clipboard-degradation reports must not look like a failure.
        let frame = test_frame(
            OverlayState::Notice(message.to_owned()),
            Duration::ZERO,
            1.0,
            "",
        );
        let pixmap = render(&frame, 1).expect("render succeeds");
        assert!(opaque_pixels(pixmap.data(), palette::rgb8(palette::ACCENT)) > 0);
        assert_eq!(
            opaque_pixels(pixmap.data(), palette::rgb8(palette::ERROR)),
            0
        );
    }

    #[test]
    fn render_sizes_follow_the_scale() {
        for scale in 1..=4 {
            let frame = test_frame(OverlayState::Inserting, Duration::ZERO, 0.0, "");
            let pixmap = render(&frame, scale).expect("render succeeds");
            assert_eq!(pixmap.width(), OVERLAY_W * scale as u32);
            assert_eq!(pixmap.height(), OVERLAY_H * scale as u32);
            let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
            assert_eq!(pixmap.data().len(), w * h * 4);
        }

        // Nonsense scales are clamped instead of panicking.
        let frame = test_frame(OverlayState::Queued, Duration::ZERO, 0.0, "");
        let zero = render(&frame, 0).expect("render succeeds");
        assert_eq!(zero.width(), OVERLAY_W);
        let huge = render(&frame, 99).expect("render succeeds");
        assert_eq!(huge.width(), OVERLAY_W * MAX_SCALE as u32);
    }

    #[test]
    fn recording_frame_with_text_draws_the_level_bar() {
        let frame = test_frame(
            OverlayState::Recording,
            Duration::from_secs(7),
            0.5,
            "hello",
        );
        let pixmap = render(&frame, 1).expect("render succeeds");
        assert!(opaque_pixels(pixmap.data(), palette::rgb8(palette::ACCENT)) > 0);
    }

    #[test]
    fn error_state_uses_the_error_colour() {
        let state = OverlayState::Error("Auto-paste blocked".to_owned());
        let frame = test_frame(state, Duration::ZERO, 1.0, "");
        let pixmap = render(&frame, 1).expect("render succeeds");
        assert!(opaque_pixels(pixmap.data(), palette::rgb8(palette::ERROR)) > 0);
        assert_eq!(
            opaque_pixels(pixmap.data(), palette::rgb8(palette::ACCENT)),
            0
        );
    }

    #[test]
    fn level_changes_the_rendered_pixels() {
        let silent = test_frame(OverlayState::Recording, Duration::ZERO, 0.0, "hello");
        let loud = test_frame(OverlayState::Recording, Duration::ZERO, 1.0, "hello");
        let silent = render(&silent, 1).expect("render succeeds");
        let loud = render(&loud, 1).expect("render succeeds");
        assert_ne!(silent.data(), loud.data());
        assert_eq!(
            opaque_pixels(silent.data(), palette::rgb8(palette::ACCENT)),
            0
        );
        assert!(opaque_pixels(loud.data(), palette::rgb8(palette::ACCENT)) > 0);

        // Out-of-range levels are clamped instead of panicking.
        let under = test_frame(OverlayState::Recording, Duration::ZERO, -3.0, "hi");
        let under = render(&under, 1).expect("render succeeds");
        assert_eq!(
            opaque_pixels(under.data(), palette::rgb8(palette::ACCENT)),
            0
        );
        let over = test_frame(OverlayState::Recording, Duration::ZERO, 12.0, "hi");
        let over = render(&over, 1).expect("render succeeds");
        assert!(opaque_pixels(over.data(), palette::rgb8(palette::ACCENT)) > 0);
    }

    #[test]
    fn very_long_text_does_not_panic() {
        let long = "wort ".repeat(1000);
        for scale in [1, 2] {
            let frame = test_frame(OverlayState::Recording, Duration::from_secs(90), 0.4, &long);
            let pixmap = render(&frame, scale).expect("render succeeds");
            assert_eq!(pixmap.width(), OVERLAY_W * scale as u32);
        }
        // Text without spaces and full-width characters are equally safe.
        let unicode = "Ω".repeat(5000);
        let frame = test_frame(OverlayState::Transcribing, Duration::ZERO, 0.0, &unicode);
        assert!(render(&frame, 1).is_ok());
    }

    #[test]
    fn empty_transcript_render_is_not_panicking() {
        let frame = test_frame(OverlayState::NoSpeech, Duration::ZERO, 0.0, "");
        let pixmap = render(&frame, 1).expect("render succeeds");
        assert_eq!(pixmap.width(), OVERLAY_W);
        assert_eq!(pixmap.height(), OVERLAY_H);
    }

    #[test]
    fn premultiplied_bgra_is_bgra_with_transparent_corners() {
        let frame = test_frame(OverlayState::Recording, Duration::from_secs(2), 0.5, "hi");
        let pixmap = render(&frame, 2).expect("render succeeds");
        let bytes = premultiplied_bgra(&pixmap);
        let expected = (pixmap.width() * pixmap.height() * 4) as usize;
        assert_eq!(bytes.len(), expected);

        // The rounded corner lies outside the card: premultiplied transparent.
        assert_eq!(&bytes[0..4], [0u8, 0, 0, 0].as_slice());

        // The accent bar survives the channel swap as B, G, R, A.
        let (r, g, b) = palette::rgb8(palette::ACCENT);
        let accent = [b, g, r, 255];
        let found = bytes.as_chunks::<4>().0.contains(&accent);
        assert!(found);
    }
}
