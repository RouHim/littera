//! Shared iced surfaces: messages, window plumbing, chord capture and the
//! vocabulary both UI modules build on.

use std::sync::LazyLock;

use iced::font::Weight;
use iced::keyboard::{Event as KeyEvent, key};
use iced::widget::text::Wrapping;
use iced::widget::{button, container, progress_bar, scrollable, text};
use iced::{Background, Element, Font, Length, Padding, Theme, border, window};

use crate::app::{App, AppEvent};
use crate::config::{BackendPreference, Config, Language};
use crate::diagnostics::Check;
use crate::hotkey::{self, KeyToken, Modifier};
use crate::models::{self, ModelEntry};

pub const ICON_PNG: &[u8] = include_bytes!("../assets/lingua-64.png");

/// The embedded application icon as straight (non-premultiplied) RGBA8,
/// decoded once on first use.
///
/// The asset is a build-time constant, so anything but 8-bit RGBA is a bug in
/// the repository rather than a runtime condition; callers fall back to "no
/// icon" and the daemon keeps working.
pub(crate) fn icon_rgba() -> Option<&'static (Vec<u8>, u32, u32)> {
    static PIXELS: LazyLock<Option<(Vec<u8>, u32, u32)>> = LazyLock::new(|| {
        let decoder = png::Decoder::new(std::io::Cursor::new(ICON_PNG));
        let mut reader = decoder.read_info().ok()?;
        let mut pixels = vec![0; reader.output_buffer_size()?];
        let frame = reader.next_frame(&mut pixels).ok()?;
        if frame.color_type != png::ColorType::Rgba || frame.bit_depth != png::BitDepth::Eight {
            tracing::warn!(?frame.color_type, "embedded icon is not 8-bit RGBA");
            return None;
        }
        pixels.truncate(frame.buffer_size());
        Some((pixels, frame.width, frame.height))
    });

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
    /// Re-run onboarding after completion (settings action).
    ReopenOnboarding,
    /// A message from the onboarding surface.
    Onboarding(crate::ui_onboarding::Action),
    /// A message from the settings surface.
    Settings(crate::ui_settings::Action),
}

/// The two windows the daemon can own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Onboarding,
    Settings,
}

/// The app palette: one dark foundation for both windows and the overlay
/// indicator (`overlay_render` reads the same constants through [`palette::rgb8`]).
///
/// The six roles map onto [`iced::theme::Palette`] (`background`, `text`,
/// `primary`, `success`, `warning`, `danger`); the surfaces, borders and the two
/// text tones are the tokens the views build with. Contrast on
/// [`palette::BACKGROUND`]: text 16.0:1, muted 6.6:1, accent 6.8:1, warning
/// 8.5:1, error 6.5:1, success 9.4:1 — all above WCAG AA, and `ON_ACCENT`
/// (7.2:1) is the only label colour allowed on an accent fill.
pub mod palette {
    use iced::Color;

    // Surfaces.
    /// Window background.
    pub const BACKGROUND: Color = Color::from_rgb8(0x1A, 0x1B, 0x26);
    /// Cards, chips and the segmented navigation.
    pub const SURFACE: Color = Color::from_rgb8(0x1F, 0x23, 0x35);
    /// Hovered and pressed surfaces.
    pub const SURFACE_HOVER: Color = Color::from_rgb8(0x29, 0x2E, 0x42);
    /// Hairline borders.
    pub const BORDER: Color = Color::from_rgb8(0x2F, 0x34, 0x48);
    /// Borders that carry state (hover, selection).
    pub const BORDER_STRONG: Color = Color::from_rgb8(0x3B, 0x42, 0x61);

    // Text tones.
    /// Primary text.
    pub const TEXT: Color = Color::from_rgb8(0xF2, 0xF2, 0xF7);
    /// Secondary text: labels, hints, details.
    pub const MUTED: Color = Color::from_rgb8(0x9A, 0xA0, 0xB4);

