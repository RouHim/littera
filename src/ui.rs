//! Shared iced surfaces: messages, window plumbing, chord capture and the
//! vocabulary both UI modules build on.

use std::sync::LazyLock;

use iced::font::{Family, Weight};
use iced::keyboard::{Event as KeyEvent, key};
use iced::widget::text::Wrapping;
use iced::widget::{button, container, progress_bar, scrollable, text};
use iced::{Background, Element, Font, Length, Padding, Theme, border, window};

use crate::app::{App, AppEvent, ModelStatus};
use crate::audio;
use crate::config::{Config, Language};
use crate::diagnostics::Check;
use crate::hotkey::{self, KeyToken, Modifier};
use crate::model_catalog::{self, Selection};
use crate::models;

/// The embedded application icon: the 256-px rendering of the icon master
/// (`assets/icon/hicolor/scalable/apps/littera.svg`), and the single source for
/// both the tray pixmap and the window icon (FR-005).
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon/hicolor/256x256/apps/littera.png");

/// The embedded text face, regular weight — Atkinson Hyperlegible (SIL OFL 1.1,
/// see `assets/AtkinsonHyperlegible-LICENSE.txt`), drawn by the Braille
/// Institute so that letters stay distinguishable at small sizes. Registered
/// with iced in `run_daemon`; the overlay rasteriser embeds the same file.
pub const FONT_REGULAR: &[u8] = include_bytes!("../assets/AtkinsonHyperlegible-Regular.ttf");

/// The embedded text face, bold weight. The only second weight the interface
/// uses: hierarchy comes from weight and space, never from a bigger ladder.
pub const FONT_BOLD: &[u8] = include_bytes!("../assets/AtkinsonHyperlegible-Bold.ttf");

/// Decode a PNG into straight (non-premultiplied) RGBA8 plus its dimensions.
///
/// Returns `None` for anything that is not an 8-bit RGBA PNG: the asset is a
/// build-time constant, so that is a bug in the repository rather than a
/// runtime condition, and callers fall back to "no icon".
fn decode_icon(png: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    if frame.color_type != png::ColorType::Rgba || frame.bit_depth != png::BitDepth::Eight {
        tracing::warn!(?frame.color_type, "embedded icon is not 8-bit RGBA");
        return None;
    }
    pixels.truncate(frame.buffer_size());
    Some((pixels, frame.width, frame.height))
}

/// The embedded application icon decoded once on first use.
pub(crate) fn icon_rgba() -> Option<&'static (Vec<u8>, u32, u32)> {
    static PIXELS: LazyLock<Option<(Vec<u8>, u32, u32)>> = LazyLock::new(|| decode_icon(ICON_PNG));

    PIXELS.as_ref()
}

/// Everything the views can produce.
#[derive(Debug, Clone)]
pub enum Message {
    /// A background event (engine, capture, hotkey, tray, IPC, timers).
    Event(AppEvent),
    /// A window finished opening.
    WindowOpened(window::Id),
    /// A window was closed by the user.
    WindowClosed(window::Id),
    /// A raw keyboard event, only subscribed while a chord is being captured.
    Key(KeyEvent),
    /// Show the onboarding surface (first run).
    OpenOnboarding,
    /// A message from the onboarding surface.
    Onboarding(crate::ui_onboarding::Action),
    /// A message from the settings surface.
    Settings(crate::ui_settings::Action),
    /// A message from the startup problem notice.
    Problems(crate::ui_problems::Action),
}

/// The three windows the daemon can own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Onboarding,
    Settings,
    /// The startup problem notice (FR-014).
    Problems,
}

/// The app palette: one warm-paper foundation for both windows and the overlay
/// indicator (`overlay_render` reads the same constants through
/// [`palette::rgb8`]).
///
/// The six roles map onto [`iced::theme::Palette`] (`background`, `text`,
/// `primary`, `success`, `warning`, `danger`); the sheet, well and rules are the
/// surfaces the views build with. Contrast on [`palette::BACKGROUND`]: ink
/// 13.8:1, muted 6.0:1, accent 4.8:1, warning 5.5:1, success 5.9:1, failure
/// 7.9:1 — all above WCAG AA, and `ON_ACCENT` (4.9:1) is the only ink allowed on
/// an accent fill.
pub mod palette {
    use iced::Color;

    // Surfaces.
    /// Window background: warm paper.
    pub const BACKGROUND: Color = Color::from_rgb8(0xFB, 0xF7, 0xEE);
    /// A sheet laid on the paper: cards, rows and the capability grid.
    pub const SURFACE: Color = Color::from_rgb8(0xFF, 0xFD, 0xF6);
    /// Recessed interiors: search fields, pickers, the sort chips.
    pub const WELL: Color = Color::from_rgb8(0xF1, 0xEA, 0xDA);
    /// Hairline rules: the field grid between rows.
    pub const BORDER: Color = Color::from_rgb8(0xE7, 0xDE, 0xCB);
    /// Rules that carry structure (hover, a selected sheet, the capability grid).
    pub const BORDER_STRONG: Color = Color::from_rgb8(0xC9, 0xBC, 0xA3);

    // Text tones.
    /// Primary ink: names, values, sentences.
    pub const TEXT: Color = Color::from_rgb8(0x2C, 0x27, 0x21);
    /// Secondary ink: keys, hints, explanations. 6:1 on the paper.
    pub const MUTED: Color = Color::from_rgb8(0x66, 0x5E, 0x52);

    // Signal and semantic roles.
    /// The one crayon colour: the live row, the primary action, the level bar.
    pub const ACCENT: Color = Color::from_rgb8(0xC0, 0x44, 0x2F);
    pub const ACCENT_HOVER: Color = Color::from_rgb8(0xAE, 0x3A, 0x25);
    pub const ACCENT_PRESSED: Color = Color::from_rgb8(0x9C, 0x33, 0x1F);
    /// The ink an accent fill carries; paper white on the crayon is 3.8:1.
    pub const ON_ACCENT: Color = Color::from_rgb8(0xFF, 0xF9, 0xF0);
    /// Done and verified.
    pub const SUCCESS: Color = Color::from_rgb8(0x2E, 0x6B, 0x45);
    /// Caution that is not an action: the model/language mismatch.
    pub const WARNING: Color = Color::from_rgb8(0x8A, 0x5A, 0x12);
    /// Failure text — always the readable ink, never the fill.
    pub const ERROR: Color = Color::from_rgb8(0x8E, 0x2A, 0x20);
    /// The stop red for fills and heavy bars; text uses [`ERROR`].
    pub const ERROR_FILL: Color = Color::from_rgb8(0xA6, 0x2B, 0x21);

    /// [`Color`] with a different alpha channel.
    pub const fn alpha(color: Color, a: f32) -> Color {
        Color {
            r: color.r,
            g: color.g,
            b: color.b,
            a,
        }
    }

    /// The opaque 8-bit channels of a colour, for the CPU rasteriser
    /// (`overlay_render`) that cannot take an [`iced::Color`].
    pub fn rgb8(color: Color) -> (u8, u8, u8) {
        let channel = |value: f32| (value * 255.0).round().clamp(0.0, 255.0) as u8;
        (channel(color.r), channel(color.g), channel(color.b))
    }
}

/// Layout metrics in logical pixels: one spacing scale, one radius system, one
/// type scale. Every `spacing`, `padding`, `size` and `width` in the views comes
/// from here.
pub mod metrics {
    // Spacing: 4 / 8 / 12 / 16 / 24 / 36. More air above a heading than below it.
    pub const SPACE_XS: f32 = 4.0;
    pub const SPACE_SM: f32 = 8.0;
    pub const SPACE_MD: f32 = 12.0;
    pub const SPACE_LG: f32 = 16.0;
    pub const SPACE_XL: f32 = 24.0;
    /// The gap between settings sections.
    pub const SPACE_XXL: f32 = 36.0;