    // Accent and semantic roles.
    /// The single accent: selection, primary actions, focus.
    pub const ACCENT: Color = Color::from_rgb8(0x7A, 0xA2, 0xF7);
    pub const ACCENT_HOVER: Color = Color::from_rgb8(0x93, 0xB4, 0xF9);
    pub const ACCENT_PRESSED: Color = Color::from_rgb8(0x5F, 0x87, 0xD6);
    /// The label colour for accent fills; white on the accent would be 2.5:1.
    pub const ON_ACCENT: Color = Color::from_rgb8(0x16, 0x16, 0x1E);
    pub const SUCCESS: Color = Color::from_rgb8(0x9E, 0xCE, 0x6A);
    pub const WARNING: Color = Color::from_rgb8(0xE0, 0xAF, 0x68);
    pub const ERROR: Color = Color::from_rgb8(0xF7, 0x76, 0x8E);

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
    // Spacing: 4 / 8 / 12 / 16 / 24 / 32.
    pub const SPACE_XS: f32 = 4.0;
    pub const SPACE_SM: f32 = 8.0;
    pub const SPACE_MD: f32 = 12.0;
    pub const SPACE_LG: f32 = 16.0;
    pub const SPACE_XL: f32 = 24.0;

    // Shape: controls are rounded, surfaces more so, step chips are pills.
    pub const RADIUS_CONTROL: f32 = 8.0;
    pub const RADIUS_SURFACE: f32 = 12.0;
    /// The floating overlay indicator (`overlay_render`).
    pub const RADIUS_OVERLAY: f32 = 16.0;
    pub const RADIUS_PILL: f32 = 999.0;

    // Type scale, logical pixels.
    pub const TEXT_CAPTION: f32 = 13.0;
    pub const TEXT_BODY: f32 = 14.0;
    pub const TEXT_LEAD: f32 = 16.0;
    pub const TEXT_HEADING: f32 = 18.0;
    pub const TEXT_TITLE: f32 = 24.0;

    /// The padding of a labelled button: `[vertical, horizontal]`.
    pub const BUTTON_PADDING: [f32; 2] = [SPACE_SM, SPACE_LG];
    /// The padding of a segmented navigation item.
    pub const SEGMENT_PADDING: [f32; 2] = [SPACE_SM, SPACE_MD];

    // Scrollbar groove and scroller widths.
    pub const SCROLLBAR_WIDTH: f32 = 10.0;
    pub const SCROLLBAR_SCROLLER: f32 = 8.0;
}

/// The style vocabulary: every widget in both windows styles all of its statuses
/// from [`palette`], so nothing is left at a built-in default.
pub mod style {
    use super::{Tone, metrics, palette};
    use iced::widget::radio as radio_widget;
    use iced::widget::scrollable as scrollable_widget;
    use iced::widget::{button, container, progress_bar};
    use iced::{Background, Border, Color, Shadow, Theme, border};

    fn fill(color: Color) -> Option<Background> {
        Some(Background::Color(color))
    }

    fn outline(color: Color, radius: f32) -> Border {
        border::rounded(radius).color(color).width(1.0)
    }

    /// Primary action: accent fill with a dark label.
    pub fn primary_button(_theme: &Theme, status: button::Status) -> button::Style {
        let (background, text_color) = match status {
            button::Status::Active => (palette::ACCENT, palette::ON_ACCENT),
            button::Status::Hovered => (palette::ACCENT_HOVER, palette::ON_ACCENT),
            button::Status::Pressed => (palette::ACCENT_PRESSED, palette::ON_ACCENT),
            button::Status::Disabled => (palette::SURFACE, palette::MUTED),
        };

        button::Style {
            background: fill(background),
            text_color,
            border: outline(Color::TRANSPARENT, metrics::RADIUS_CONTROL),
            ..button::Style::default()
        }
    }

    /// Secondary action: surface fill with a visible border.
    pub fn secondary_button(_theme: &Theme, status: button::Status) -> button::Style {
        let (background, border_color, text_color) = match status {
            button::Status::Active => (palette::SURFACE, palette::BORDER_STRONG, palette::TEXT),
            button::Status::Hovered => (palette::SURFACE_HOVER, palette::ACCENT, palette::TEXT),
            button::Status::Pressed => (palette::BACKGROUND, palette::ACCENT, palette::TEXT),
            button::Status::Disabled => (Color::TRANSPARENT, palette::BORDER, palette::MUTED),
        };

        button::Style {
            background: fill(background),
            text_color,
            border: outline(border_color, metrics::RADIUS_CONTROL),
            ..button::Style::default()
        }
    }

    /// One item of a segmented navigation; `selected` marks the active view.
    pub fn segment(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style + 'static {
        move |_theme, status| {
            let (background, border_color, text_color) = match (selected, status) {
                (true, button::Status::Active) => (
                    palette::alpha(palette::ACCENT, 0.16),
                    palette::ACCENT,
                    palette::ACCENT,
                ),
                (true, button::Status::Hovered) => (
                    palette::alpha(palette::ACCENT, 0.24),
                    palette::ACCENT,
                    palette::ACCENT,
                ),
                (true, button::Status::Pressed) => (
                    palette::alpha(palette::ACCENT, 0.30),
                    palette::ACCENT,
                    palette::ACCENT,
                ),
                (true, button::Status::Disabled) => (
                    palette::alpha(palette::ACCENT, 0.10),
                    palette::BORDER,
                    palette::MUTED,
                ),
                (false, button::Status::Hovered) => (
                    palette::SURFACE_HOVER,
                    palette::BORDER_STRONG,
                    palette::TEXT,
                ),
                (false, button::Status::Pressed) => {
                    (palette::SURFACE, palette::BORDER_STRONG, palette::TEXT)
                }
                (false, button::Status::Active | button::Status::Disabled) => {
                    (palette::SURFACE, palette::BORDER_STRONG, palette::MUTED)
                }
            };

            button::Style {
                background: fill(background),
                text_color,
                border: outline(border_color, metrics::RADIUS_CONTROL),
                ..button::Style::default()
            }
        }
    }

    /// A card surface.
    pub fn card(_theme: &Theme) -> container::Style {
        container::Style {
            background: fill(palette::SURFACE),
            border: outline(palette::BORDER, metrics::RADIUS_SURFACE),
            text_color: Some(palette::TEXT),
            ..container::Style::default()
        }
    }

    /// The same card with an accent border: the selected model.
    pub fn card_selected(theme: &Theme) -> container::Style {
        container::Style {
            border: outline(
                palette::alpha(palette::ACCENT, 0.55),
                metrics::RADIUS_SURFACE,
            ),
            ..card(theme)
        }
    }