    // Shape: sheets and controls are softly rounded; only the floating overlay
    // pill and the onboarding step chips are fully round.
    pub const RADIUS_CONTROL: f32 = 10.0;
    pub const RADIUS_SURFACE: f32 = 14.0;
    /// The floating overlay indicator (`overlay_render`).
    pub const RADIUS_OVERLAY: f32 = 16.0;
    pub const RADIUS_PILL: f32 = 999.0;

    // Type scale, logical pixels: four steps, hierarchy carried by weight and
    // space. Atkinson Hyperlegible's tall x-height keeps 14 px legible.
    pub const TEXT_CAPTION: f32 = 14.0;
    pub const TEXT_BODY: f32 = 17.0;
    pub const TEXT_LEAD: f32 = 19.0;
    pub const TEXT_HEADING: f32 = 20.0;
    pub const TEXT_TITLE: f32 = 26.0;
    /// Button and chip labels: a step between caption and body.
    pub const TEXT_CONTROL: f32 = 15.0;

    /// Reading line height for prose and explanations.
    pub const LINE_HEIGHT: f32 = 1.5;
    /// Tighter line height for titles and headings.
    pub const LINE_HEIGHT_TIGHT: f32 = 1.25;

    /// The label column of a labelled row: the key locked, the value aligned.
    pub const FIELD_LABEL_WIDTH: f32 = 150.0;

    /// The padding of a labelled button: `[vertical, horizontal]`.
    pub const BUTTON_PADDING: [f32; 2] = [SPACE_SM + 2.0, SPACE_LG + 2.0];

    // Scrollbar groove and scroller widths.
    pub const SCROLLBAR_WIDTH: f32 = 10.0;
    pub const SCROLLBAR_SCROLLER: f32 = 6.0;
}

/// The style vocabulary: every widget in both windows styles all of its statuses
/// from [`palette`], so nothing is left at a built-in default.
pub mod style {
    use super::{Tone, metrics, palette};
    use iced::widget::overlay::menu as menu_widget;
    use iced::widget::pick_list as pick_list_widget;
    use iced::widget::radio as radio_widget;
    use iced::widget::scrollable as scrollable_widget;
    use iced::widget::text_input as text_input_widget;
    use iced::widget::{button, container, progress_bar};
    use iced::{Background, Border, Color, Shadow, Theme, border};

    fn fill(color: Color) -> Option<Background> {
        Some(Background::Color(color))
    }

    fn outline(color: Color, radius: f32) -> Border {
        border::rounded(radius).color(color).width(1.0)
    }

    /// Primary action: a crayon fill carrying paper-white ink.
    pub fn primary_button(_theme: &Theme, status: button::Status) -> button::Style {
        let (background, text_color) = match status {
            button::Status::Active => (palette::ACCENT, palette::ON_ACCENT),
            button::Status::Hovered => (palette::ACCENT_HOVER, palette::ON_ACCENT),
            button::Status::Pressed => (palette::ACCENT_PRESSED, palette::ON_ACCENT),
            button::Status::Disabled => (palette::alpha(palette::TEXT, 0.10), palette::MUTED),
        };

        button::Style {
            background: fill(background),
            text_color,
            border: border::rounded(metrics::RADIUS_CONTROL),
            ..button::Style::default()
        }
    }

    /// Secondary action: a sheet with a hairline around ink text.
    pub fn secondary_button(_theme: &Theme, status: button::Status) -> button::Style {
        let (background, border_color, text_color) = match status {
            button::Status::Active => (palette::SURFACE, palette::BORDER_STRONG, palette::TEXT),
            button::Status::Hovered => (palette::WELL, palette::BORDER_STRONG, palette::TEXT),
            button::Status::Pressed => (
                palette::alpha(palette::TEXT, 0.08),
                palette::BORDER_STRONG,
                palette::TEXT,
            ),
            button::Status::Disabled => (palette::SURFACE, palette::BORDER, palette::MUTED),
        };

        button::Style {
            background: fill(background),
            text_color,
            border: outline(border_color, metrics::RADIUS_CONTROL),
            ..button::Style::default()
        }
    }

    /// A selected chip: the crayon as ink and frame, the sheet behind it. A chip
    /// marks a choice already made, so it must not look like the primary action.
    pub fn chip_active(_theme: &Theme, status: button::Status) -> button::Style {
        let (background, border_color) = match status {
            button::Status::Active => (palette::SURFACE, palette::ACCENT),
            button::Status::Hovered => (palette::WELL, palette::ACCENT),
            button::Status::Pressed => (palette::WELL, palette::ACCENT_PRESSED),
            button::Status::Disabled => (palette::SURFACE, palette::BORDER),
        };

        button::Style {
            background: fill(background),
            text_color: if matches!(status, button::Status::Disabled) {
                palette::MUTED
            } else {
                palette::ACCENT
            },
            border: outline(border_color, metrics::RADIUS_CONTROL),
            ..button::Style::default()
        }
    }

    /// A link-style button: ink at rest, the crayon on hover.
    pub fn link(_theme: &Theme, status: button::Status) -> button::Style {
        let text_color = match status {
            button::Status::Active => palette::MUTED,
            button::Status::Hovered => palette::ACCENT,
            button::Status::Pressed => palette::ACCENT_PRESSED,
            button::Status::Disabled => palette::MUTED,
        };

        button::Style {
            background: None,
            text_color,
            border: outline(Color::TRANSPARENT, metrics::RADIUS_CONTROL),
            ..button::Style::default()
        }
    }

    /// A sheet laid on the paper: the surface grouped content sits on.
    pub fn plate(_theme: &Theme) -> container::Style {
        container::Style {
            background: fill(palette::SURFACE),
            border: outline(palette::BORDER, metrics::RADIUS_SURFACE),
            text_color: Some(palette::TEXT),
            ..container::Style::default()
        }
    }

    /// The grouped-row surface ([`plate`], under the name the views use).
    pub fn card(theme: &Theme) -> container::Style {
        plate(theme)
    }

    /// The live sheet: the same paper behind a heavier crayon frame. Selection is
    /// a frame, not a wash — a tint behind nested content muddies both.
    pub fn card_selected(theme: &Theme) -> container::Style {
        container::Style {
            border: border::rounded(metrics::RADIUS_SURFACE)
                .color(palette::ACCENT)
                .width(1.5),
            ..plate(theme)
        }
    }

    /// A row outline without a fill: one field of the grid.
    pub fn outline_row(_theme: &Theme) -> container::Style {
        container::Style {
            background: None,
            border: outline(palette::BORDER, metrics::RADIUS_SURFACE),
            text_color: Some(palette::TEXT),
            ..container::Style::default()
        }
    }

    /// A loading placeholder shaped like the row it stands in for.
    pub fn skeleton(_theme: &Theme) -> container::Style {
        container::Style {
            background: fill(palette::WELL),
            border: outline(palette::BORDER, metrics::RADIUS_SURFACE),
            ..container::Style::default()
        }
    }

    /// A placard: the tone's wash, its ink and a soft frame in the same tone —
    /// the one alarm shape. Failures frame in the stop red.
    pub fn placard(tone: Tone) -> impl Fn(&Theme) -> container::Style + 'static {
        let ink = tone.color();
        let frame = match tone {
            Tone::Error => palette::ERROR_FILL,
            _ => ink,
        };