    /// A row outline without a fill: system-check rows.
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
            background: fill(palette::SURFACE),
            border: outline(palette::BORDER, metrics::RADIUS_SURFACE),
            ..container::Style::default()
        }
    }

    /// A status panel tinted with its tone.
    pub fn banner(tone: Tone) -> impl Fn(&Theme) -> container::Style + 'static {
        let color = tone.color();

        move |_theme| container::Style {
            background: fill(palette::alpha(color, 0.14)),
            border: outline(palette::alpha(color, 0.45), metrics::RADIUS_CONTROL),
            text_color: Some(color),
            ..container::Style::default()
        }
    }

    /// Determinate progress (model download). The track is the strong border
    /// tone so the filled/unfilled boundary stays visible against the bar; the
    /// percentage next to it carries the exact value.
    pub fn progress(_theme: &Theme) -> progress_bar::Style {
        progress_bar::Style {
            background: Background::Color(palette::BORDER_STRONG),
            bar: Background::Color(palette::ACCENT),
            border: border::rounded(metrics::SPACE_XS / 2.0),
        }
    }

    /// A radio option; the dot carries the accent, the ring carries the hover.
    /// The ring is a text tone so the unselected control stays above the 3:1
    /// non-text contrast minimum.
    pub fn radio(_theme: &Theme, status: radio_widget::Status) -> radio_widget::Style {
        let border_color = match status {
            radio_widget::Status::Active { .. } => palette::MUTED,
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

    /// The scrollbars of the two scrolling regions.
    pub fn scrollable(
        _theme: &Theme,
        status: scrollable_widget::Status,
    ) -> scrollable_widget::Style {
        let scroller = match status {
            scrollable_widget::Status::Active { .. } => palette::BORDER_STRONG,
            scrollable_widget::Status::Hovered { .. }
            | scrollable_widget::Status::Dragged { .. } => palette::ACCENT,
        };
        let rail = scrollable_widget::Rail {
            background: None,
            border: border::rounded(metrics::SPACE_XS / 2.0),
            scroller: scrollable_widget::Scroller {
                background: Background::Color(scroller),
                border: border::rounded(metrics::SPACE_XS / 2.0),
            },
        };

        scrollable_widget::Style {
            container: container::Style::default(),
            vertical_rail: rail,
            horizontal_rail: rail,
            gap: None,
            auto_scroll: scrollable_widget::AutoScroll {
                background: Background::Color(palette::SURFACE),
                border: outline(palette::BORDER, metrics::RADIUS_SURFACE),
                shadow: Shadow::default(),
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
    let (width, height) = match surface {
        Surface::Onboarding => (820.0, 660.0),
        Surface::Settings => (900.0, 720.0),
    };

    window::Settings {
        size: iced::Size::new(width, height),
        position: window::Position::Centered,
        min_size: Some(iced::Size::new(620.0, 480.0)),
        icon: window_icon(),
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
        None => iced::widget::space().into(),
    }
}

/// Window titles (the OS-level title bar, not a view heading).
pub fn window_title(app: &App, id: window::Id) -> String {
    match app.windows.get(&id) {
        Some(Surface::Onboarding) => "Lingua — Setup".to_owned(),
        Some(Surface::Settings) => "Lingua — Settings".to_owned(),
        None => "Lingua".to_owned(),
    }
}

/// The scrollbar both surfaces use for their content region.
pub fn scroll_direction() -> scrollable::Direction {
    scrollable::Direction::Vertical(
        scrollable::Scrollbar::new()
            .width(metrics::SCROLLBAR_WIDTH)
            .scroller_width(metrics::SCROLLBAR_SCROLLER),
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
            "Lingua",
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
        let entry = models::entry(&cfg.model).unwrap_or_else(models::default_entry);
        Self {
            step: Step::Model,
            model: entry.id.to_owned(),
            language: cfg.language,
            hotkey: cfg.hotkey.clone(),
            capture: ChordCapture::default(),
            model_ready: models::path_for(entry)
                .map(|path| path.exists())
                .unwrap_or(false),
        }
    }

    pub fn entry(&self) -> &'static ModelEntry {
        models::entry(&self.model).unwrap_or_else(models::default_entry)
    }

    /// FR-002/Scenario 1: an English-only model cannot transcribe German.
    pub fn language_conflict(&self) -> Option<&'static str> {
        language_conflict(self.entry(), self.language)
    }
}

/// The exact warning shown (and `Next` blocked) when the English-only model is
/// paired with German or automatic language detection.
pub fn language_conflict(entry: &ModelEntry, language: Language) -> Option<&'static str> {
    if matches!(language, Language::DeDe | Language::Auto) && !entry.multilingual {
        Some("The English-only model cannot transcribe German — choose a multilingual model.")
    } else {
        None
    }
}

/// Settings sections (FR-016/FR-022).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Dictation,
    SystemCheck,
    Diagnostics,
}

impl Section {
    pub const ALL: [Section; 3] = [
        Section::Dictation,
        Section::SystemCheck,
        Section::Diagnostics,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Dictation => "Dictation",
            Section::SystemCheck => "System check",
            Section::Diagnostics => "Diagnostics",
        }
    }
}

/// Editable settings state (applied on save, Scenario 3.4).
#[derive(Debug, Clone)]
pub struct SettingsUi {
    pub section: Section,
    pub model: String,
    pub language: Language,
    pub backend: BackendPreference,
    pub hotkey: String,
    pub hotkey_draft: Option<String>,
    pub capture: ChordCapture,
    pub note: Option<String>,
}