        move |_theme| container::Style {
            background: fill(palette::alpha(ink, 0.08)),
            border: outline(palette::alpha(frame, 0.32), metrics::RADIUS_CONTROL),
            text_color: Some(ink),
            ..container::Style::default()
        }
    }

    /// Determinate progress (model download). The track is the hairline tone so
    /// the filled/unfilled boundary stays visible; the percentage carries the value.
    pub fn progress(_theme: &Theme) -> progress_bar::Style {
        progress_bar::Style {
            background: Background::Color(palette::BORDER),
            bar: Background::Color(palette::ACCENT),
            border: border::rounded(4.0),
        }
    }

    /// The model-card indicator bars: the same crayon, quieter and thin.
    /// Deliberately not the download progress look — these are reference values,
    /// not work in flight.
    pub fn progress_subtle(_theme: &Theme) -> progress_bar::Style {
        progress_bar::Style {
            background: Background::Color(palette::BORDER),
            bar: Background::Color(palette::alpha(palette::ACCENT, 0.55)),
            border: border::rounded(4.0),
        }
    }

    /// A radio option; the dot carries the crayon, the ring carries the hover.
    /// The ring is a text tone so the unselected control stays above the 3:1
    /// non-text contrast minimum.
    pub fn radio(_theme: &Theme, status: radio_widget::Status) -> radio_widget::Style {
        let border_color = match status {
            radio_widget::Status::Active { .. } => palette::BORDER_STRONG,
            radio_widget::Status::Hovered { .. } => palette::ACCENT,
        };

        radio_widget::Style {
            background: Background::Color(Color::TRANSPARENT),
            dot_color: palette::ACCENT,
            border_width: 1.5,
            border_color,
            text_color: Some(palette::TEXT),
        }
    }

    /// A field: the recessed well, crayon frame while focused.
    pub fn text_input(
        _theme: &Theme,
        status: text_input_widget::Status,
    ) -> text_input_widget::Style {
        let border_color = match status {
            text_input_widget::Status::Focused { .. } => palette::ACCENT,
            text_input_widget::Status::Hovered => palette::BORDER_STRONG,
            text_input_widget::Status::Active | text_input_widget::Status::Disabled => {
                palette::BORDER
            }
        };

        text_input_widget::Style {
            background: Background::Color(palette::WELL),
            border: outline(border_color, metrics::RADIUS_CONTROL),
            icon: palette::MUTED,
            placeholder: palette::MUTED,
            value: palette::TEXT,
            selection: palette::alpha(palette::ACCENT, 0.28),
        }
    }

    /// The device picker: the field's well, its border carrying hover and open.
    pub fn pick_list(_theme: &Theme, status: pick_list_widget::Status) -> pick_list_widget::Style {
        // Hover is a heavier hairline; only the open picker takes the crayon, so
        // hover and focus never look the same.
        let border_color = match status {
            pick_list_widget::Status::Opened { .. } => palette::ACCENT,
            pick_list_widget::Status::Hovered => palette::BORDER_STRONG,
            pick_list_widget::Status::Active => palette::BORDER,
        };

        pick_list_widget::Style {
            text_color: palette::TEXT,
            placeholder_color: palette::MUTED,
            handle_color: palette::MUTED,
            background: Background::Color(palette::WELL),
            border: outline(border_color, metrics::RADIUS_CONTROL),
        }
    }

    /// The picker's dropdown: a sheet lifted off the paper by a soft shadow.
    pub fn pick_list_menu(_theme: &Theme) -> menu_widget::Style {
        menu_widget::Style {
            background: Background::Color(palette::SURFACE),
            border: outline(palette::BORDER_STRONG, metrics::RADIUS_CONTROL),
            text_color: palette::TEXT,
            selected_text_color: palette::ON_ACCENT,
            selected_background: Background::Color(palette::ACCENT),
            shadow: Shadow {
                color: palette::alpha(palette::TEXT, 0.18),
                offset: iced::Vector::new(0.0, 6.0),
                blur_radius: 18.0,
            },
        }
    }

    /// The scrollbars of the scrolling regions: a soft rail-less thumb.
    pub fn scrollable(
        _theme: &Theme,
        status: scrollable_widget::Status,
    ) -> scrollable_widget::Style {
        let scroller = match status {
            scrollable_widget::Status::Active { .. } => palette::alpha(palette::MUTED, 0.45),
            scrollable_widget::Status::Hovered { .. } => palette::alpha(palette::MUTED, 0.70),
            scrollable_widget::Status::Dragged { .. } => palette::ACCENT,
        };
        let rail = scrollable_widget::Rail {
            background: None,
            border: border::rounded(metrics::RADIUS_PILL),
            scroller: scrollable_widget::Scroller {
                background: Background::Color(scroller),
                border: border::rounded(metrics::RADIUS_PILL),
            },
        };

        scrollable_widget::Style {
            container: container::Style::default(),
            vertical_rail: rail,
            horizontal_rail: rail,
            gap: None,
            auto_scroll: scrollable_widget::AutoScroll {
                background: Background::Color(palette::SURFACE),
                border: outline(palette::BORDER_STRONG, metrics::RADIUS_CONTROL),
                shadow: Shadow {
                    color: palette::alpha(palette::TEXT, 0.18),
                    offset: iced::Vector::new(0.0, 4.0),
                    blur_radius: 14.0,
                },
                icon: palette::TEXT,
            },
        }
    }
}

/// Tone of a banner / status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warning,
    Error,
    Success,
}

impl Tone {
    fn color(self) -> iced::Color {
        match self {
            Tone::Info => palette::ACCENT,
            Tone::Warning => palette::WARNING,
            Tone::Error => palette::ERROR,
            Tone::Success => palette::SUCCESS,
        }
    }
}

// ---------------------------------------------------------------- window glue

/// Window size and appearance per surface.
pub fn window_settings(surface: &Surface) -> window::Settings {
    let (width, height, min_width, min_height) = match surface {
        Surface::Onboarding => (820.0, 660.0, 620.0, 480.0),
        Surface::Settings => (900.0, 720.0, 620.0, 480.0),
        Surface::Problems => (560.0, 620.0, 420.0, 320.0),
    };

    window::Settings {
        size: iced::Size::new(width, height),
        position: window::Position::Centered,
        min_size: Some(iced::Size::new(min_width, min_height)),
        icon: window_icon(),
        // Match the shipped desktop entry so the compositor ties the running
        // window to it (FR-008): the same single lowercase token as the entry
        // basename, `Icon=` and `StartupWMClass=`.
        platform_specific: window::settings::PlatformSpecific {
            application_id: crate::config::APP_ID.to_owned(),
            ..Default::default()
        },
        ..window::Settings::default()
    }
}

fn window_icon() -> Option<window::Icon> {
    let (rgba, width, height) = icon_rgba()?;
    window::icon::from_rgba(rgba.clone(), *width, *height).ok()
}

/// View dispatch for the daemon.
pub fn view(app: &App, id: window::Id) -> Element<'_, Message> {
    match app.windows.get(&id) {
        Some(Surface::Onboarding) => crate::ui_onboarding::view(app).map(Message::Onboarding),
        Some(Surface::Settings) => crate::ui_settings::view(app).map(Message::Settings),
        Some(Surface::Problems) => crate::ui_problems::view(app).map(Message::Problems),
        None => iced::widget::space().into(),
    }
}

/// Window titles (the OS-level title bar, not a view heading).
pub fn window_title(app: &App, id: window::Id) -> String {
    match app.windows.get(&id) {
        Some(Surface::Onboarding) => "Littera — Setup".to_owned(),
        Some(Surface::Settings) => "Littera — Settings".to_owned(),
        Some(Surface::Problems) => "Littera — Problems".to_owned(),
        None => "Littera".to_owned(),
    }
}

/// The scrollbar every surface uses for its content region.
///
/// `spacing` embeds the scrollbar instead of letting it float over the content:
/// iced then reserves `width + spacing` on the right whenever the scrollbar is
/// visible, so cards and banners can never run under it.
pub fn scroll_direction() -> scrollable::Direction {
    scrollable::Direction::Vertical(
        scrollable::Scrollbar::new()
            .width(metrics::SCROLLBAR_WIDTH)
            .scroller_width(metrics::SCROLLBAR_SCROLLER)
            .spacing(metrics::SPACE_SM),
    )
}

/// One theme for every window: the [`palette`] above, dark only (the overlay
/// indicator and the tray icon are dark, so the app has exactly one mode).
pub fn theme(_app: &App, _id: window::Id) -> Theme {
    app_theme()
}

/// The single app theme, also used when a surface is rendered on its own.
pub fn app_theme() -> Theme {
    static THEME: LazyLock<Theme> = LazyLock::new(|| {
        Theme::custom(
            "Littera",
            iced::theme::Palette {
                background: palette::BACKGROUND,
                text: palette::TEXT,
                primary: palette::ACCENT,
                success: palette::SUCCESS,
                warning: palette::WARNING,
                danger: palette::ERROR,
            },
        )
    });

    THEME.clone()
}

// -------------------------------------------------------------- chord capture

/// What the capture produced and which surface asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    Onboarding(String),
    Settings(String),
}

/// In-window chord capture for compositors without a shortcut portal
/// (Scenario 1.4 / FR-009).
#[derive(Debug, Default, Clone)]
pub struct ChordCapture {
    active: bool,
    mods: Vec<Modifier>,
    candidate: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapturedToken {
    Modifier(Modifier),
    Key(KeyToken),
}

impl ChordCapture {
    pub fn is_capturing(&self) -> bool {
        self.active
    }

    /// Start (or restart) capturing; the keyboard subscription is enabled while
    /// a capture is active.
    pub fn start(&mut self) {
        *self = Self {
            active: true,
            ..Self::default()
        };
    }

    /// Stop capturing and forget the transient state.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Live preview of a modifier-only chord being held.
    pub fn preview(&self) -> Option<&str> {
        self.candidate.as_deref()
    }

    /// Why the last attempt was rejected.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn on_press(&mut self, physical: &key::Physical) -> Option<String> {
        match decode(physical) {
            Some(CapturedToken::Modifier(modifier)) => {
                if !self.mods.contains(&modifier) {
                    self.mods.push(modifier);
                }
                if self.mods.len() >= 2 {
                    self.refresh_candidate();
                }
                None
            }
            Some(CapturedToken::Key(key)) => self.commit(Some(key)),
            None => None,
        }
    }

    fn on_release(&mut self, physical: &key::Physical) -> Option<String> {
        match decode(physical) {
            Some(CapturedToken::Modifier(modifier)) => {
                self.mods.retain(|held| *held != modifier);
                if self.mods.len() <= 1
                    && let Some(candidate) = self.candidate.take()
                {
                    return self.finish(candidate);
                }
                None
            }
            _ => None,
        }
    }

    fn refresh_candidate(&mut self) {
        let raw = chord_string(&self.mods, None);
        match hotkey::normalize_chord(&raw) {
            Ok(chord) => {
                self.candidate = Some(chord);
                self.error = None;
            }
            Err(error) => {
                self.candidate = None;
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    fn commit(&mut self, key: Option<KeyToken>) -> Option<String> {
        let raw = chord_string(&self.mods, key);
        self.finish(raw)
    }

    fn finish(&mut self, raw: String) -> Option<String> {
        match hotkey::normalize_chord(&raw) {
            Ok(chord) => {
                self.reset();
                Some(chord)
            }
            Err(error) => {
                self.candidate = None;
                self.error = Some(format!("{error:#}"));
                None
            }
        }
    }
}

/// Feed a keyboard event to whichever surface is capturing.
pub fn capture_chord(
    event: &KeyEvent,
    onboarding: &mut ChordCapture,
    settings: &mut ChordCapture,
) -> Option<CaptureTarget> {
    let (capture, target): (&mut ChordCapture, fn(String) -> CaptureTarget) =
        if onboarding.is_capturing() {
            (onboarding, CaptureTarget::Onboarding)
        } else if settings.is_capturing() {
            (settings, CaptureTarget::Settings)
        } else {
            return None;
        };

    match event {
        KeyEvent::KeyPressed { physical_key, .. } => capture.on_press(physical_key).map(target),
        KeyEvent::KeyReleased { physical_key, .. } => capture.on_release(physical_key).map(target),
        _ => None,
    }
}

fn decode(physical: &key::Physical) -> Option<CapturedToken> {
    use key::Code as C;

    if let key::Physical::Code(code) = physical {
        let modifier = match code {
            C::ControlLeft | C::ControlRight => Some(Modifier::Ctrl),
            C::SuperLeft | C::SuperRight => Some(Modifier::Super),
            C::AltLeft | C::AltRight => Some(Modifier::Alt),
            C::ShiftLeft | C::ShiftRight => Some(Modifier::Shift),
            _ => None,
        };
        if let Some(modifier) = modifier {
            return Some(CapturedToken::Modifier(modifier));
        }

        let key = match code {
            C::KeyA => KeyToken::Letter('A'),
            C::KeyB => KeyToken::Letter('B'),
            C::KeyC => KeyToken::Letter('C'),
            C::KeyD => KeyToken::Letter('D'),
            C::KeyE => KeyToken::Letter('E'),
            C::KeyF => KeyToken::Letter('F'),
            C::KeyG => KeyToken::Letter('G'),
            C::KeyH => KeyToken::Letter('H'),
            C::KeyI => KeyToken::Letter('I'),
            C::KeyJ => KeyToken::Letter('J'),
            C::KeyK => KeyToken::Letter('K'),
            C::KeyL => KeyToken::Letter('L'),
            C::KeyM => KeyToken::Letter('M'),
            C::KeyN => KeyToken::Letter('N'),
            C::KeyO => KeyToken::Letter('O'),
            C::KeyP => KeyToken::Letter('P'),
            C::KeyQ => KeyToken::Letter('Q'),
            C::KeyR => KeyToken::Letter('R'),
            C::KeyS => KeyToken::Letter('S'),
            C::KeyT => KeyToken::Letter('T'),
            C::KeyU => KeyToken::Letter('U'),
            C::KeyV => KeyToken::Letter('V'),
            C::KeyW => KeyToken::Letter('W'),
            C::KeyX => KeyToken::Letter('X'),
            C::KeyY => KeyToken::Letter('Y'),
            C::KeyZ => KeyToken::Letter('Z'),
            C::Digit1 => KeyToken::Digit(1),
            C::Digit2 => KeyToken::Digit(2),
            C::Digit3 => KeyToken::Digit(3),
            C::Digit4 => KeyToken::Digit(4),
            C::Digit5 => KeyToken::Digit(5),
            C::Digit6 => KeyToken::Digit(6),
            C::Digit7 => KeyToken::Digit(7),
            C::Digit8 => KeyToken::Digit(8),
            C::Digit9 => KeyToken::Digit(9),
            C::Digit0 => KeyToken::Digit(0),
            C::F1 => KeyToken::F(1),
            C::F2 => KeyToken::F(2),
            C::F3 => KeyToken::F(3),
            C::F4 => KeyToken::F(4),
            C::F5 => KeyToken::F(5),
            C::F6 => KeyToken::F(6),
            C::F7 => KeyToken::F(7),
            C::F8 => KeyToken::F(8),
            C::F9 => KeyToken::F(9),
            C::F10 => KeyToken::F(10),
            C::F11 => KeyToken::F(11),
            C::F12 => KeyToken::F(12),
            C::Space => KeyToken::Space,
            _ => return None,
        };
        return Some(CapturedToken::Key(key));
    }

    None
}

fn chord_string(mods: &[Modifier], key: Option<KeyToken>) -> String {
    let mut parts: Vec<String> = [
        Modifier::Ctrl,
        Modifier::Super,
        Modifier::Alt,
        Modifier::Shift,
    ]
    .iter()
    .filter(|modifier| mods.contains(modifier))
    .map(|modifier| match modifier {
        Modifier::Ctrl => "CTRL".to_owned(),
        Modifier::Super => "SUPER".to_owned(),
        Modifier::Alt => "ALT".to_owned(),
        Modifier::Shift => "SHIFT".to_owned(),
    })
    .collect();

    if let Some(key) = key {
        parts.push(match key {
            KeyToken::Letter(letter) => letter.to_string(),
            KeyToken::Digit(digit) => digit.to_string(),
            KeyToken::F(number) => format!("F{number}"),
            KeyToken::Space => "SPACE".to_owned(),
        });
    }

    parts.join("+")
}

// --------------------------------------------------------------- UI state

/// Onboarding steps (FR-001).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Model,
    Language,
    KeyBinding,
    SystemCheck,
}

impl Step {
    pub const ALL: [Step; 4] = [
        Step::Model,
        Step::Language,
        Step::KeyBinding,
        Step::SystemCheck,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Step::Model => "Model",
            Step::Language => "Language",
            Step::KeyBinding => "Key binding",
            Step::SystemCheck => "System check",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|step| *step == self).unwrap_or(0)
    }

    pub fn next(self) -> Option<Step> {
        Self::ALL.get(self.index() + 1).copied()
    }

    pub fn previous(self) -> Option<Step> {
        self.index()
            .checked_sub(1)
            .and_then(|index| Self::ALL.get(index))
            .copied()
    }
}

/// Everything the onboarding surface shows (FR-001/FR-002/Scenario 1).
#[derive(Debug, Clone)]
pub struct OnboardingUi {
    pub step: Step,
    pub model: String,
    pub language: Language,
    pub hotkey: String,
    pub capture: ChordCapture,
    /// The selected model file exists and passed verification.
    pub model_ready: bool,
}

impl OnboardingUi {
    pub fn from_config(cfg: &Config) -> Self {
        // An unknown (or from an older release) catalog id falls back to the
        // default selection without rewriting the config.
        let selection =
            model_catalog::artifact(&cfg.model).unwrap_or_else(model_catalog::default_selection);
        Self {
            step: Step::Model,
            model: selection.artifact.id.to_owned(),
            language: cfg.language,
            hotkey: cfg.hotkey.clone(),
            capture: ChordCapture::default(),
            model_ready: models::path_for(selection.artifact)
                .map(|path| path.exists())
                .unwrap_or(false),
        }
    }