impl SettingsUi {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            section: Section::Dictation,
            model: cfg.model.clone(),
            language: cfg.language,
            backend: cfg.backend,
            hotkey: cfg.hotkey.clone(),
            hotkey_draft: None,
            capture: ChordCapture::default(),
            note: None,
        }
    }

    pub fn entry(&self) -> &'static ModelEntry {
        models::entry(&self.model).unwrap_or_else(models::default_entry)
    }

    pub fn language_conflict(&self) -> Option<&'static str> {
        language_conflict(self.entry(), self.language)
    }

    /// The chord to apply on save: the captured draft, else the current one.
    pub fn effective_hotkey(&self) -> String {
        self.hotkey_draft
            .clone()
            .unwrap_or_else(|| self.hotkey.clone())
    }

    pub fn dirty(&self, cfg: &Config) -> bool {
        self.model != cfg.model
            || self.language != cfg.language
            || self.backend != cfg.backend
            || self.hotkey_draft.is_some()
    }
}

// ------------------------------------------------------------- small widgets

/// The semibold face of the UI font: titles, headings, emphasised labels.
fn semibold() -> Font {
    Font {
        weight: Weight::Semibold,
        ..Font::DEFAULT
    }
}

/// A surface title.
pub fn title<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_TITLE)
        .font(semibold())
        .color(palette::TEXT)
        .line_height(1.2)
        .into()
}

/// A section or step heading.
pub fn heading<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_HEADING)
        .font(semibold())
        .color(palette::TEXT)
        .line_height(1.2)
        .into()
}

/// An emphasised inline label.
pub fn strong<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_BODY)
        .font(semibold())
        .color(palette::TEXT)
        .into()
}

/// A muted label, sized to its content (inline use).
pub fn muted<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_CAPTION)
        .color(palette::MUTED)
        .into()
}

/// A muted explanation line that wraps instead of overflowing.
pub fn lead<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_BODY)
        .color(palette::MUTED)
        .width(Length::Fill)
        .wrapping(Wrapping::WordOrGlyph)
        .into()
}

/// A monospace data value, sized to its content (inline use).
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

/// The configured key chord (an identifier: monospace, one step larger).
pub fn chord<M: 'static>(content: impl Into<String>) -> Element<'static, M> {
    text(content.into())
        .size(metrics::TEXT_LEAD)
        .font(Font::MONOSPACE)
        .color(palette::TEXT)
        .into()
}

/// A card: one surface, radius and padding for grouped content.
pub fn card<M: 'static>(content: impl Into<Element<'static, M>>) -> Element<'static, M> {
    surface(content, style::card)
}

/// The selected card: same surface, accent border.
pub fn card_active<M: 'static>(content: impl Into<Element<'static, M>>) -> Element<'static, M> {
    surface(content, style::card_selected)
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