    /// The curated artifact this step has selected.
    pub fn selection(&self) -> Selection {
        model_catalog::artifact(&self.model).unwrap_or_else(model_catalog::default_selection)
    }

    /// FR-002/FR-011: a model that does not serve the chosen language warns. The
    /// warning is informational — it never blocks a step (SC-007).
    pub fn language_warning(&self) -> Option<String> {
        language_warning(self.selection(), self.language)
    }
}

/// The warning shown when the selected model does not serve the configured
/// dictation language; `None` means the combination is fine (FR-011). The text
/// names the configured language, so it reads the same for every entry of the
/// language table.
pub fn language_warning(selection: Selection, language: Language) -> Option<String> {
    model_catalog::language_warning(selection.variant, language)
}

/// The settings section a rejected change belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Model,
    Language,
    /// The capture device used for dictation (FR-004/FR-006).
    Microphone,
    Hotkey,
    Storage,
}

/// The last settings change that could not be applied: a language/model
/// conflict or a failed config write (FR-002/FR-016).
#[derive(Debug, Clone)]
pub struct SettingsStatus {
    pub text: String,
    pub tone: Tone,
    pub section: SettingsSection,
}

/// Settings state. Every edit applies immediately (autosave, Scenario 3.4), so
/// the surface holds nothing but transient state: the chord capture and the
/// last change that could not be applied.
#[derive(Debug, Clone, Default)]
pub struct SettingsUi {
    pub capture: ChordCapture,
    pub status: Option<SettingsStatus>,
    /// Filters, sorting and the expanded detail of the model list (FR-005);
    /// session-only, never persisted.
    pub list: crate::ui_settings::ListState,
    /// The Microphone section's device list: `None` until the enumeration started when the
    /// window opened has answered (FR-008/FR-009).
    pub input_devices: Option<Vec<audio::InputDevice>>,
}

impl SettingsUi {
    /// The status banner to render in `section`, when the last change that
    /// could not be applied was attempted there.
    pub fn status_for(&self, section: SettingsSection) -> Option<&SettingsStatus> {
        self.status
            .as_ref()
            .filter(|status| status.section == section)
    }
}

// ------------------------------------------------------------- small widgets

/// The reading face at regular weight: prose, values and labels.
fn value() -> Font {
    Font {
        family: Family::Name("Atkinson Hyperlegible"),
        ..Font::DEFAULT
    }
}

/// The same face at bold: titles, headings, names and actions. Hierarchy is
/// weight and space, never a fourth type size.
fn legend() -> Font {
    Font {
        family: Family::Name("Atkinson Hyperlegible"),
        weight: Weight::Bold,
        ..Font::DEFAULT
    }
}

/// A surface title: the largest step, sentence case.
pub fn title<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_TITLE)
        .font(legend())
        .color(palette::TEXT)
        .line_height(metrics::LINE_HEIGHT_TIGHT)
        .into()
}

/// A section heading: bold at the body step, sentence case.
pub fn heading<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_HEADING)
        .font(legend())
        .color(palette::TEXT)
        .line_height(metrics::LINE_HEIGHT_TIGHT)
        .into()
}

/// A field key: the quiet label that names the value beside it.
pub fn key<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_CAPTION)
        .font(value())
        .color(palette::MUTED)
        .into()
}

/// An emphasised inline value: the reading face, bold.
pub fn strong<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_BODY)
        .font(legend())
        .color(palette::TEXT)
        .into()
}

/// A muted label, sized to its content (inline use).
pub fn muted<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_CAPTION)
        .font(value())
        .color(palette::MUTED)
        .into()
}

/// A muted explanation that wraps instead of overflowing.
pub fn lead<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_BODY)
        .font(value())
        .color(palette::MUTED)
        .width(Length::Fill)
        .line_height(metrics::LINE_HEIGHT)
        .wrapping(Wrapping::WordOrGlyph)
        .into()
}

/// A data value in the monospace face: sizes, counts, percentages.
pub fn data<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_CAPTION)
        .font(Font::MONOSPACE)
        .color(palette::TEXT)
        .into()
}

/// Monospace text for commands and paths; wraps into the available width.
pub fn mono<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_CAPTION)
        .font(Font::MONOSPACE)
        .color(palette::TEXT)
        .width(Length::Fill)
        .wrapping(Wrapping::WordOrGlyph)
        .into()
}

/// The configured key chord: ordinary words, not an identifier — "Ctrl + Super"
/// reads faster than the stored "CTRL+SUPER".
pub fn chord<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(chord_words(&content.into()))
        .size(metrics::TEXT_LEAD)
        .font(legend())
        .color(palette::TEXT)
        .into()
}

/// Spell a stored chord the way a person says it: `CTRL+SUPER` → `Ctrl + Super`.
/// Unknown tokens keep their spelling with the first letter capitalised, so a
/// key the table does not name still reads as a word.
pub fn chord_words(raw: &str) -> String {
    raw.split('+')
        .filter(|token| !token.trim().is_empty())
        .map(|token| {
            let upper = token.trim().to_ascii_uppercase();
            match upper.as_str() {
                "CTRL" | "CONTROL" => "Ctrl".to_owned(),
                "SUPER" | "META" | "WIN" => "Super".to_owned(),
                "ALT" => "Alt".to_owned(),
                "SHIFT" => "Shift".to_owned(),
                _ => {
                    let mut chars = upper.chars();
                    match chars.next() {
                        Some(first) => first.to_string() + &chars.as_str().to_ascii_lowercase(),
                        None => String::new(),
                    }
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// A one-line state caption in its tone's colour. The state always says its
/// word, so colour is a second signal and never the only one.
pub fn state_tag<M: 'static>(content: impl Into<String>, tone: Tone) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_CAPTION)
        .font(legend())
        .color(tone.color())
        .into()
}

/// A control label: sentence case in the bold face.
pub fn stamp<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_CONTROL)
        .font(legend())
        .into()
}

/// A region inside a sheet: a hairline boundary with no fill of its own, so a
/// grouped block never becomes a card inside a card.
pub fn inset<M: 'static>(content: impl Into<Element<'static, M>>) -> Element<'static, M> {
    let content = container(content).padding(metrics::SPACE_MD);
    container(content)
        .width(Length::Fill)
        .style(style::outline_row)
        .into()
}

/// A sentence in ink at the body step: values that read as prose, not as data.
pub fn body<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_BODY)
        .font(value())
        .color(palette::TEXT)
        .width(Length::Fill)
        .line_height(metrics::LINE_HEIGHT)
        .wrapping(Wrapping::WordOrGlyph)
        .into()
}

/// A one-sentence statement of the machine's state: the first thing a surface
/// says. Bold, at the lead step, wrapping into the available width.
pub fn headline<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_LEAD)
        .font(legend())
        .color(palette::TEXT)
        .width(Length::Fill)
        .line_height(metrics::LINE_HEIGHT)
        .wrapping(Wrapping::WordOrGlyph)
        .into()
}

/// A sheet laid on the paper: one frame, one hairline, generous padding.
pub fn plate<M: 'static>(content: impl Into<Element<'static, M>>) -> Element<'static, M> {
    container(content)
        .padding(metrics::SPACE_XL)
        .width(Length::Fill)
        .style(style::plate)
        .into()
}

/// A card: one sheet, one hairline, room for grouped content.
pub fn card<M: 'static>(content: impl Into<Element<'static, M>>) -> Element<'static, M> {
    surface(content, style::card)
}

/// The selected card: same surface, accent border.
pub fn card_active<M: 'static>(content: impl Into<Element<'static, M>>) -> Element<'static, M> {
    surface(content, style::card_selected)
}

/// A field row: the same plate as a card, tightened to the list's row rhythm
/// (spec `model-list-field-rows` FR-001) — one row carries the facts of a list
/// entry in two lines instead of a card's full column.
pub fn field_row<M: 'static>(content: impl Into<Element<'static, M>>) -> Element<'static, M> {
    row_surface(content, style::card)
}

/// The active field row: same surface behind the signal frame.
pub fn field_row_active<M: 'static>(
    content: impl Into<Element<'static, M>>,
) -> Element<'static, M> {
    row_surface(content, style::card_selected)
}

fn row_surface<M: 'static>(
    content: impl Into<Element<'static, M>>,
    style: fn(&Theme) -> container::Style,
) -> Element<'static, M> {
    container(content)
        .padding([metrics::SPACE_MD, metrics::SPACE_LG])
        .width(Length::Fill)
        .style(style)
        .into()
}

fn surface<M: 'static>(
    content: impl Into<Element<'static, M>>,
    style: fn(&Theme) -> container::Style,
) -> Element<'static, M> {
    container(content)
        .padding(metrics::SPACE_LG)
        .width(Length::Fill)
        .style(style)
        .into()
}

/// A placard: the tone's wash, frame and ink — the one alarm shape.
pub fn banner<M: 'static>(content: impl Into<String>, tone: Tone) -> Element<'static, M> {
    container(
        text(content.into())
            .size(metrics::TEXT_BODY)
            .font(value())
            .color(tone.color())
            .width(Length::Fill)
            .line_height(metrics::LINE_HEIGHT)
            .wrapping(Wrapping::WordOrGlyph),
    )
    .padding([metrics::SPACE_MD, metrics::SPACE_LG])
    .width(Length::Fill)
    .style(style::placard(tone))
    .into()
}

/// Loading placeholder rows shaped like [`check_row`], for a surface whose
/// capability checks have not arrived yet.
pub fn check_skeleton<M: 'static>(rows: usize) -> Element<'static, M> {
    let mut column = iced::widget::column![].spacing(metrics::SPACE_SM);

    for _ in 0..rows {
        column = column.push(
            container(iced::widget::Space::new().height(Length::Fixed(56.0)))
                .width(Length::Fill)
                .style(style::skeleton),
        );
    }

    column.into()
}

/// Position of one step in a progress stepper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipState {
    /// The step being edited.
    Current,
    /// A step already behind the current one.
    Done,
    /// A step still ahead.
    Upcoming,
}

/// One numbered step chip.
pub fn step_chip<M: 'static>(
    number: usize,
    label: &'static str,
    state: ChipState,
) -> Element<'static, M> {
    let (background, border_color, text_color) = match state {
        ChipState::Current => (palette::ACCENT, palette::ACCENT, palette::ON_ACCENT),
        ChipState::Done => (palette::SURFACE, palette::BORDER_STRONG, palette::TEXT),
        ChipState::Upcoming => (palette::BACKGROUND, palette::BORDER, palette::MUTED),
    };

    container(
        text(format!("{number}  {label}"))
            .size(metrics::TEXT_CAPTION)
            .font(legend())
            .color(text_color),
    )
    .padding(Padding {
        top: metrics::SPACE_SM,
        right: metrics::SPACE_MD,
        bottom: metrics::SPACE_SM,
        left: metrics::SPACE_MD,
    })
    .style(move |_theme: &Theme| container::Style {
        background: Some(Background::Color(background)),
        border: border::rounded(metrics::RADIUS_PILL)
            .color(border_color)
            .width(1.0),
        text_color: Some(text_color),
        ..container::Style::default()
    })
    .into()
}

/// What a model card is doing right now (FR-002/FR-003/FR-006/SC-008).
///
/// One decision, two renderings: the onboarding renders it as the expanded
/// [`model_panel`], the settings list as one compact line next to the action.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelState {
    /// A transfer for this artifact is running.
    Downloading { percent: f32 },
    /// The file is on disk and its SHA-256 is being checked; a transfer only
    /// starts when the check fails.
    Verifying,
    /// The last run failed (the partial file is kept).
    Failed(String),
    /// The file is on disk and the engine loaded it.
    Verified,
    /// The file is on disk, but it is not the running model.
    OnDisk,
    /// No file on disk yet: using the model needs a download.
    NotDownloaded,
}

/// The state of one artifact; `verified` is the caller's "the engine reported
/// this model loaded" flag, which only the surface knows (FR-002/FR-003).
pub fn model_state(app: &App, selection: Selection, verified: bool) -> ModelState {
    if let Some(state) = app
        .download
        .as_ref()
        .filter(|state| state.selection.artifact.id == selection.artifact.id)
    {
        if let Some(error) = &state.error {
            return ModelState::Failed(error.clone());
        }
        if state.verifying {
            return ModelState::Verifying;
        }

        return ModelState::Downloading {
            percent: state.percent(),
        };
    }

    if verified {
        return ModelState::Verified;
    }

    let on_disk = models::path_for(selection.artifact)
        .ok()
        .is_some_and(|path| path.exists());

    if on_disk {
        ModelState::OnDisk
    } else {
        ModelState::NotDownloaded
    }
}