/// A tinted status banner.
pub fn banner<M: 'static>(content: impl Into<String>, tone: Tone) -> Element<'static, M> {
    container(
        text(content.into())
            .size(metrics::TEXT_CAPTION)
            .color(tone.color())
            .width(Length::Fill)
            .wrapping(Wrapping::WordOrGlyph),
    )
    .padding(metrics::SPACE_MD)
    .width(Length::Fill)
    .style(style::banner(tone))
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
        ChipState::Current => (
            palette::alpha(palette::ACCENT, 0.16),
            palette::ACCENT,
            palette::ACCENT,
        ),
        ChipState::Done => (palette::SURFACE, palette::BORDER_STRONG, palette::TEXT),
        ChipState::Upcoming => (palette::BACKGROUND, palette::BORDER_STRONG, palette::MUTED),
    };

    container(
        text(format!("{number}. {label}"))
            .size(metrics::TEXT_CAPTION)
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

/// Model status and progress, shared by onboarding and settings (FR-002/FR-003).
///
/// `download`, `cancel` and `retry` are the surface-specific messages for the
/// three actions.
pub fn model_panel<M: Clone + 'static>(
    app: &App,
    entry: &'static ModelEntry,
    verified: bool,
    download: M,
    cancel: M,
    retry: M,
) -> Element<'static, M> {
    let path = models::path_for(entry).ok();
    let on_disk = path.as_ref().is_some_and(|path| path.exists());
    let downloading = app
        .download
        .as_ref()
        .filter(|state| state.entry.id == entry.id);

    if let Some(state) = downloading
        && state.is_running()
    {
        let row = iced::widget::row![
            progress_bar(0.0..=100.0, state.percent())
                .length(Length::Fill)
                .style(style::progress),
            text(format!("{:.0}%", state.percent()))
                .size(metrics::TEXT_CAPTION)
                .font(Font::MONOSPACE)
                .color(palette::MUTED),
            button("Cancel")
                .style(style::secondary_button)
                .on_press(cancel),
        ]
        .spacing(metrics::SPACE_MD)
        .align_y(iced::Alignment::Center);

        return card(
            iced::widget::column![
                strong(format!(
                    "Downloading the model — {}",
                    entry.approx_size_label
                )),
                row,
            ]
            .spacing(metrics::SPACE_MD),
        );
    }

    if let Some(state) = downloading
        && let Some(error) = &state.error
    {
        return card(
            iced::widget::column![
                banner(error.clone(), Tone::Error),
                iced::widget::row![
                    button("Retry")
                        .style(style::secondary_button)
                        .on_press(retry),
                    muted("The partial download is kept and resumed."),
                ]
                .spacing(metrics::SPACE_MD)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(metrics::SPACE_MD),
        );
    }

    let (status, action) = if verified {
        (banner("Verified — ready to use", Tone::Success), "Verify")
    } else if on_disk {
        (
            banner(
                "Already on disk — verified against its SHA-256 before use",
                Tone::Info,
            ),
            "Verify",
        )
    } else {
        (
            banner(
                format!(
                    "Not downloaded yet — {} to download",
                    entry.approx_size_label
                ),
                Tone::Warning,
            ),
            "Download",
        )
    };

    card(
        iced::widget::column![
            status,
            iced::widget::row![
                button(action)
                    .style(style::primary_button)
                    .on_press(download)
            ]
            .spacing(metrics::SPACE_SM),
        ]
        .spacing(metrics::SPACE_MD),
    )
}

/// One system-check row: status, detail and remediation (FR-014).
pub fn check_row<M: 'static>(check: &Check) -> Element<'static, M> {
    let (tone, marker) = if check.ok {
        (Tone::Success, "[ok]")
    } else {
        (Tone::Error, "[fail]")
    };

    let mut body = iced::widget::column![
        iced::widget::row![
            text(marker)
                .size(metrics::TEXT_CAPTION)
                .font(Font::MONOSPACE)
                .color(tone.color()),
            strong(check.name.to_owned()),
        ]
        .spacing(metrics::SPACE_SM)
        .align_y(iced::Alignment::Center),
        lead(check.detail.clone()),
    ]
    .spacing(metrics::SPACE_XS);

    if let Some(remediation) = &check.remediation {
        body = body.push(mono::<M>(remediation.clone()));
    }

    container(body)
        .padding(metrics::SPACE_MD)
        .width(Length::Fill)
        .style(style::outline_row)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::CATALOG;

    fn physical(code: key::Code) -> key::Physical {
        key::Physical::Code(code)
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
    fn conflict_warning_blocks_german_for_the_english_model() {
        let english = CATALOG
            .iter()
            .find(|entry| !entry.multilingual)
            .expect("the catalog has an English-only entry");
        let multilingual = CATALOG
            .iter()
            .find(|entry| entry.multilingual)
            .expect("the catalog has a multilingual entry");

        assert_eq!(
            language_conflict(english, Language::DeDe),
            Some("The English-only model cannot transcribe German — choose a multilingual model.")
        );
        assert!(language_conflict(english, Language::Auto).is_some());
        assert_eq!(language_conflict(english, Language::EnUs), None);
        assert_eq!(language_conflict(multilingual, Language::DeDe), None);
        assert_eq!(language_conflict(multilingual, Language::Auto), None);
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
    fn settings_effective_hotkey_prefers_the_draft() {
        let cfg = Config::default();
        let mut settings = SettingsUi::from_config(&cfg);
        assert_eq!(settings.effective_hotkey(), cfg.hotkey);
        assert!(!settings.dirty(&cfg));

        settings.hotkey_draft = Some("CTRL+SHIFT+D".to_owned());
        assert_eq!(settings.effective_hotkey(), "CTRL+SHIFT+D");
        assert!(settings.dirty(&cfg));
    }
}