/// Whether another artifact's transfer occupies the pipeline (FR-006/SC-008):
/// exactly one download runs at a time, so this card's action is unavailable.
pub fn download_busy_elsewhere(app: &App, artifact: &str) -> bool {
    app.download
        .as_ref()
        .is_some_and(|state| state.is_running() && state.selection.artifact.id != artifact)
}

/// Model status and progress, shared by onboarding and settings (FR-002/FR-003).
///
/// `download`, `cancel` and `retry` are the surface-specific messages for the
/// three actions.
pub fn model_panel<M: Clone + 'static>(
    app: &App,
    selection: Selection,
    verified: bool,
    download: M,
    cancel: M,
    retry: M,
) -> Element<'static, M> {
    let size_label = models::human_size(selection.artifact.size_bytes);
    let busy_elsewhere = download_busy_elsewhere(app, selection.artifact.id);

    match model_state(app, selection, verified) {
        ModelState::Verifying => {
            // The artifact is on disk: this run checks the SHA-256 and only
            // starts a transfer when the file fails that check (FR-003).
            inset(
                iced::widget::column![
                    strong(format!("Verifying the model — {size_label} (SHA-256)")),
                    iced::widget::row![
                        muted("No download — a transfer only starts if the check fails."),
                        button(stamp("Cancel"))
                            .style(style::secondary_button)
                            .on_press(cancel),
                    ]
                    .spacing(metrics::SPACE_MD)
                    .align_y(iced::Alignment::Center),
                ]
                .spacing(metrics::SPACE_MD),
            )
        }
        ModelState::Downloading { percent } => {
            let row = iced::widget::row![
                progress_bar(0.0..=100.0, percent)
                    .length(Length::Fill)
                    .style(style::progress),
                text(format!("{percent:.0}%"))
                    .size(metrics::TEXT_CAPTION)
                    .font(Font::MONOSPACE)
                    .color(palette::MUTED),
                button(stamp("Cancel"))
                    .style(style::secondary_button)
                    .on_press(cancel),
            ]
            .spacing(metrics::SPACE_MD)
            .align_y(iced::Alignment::Center);

            inset(
                iced::widget::column![
                    strong(format!("Downloading the model — {size_label}")),
                    row,
                ]
                .spacing(metrics::SPACE_MD),
            )
        }
        ModelState::Failed(error) => inset(
            iced::widget::column![
                banner(error, Tone::Error),
                iced::widget::row![
                    action_button("Retry", retry, busy_elsewhere, style::secondary_button),
                    muted("The partial download is kept and resumed."),
                ]
                .spacing(metrics::SPACE_MD)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(metrics::SPACE_MD),
        ),
        state @ (ModelState::Verified | ModelState::OnDisk | ModelState::NotDownloaded) => {
            let (status, action) = match state {
                ModelState::Verified => {
                    (banner("Verified — ready to use", Tone::Success), "Verify")
                }
                ModelState::OnDisk => (
                    banner(
                        "Already on disk — verified against its SHA-256 before use",
                        Tone::Info,
                    ),
                    "Verify",
                ),
                _ => (
                    banner(
                        format!("Not downloaded yet — {size_label} to download"),
                        Tone::Warning,
                    ),
                    "Download",
                ),
            };

            inset(
                iced::widget::column![
                    status,
                    iced::widget::row![action_button(
                        action,
                        download,
                        busy_elsewhere,
                        style::primary_button
                    )]
                    .spacing(metrics::SPACE_SM),
                ]
                .spacing(metrics::SPACE_MD),
            )
        }
    }
}

/// The action of a [`model_panel`], rendered without an `on_press` while another
/// download occupies the pipeline (FR-006/SC-008).
fn action_button<M: Clone + 'static>(
    label: &'static str,
    message: M,
    disabled: bool,
    style: fn(&Theme, button::Status) -> button::Style,
) -> Element<'static, M> {
    let button = button(stamp(label)).style(style);
    if disabled {
        button.into()
    } else {
        button.on_press(message).into()
    }
}

/// Caption of the state line while the model is not resident (FR-001).
const STATUS_LABEL: &str = "Status";
/// Caption of the resident line: it is the backend report (FR-005).
const BACKEND_LABEL: &str = "Backend";
/// Prefix of the fallback notice; the reason follows after an em dash (FR-005).
const BACKEND_FALLBACK: &str = "CPU fallback";

/// The model-state value line and its optional notice (FR-001/FR-002/FR-012).
#[derive(Debug, PartialEq, Eq)]
pub struct ModelStateLine {
    pub caption: &'static str,
    pub text: String,
    pub note: Option<(String, Tone)>,
}

/// The one line that says what the selected model is doing (FR-001).
///
/// Indeterminate by construction: neither the loading nor any other state has a
/// percentage (FR-003).
pub fn model_state_line(status: &ModelStatus, label: &str) -> ModelStateLine {
    match status {
        ModelStatus::Loading => ModelStateLine {
            caption: STATUS_LABEL,
            text: format!("Loading {label}…"),
            note: None,
        },
        ModelStatus::Resident(info) => ModelStateLine {
            caption: BACKEND_LABEL,
            text: format!("{} — {}", info.backend, info.device),
            note: info.fallback.as_deref().map(fallback_note),
        },
        ModelStatus::NotResident => ModelStateLine {
            caption: STATUS_LABEL,
            text: format!("{label} is not loaded — the next dictation loads it"),
            note: None,
        },
        ModelStatus::Failed(reason) => ModelStateLine {
            caption: STATUS_LABEL,
            text: format!("Load failed — {label}"),
            note: Some((reason.clone(), Tone::Error)),
        },
    }
}

/// FR-005: a fallback is never silent — this is the reason the GPU path was
/// abandoned, spelled the same way in the engine's notice.
fn fallback_note(reason: &str) -> (String, Tone) {
    (format!("{BACKEND_FALLBACK} — {reason}"), Tone::Warning)
}

/// The selected model's load lifecycle as one card (FR-001/FR-002/FR-012),
/// shared by the settings model section and the onboarding model card.
///
/// `None` when `selection` is not the model the app loads: a card for another
/// model must not claim the running model's state.
pub fn model_status_card<M: 'static>(
    app: &App,
    selection: Selection,
) -> Option<Element<'static, M>> {
    if app.model.artifact.id != selection.artifact.id {
        return None;
    }
    let line = model_state_line(&app.model_status, &selection.label());
    let mut body = iced::widget::column![
        iced::widget::column![key::<M>(line.caption), body::<M>(line.text)]
            .spacing(metrics::SPACE_XS),
    ]
    .spacing(metrics::SPACE_MD);
    if let Some((note, tone)) = line.note {
        body = body.push(banner(note, tone));
    }
    Some(inset(body))
}

/// One system-check row: status, detail and remediation (FR-014).
pub fn check_row<M: 'static>(check: &Check) -> Element<'static, M> {
    let (tone, marker) = if check.ok {
        (Tone::Success, "Works")
    } else {
        (Tone::Error, "Needs a fix")
    };

    let mut body = iced::widget::column![
        iced::widget::row![state_tag::<M>(marker, tone), strong(check.name.to_owned()),]
            .spacing(metrics::SPACE_SM)
            .align_y(iced::Alignment::Center),
        lead(check.detail.clone()),
    ]
    .spacing(metrics::SPACE_XS);

    // The fix is prose, not code: two of the four checks name a setting rather
    // than a command (FR-014 allows either), so the line is set in the reading
    // face and labelled — mono would dress an instruction as a shell command.
    if let Some(remediation) = &check.remediation {
        body = body.push(
            iced::widget::row![
                container(key::<M>("Fix")).width(Length::Fixed(metrics::SPACE_XL)),
                text(remediation.clone())
                    .size(metrics::TEXT_CONTROL)
                    .font(value())
                    .color(palette::TEXT)
                    .width(Length::Fill)
                    .wrapping(Wrapping::WordOrGlyph),
            ]
            .spacing(metrics::SPACE_SM)
            .align_y(iced::Alignment::Start),
        );
    }

    container(body)
        .padding(metrics::SPACE_LG)
        .width(Length::Fill)
        .style(style::outline_row)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn physical(code: key::Code) -> key::Physical {
        key::Physical::Code(code)
    }

    /// FR-007/SC-005: a broken embedded icon only degrades to "no icon".
    #[test]
    fn undecodable_icon_bytes_yield_none() {
        assert!(decode_icon(b"not a png").is_none());
        assert!(decode_icon(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).is_none());
    }

    /// The config stores the canonical spelling; the window must not show it.
    #[test]
    fn chord_words_read_like_a_person_says_them() {
        assert_eq!(chord_words("CTRL+SUPER"), "Ctrl + Super");
        assert_eq!(chord_words("CTRL+ALT+SPACE"), "Ctrl + Alt + Space");
        assert_eq!(chord_words("SHIFT+F1"), "Shift + F1");
        assert_eq!(chord_words("A"), "A");
        assert_eq!(chord_words(""), "");
    }

    #[test]
    fn captures_modifier_and_key() {
        let mut capture = ChordCapture::default();
        capture.start();

        assert_eq!(capture.on_press(&physical(key::Code::ControlLeft)), None);
        assert_eq!(capture.on_press(&physical(key::Code::ShiftLeft)), None);
        assert_eq!(
            capture.on_press(&physical(key::Code::KeyA)),
            Some("CTRL+SHIFT+A".to_owned())
        );
        assert!(
            !capture.is_capturing(),
            "capture ends after a successful chord"
        );
    }

    #[test]
    fn captures_modifier_only_chord_on_release() {
        let mut capture = ChordCapture::default();
        capture.start();

        assert_eq!(capture.on_press(&physical(key::Code::SuperLeft)), None);
        assert_eq!(capture.on_press(&physical(key::Code::ControlLeft)), None);
        assert_eq!(capture.preview(), Some("CTRL+SUPER"));

        assert_eq!(
            capture.on_release(&physical(key::Code::SuperLeft)),
            Some("CTRL+SUPER".to_owned())
        );
    }

    #[test]
    fn captures_function_digit_and_space_keys() {
        for (code, expected) in [
            (key::Code::F5, "CTRL+ALT+F5"),
            (key::Code::Digit5, "CTRL+ALT+5"),
            (key::Code::Space, "CTRL+ALT+SPACE"),
        ] {
            let mut capture = ChordCapture::default();
            capture.start();
            capture.on_press(&physical(key::Code::ControlLeft));
            capture.on_press(&physical(key::Code::AltLeft));
            assert_eq!(capture.on_press(&physical(code)), Some(expected.to_owned()));
        }
    }

    #[test]
    fn ignores_unsupported_keys() {
        let mut capture = ChordCapture::default();
        capture.start();
        capture.on_press(&physical(key::Code::ControlLeft));
        assert_eq!(capture.on_press(&physical(key::Code::Escape)), None);
        assert!(
            capture.is_capturing(),
            "a rejected key must not end the capture"
        );
    }

    #[test]
    fn language_warning_names_the_mismatch_without_changing_the_selection() {
        let english = model_catalog::artifact("moonshine-streaming-tiny-Q8_0").expect("offered");
        let german =
            model_catalog::artifact("nemotron-3.5-asr-streaming-0.6b-Q8_0").expect("offered");

        assert_eq!(
            language_warning(english, Language::new("de-DE")),
            Some(
                "The selected model cannot transcribe German — choose a different model."
                    .to_owned()
            )
        );
        // Auto-detect fits every variant: the model picks the language itself.
        assert_eq!(language_warning(english, Language::AUTO), None);
        assert_eq!(language_warning(english, Language::new("en-US")), None);
        assert_eq!(language_warning(german, Language::new("de-DE")), None);
        assert_eq!(language_warning(german, Language::AUTO), None);

        // A warning is informational: the selection it describes stays valid and
        // the warning never replaces it.
        assert_eq!(english.variant.id, "moonshine-streaming-tiny");
        assert!(model_catalog::artifact(english.artifact.id).is_some());
    }

    #[test]
    fn steps_are_ordered_and_bounded() {
        assert_eq!(Step::Model.next(), Some(Step::Language));
        assert_eq!(Step::Language.next(), Some(Step::KeyBinding));
        assert_eq!(Step::KeyBinding.next(), Some(Step::SystemCheck));
        assert_eq!(Step::SystemCheck.next(), None);
        assert_eq!(Step::Model.previous(), None);
        assert_eq!(Step::SystemCheck.previous(), Some(Step::KeyBinding));
        assert_eq!(Step::SystemCheck.index(), 3);
        assert_eq!(
            Step::ALL.map(Step::title),
            ["Model", "Language", "Key binding", "System check"]
        );
    }

    #[test]
    fn model_state_line_pins_the_four_states() {
        let label = "Nemotron 3.5 ASR Streaming 0.6B — Q8_0";
        let info = |fallback: Option<&str>| crate::engine::EngineInfo {
            backend: "Vulkan".to_owned(),
            device: "AMD Radeon RX 6950 XT (RADV NAVI21)".to_owned(),
            fallback: fallback.map(str::to_owned),
        };
        let backend_text = "Vulkan — AMD Radeon RX 6950 XT (RADV NAVI21)".to_owned();

        // FR-001/FR-005: resident names the active backend and device.
        assert_eq!(
            model_state_line(&ModelStatus::Resident(info(None)), label),
            ModelStateLine {
                caption: "Backend",
                text: backend_text.clone(),
                note: None,
            }
        );
        // A CPU fallback keeps its reason (FR-005) under the same state.
        assert_eq!(
            model_state_line(
                &ModelStatus::Resident(info(Some("failed to initialize the Vulkan backend"))),
                label
            ),
            ModelStateLine {
                caption: "Backend",
                text: backend_text,
                note: Some((
                    "CPU fallback — failed to initialize the Vulkan backend".to_owned(),
                    Tone::Warning,
                )),
            }
        );
        // FR-002/FR-003: loading names the model and has no percentage.
        assert_eq!(
            model_state_line(&ModelStatus::Loading, label),
            ModelStateLine {
                caption: "Status",
                text: format!("Loading {label}…"),
                note: None,
            }
        );
        // FR-012: not resident states that the next dictation loads it.
        assert_eq!(
            model_state_line(&ModelStatus::NotResident, label),
            ModelStateLine {
                caption: "Status",
                text: format!("{label} is not loaded — the next dictation loads it"),
                note: None,
            }
        );
        // FR-016/Scenario 1.3: the failure carries its reason, never a stale backend.
        assert_eq!(
            model_state_line(
                &ModelStatus::Failed("model load failed for /tmp/x.gguf: boom".to_owned()),
                label
            ),
            ModelStateLine {
                caption: "Status",
                text: format!("Load failed — {label}"),
                note: Some((
                    "model load failed for /tmp/x.gguf: boom".to_owned(),
                    Tone::Error,
                )),
            }
        );
    }
}
