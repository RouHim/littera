//! Settings surface (FR-016/FR-022, Scenario 3).
//!
//! Every change applies immediately — there is no Save button. A change that
//! cannot apply (a failed config write) keeps the previous value and is reported
//! in the pinned footer instead.
//!
//! The model section renders the bundled [`model_catalog`] snapshot: one compact
//! two-line field row per streaming variant — name and download size, the state
//! with its one action, the language summary, a quant selector and the
//! quality/speed bars — with filters, sorting and an expandable detail for the
//! exact published values, the parameter count and the license
//! (FR-003/FR-004/FR-005, `.spec/model-list-field-rows.md`). A language mismatch
//! is a marker on the row and never blocks a selection (FR-011). The list is
//! plain page content, so the page scroll is the only scroll: the variants never
//! get a nested scroll area of their own, and the pinned snapshot version (FR-002)
//! is read in the Storage card like every other machine fact.
//!
//! Below the list the section renders the selected model's load lifecycle as one
//! card (FR-001/FR-002/FR-012): loading, the resident backend and device, the
//! not-resident state, or the failure reason — never a stale backend line. The
//! same section carries the idle-unload timeout (FR-006/FR-007): one radio per
//! preset, autosaved like every other setting.

use std::fmt;

use iced::widget::{
    button, column, container, pick_list, progress_bar, radio, row, scrollable, text_input,
};
use iced::{Element, Length, Task};

use crate::app::App;
use crate::audio;
use crate::config::{self, Config, IdleUnload, Language};
use crate::model_catalog::{self, Artifact, Selection, Variant};
use crate::models;
use crate::ui::{self, Message, Tone, metrics, palette, style};
use crate::ui_icons::{self, Icon};

/// Actions the settings surface can produce.
#[derive(Debug, Clone)]
pub enum Action {
    /// Select an artifact by catalog id (the radio of a card, a quant chip of
    /// the active variant).
    SelectModel(&'static str),
    /// Switch the quant a non-active card shows; the active variant is selected
    /// for real instead.
    SelectQuant {
        variant: &'static str,
        quant: &'static str,
    },
    Download(&'static str),
    CancelDownload,
    RetryDownload(&'static str),
    ToggleDetail(&'static str),
    SetSearch(String),
    SetLanguageFilter(LanguageFilter),
    SetSort(SortOrder),
    SelectLanguage(Language),
    /// Pick the model idle-unload timeout (FR-006).
    SelectIdleUnload(config::IdleUnload),
    /// Pick the capture device: its `alsa:…` identifier, or `system` (FR-007).
    SelectInputDevice(String),
    BindViaPortal,
    StartCapture,
    OpenModelDir,
}

/// Which languages the model list keeps (FR-005): all of them, or exactly one
/// the catalog can transcribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LanguageFilter(Option<Language>);

impl LanguageFilter {
    /// The unrestricted filter — the default.
    pub const ALL: Self = Self(None);

    /// The one-language filter.
    pub const fn only(language: Language) -> Self {
        Self(Some(language))
    }

    /// Every option, in the order the pick list renders them: all languages,
    /// then one entry per language the catalog offers, by display name.
    pub fn options() -> Vec<Self> {
        std::iter::once(Self::ALL)
            .chain(
                Language::ALL
                    .into_iter()
                    .filter(|language| !language.is_auto())
                    .map(Self::only),
            )
            .collect()
    }

    /// Whether a variant is listed under this filter. A model serves a language
    /// in whichever spelling it advertises it (`de` for `de-DE`), which
    /// [`model_catalog::supports`] resolves.
    fn keeps(self, variant: &Variant) -> bool {
        self.0
            .is_none_or(|language| model_catalog::supports(variant, language))
    }
}

impl fmt::Display for LanguageFilter {
    /// The option as the pick list renders it: the language's own name, or
    /// "All languages".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            None => f.write_str("All languages"),
            Some(language) => f.write_str(language.display_name()),
        }
    }
}

/// Order of the model list (FR-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Name,
    Quality,
    Speed,
    Size,
}

impl SortOrder {
    /// Every option, in the order the chips are rendered.
    pub const ALL: [SortOrder; 4] = [Self::Name, Self::Quality, Self::Speed, Self::Size];

    const fn label(self) -> &'static str {
        match self {
            SortOrder::Name => "Name",
            SortOrder::Quality => "Quality",
            SortOrder::Speed => "Speed",
            SortOrder::Size => "Size",
        }
    }
}

/// The indicator bars are reference marks, not progress: thin and short — a
/// row's fact line is the scan layer, the numbers live in the detail
/// (`model-list-field-rows` FR-001).
const INDICATOR_BAR_WIDTH: f32 = 44.0;
/// The compact padding of a row's quant chip.
const CHIP_PADDING: [f32; 2] = [2.0, metrics::SPACE_SM];
/// The width of a running transfer's bar in a row's identity line.
const TRANSFER_BAR_WIDTH: f32 = 96.0;
const INDICATOR_BAR_HEIGHT: f32 = 4.0;
/// The dash a quant without a published measurement shows (SC-007).
const NO_MEASUREMENT: &str = "—";

/// The note under a configured device that cannot be opened right now (FR-006).
const CURRENTLY_UNAVAILABLE: &str = "currently unavailable";
/// The label of a hand-edited device value that is empty (spec Edge Cases).
const EMPTY_DEVICE_LABEL: &str = "(empty)";
/// The section's progress note while the device list is being built (FR-009).
const SCANNING_INPUT_DEVICES: &str = "Scanning input devices…";
/// Joins the parts of one device option: name, identifier, note.
const SEPARATOR: &str = "  ·  ";

/// Caption of the catalog provenance line (FR-002): the snapshot row of the
/// Storage card states the pinned catalog the list is rendered from.
const SNAPSHOT_LABEL: &str = "Catalog snapshot";

/// Session-only list state: the filters, the sorting, the expanded detail and
/// the quants the user picked on cards that are not active.
#[derive(Debug, Clone, Default)]
pub struct ListState {
    pub search: String,
    pub language: LanguageFilter,
    pub sort: SortOrder,
    pub expanded: Option<&'static str>,
    /// Variant id → quant for cards the user configured without selecting them.
    pub quant_overrides: Vec<(&'static str, &'static str)>,
}

impl ListState {
    /// The quant a card shows: the active artifact for the active variant, else
    /// the user's pick, else the recommended quant (FR-003).
    pub fn quant_for(&self, variant: &'static Variant, active: Selection) -> &'static Artifact {
        if variant.id == active.variant.id {
            return active.artifact;
        }
        self.quant_overrides
            .iter()
            .find(|(id, _)| *id == variant.id)
            .and_then(|(_, quant)| {
                variant
                    .downloads
                    .iter()
                    .find(|artifact| artifact.quant == *quant)
            })
            .unwrap_or_else(|| model_catalog::recommended(variant))
    }

    /// Record the quant a card shows without selecting it.
    pub fn set_quant(&mut self, variant: &'static str, quant: &'static str) {
        match self
            .quant_overrides
            .iter_mut()
            .find(|(id, _)| *id == variant)
        {
            Some(entry) => entry.1 = quant,
            None => self.quant_overrides.push((variant, quant)),
        }
    }

    /// The rows to render: filters applied, then sorted (SC-005).
    pub fn visible(&self, active: Selection, language: Language) -> Vec<Selection> {
        let query = self.search.trim().to_lowercase();
        let mut visible: Vec<Selection> = model_catalog::VARIANTS
            .iter()
            .filter(|variant| {
                self.language.keeps(variant)
                    && (query.is_empty()
                        || variant.label.to_lowercase().contains(&query)
                        || variant.id.to_lowercase().contains(&query)
                        || variant.family.to_lowercase().contains(&query))
            })
            .map(|variant| Selection {
                variant,
                artifact: self.quant_for(variant, active),
            })
            .collect();

        // A missing measurement sorts last instead of counting as `None`
        // (FR-004/SC-007): the unrated models must not lead the list. Every
        // order ties by name, so the list is deterministic.
        let by_name = |left: &Selection, right: &Selection| {
            left.variant
                .label
                .cmp(right.variant.label)
                .then_with(|| left.variant.id.cmp(right.variant.id))
        };

        match self.sort {
            SortOrder::Name => visible.sort_by(by_name),
            SortOrder::Quality => visible.sort_by(|left, right| {
                let key = |selection: &Selection| {
                    model_catalog::quality(*selection, language).map(|cell| cell.err_pct)
                };
                match (key(left), key(right)) {
                    (Some(left_wer), Some(right_wer)) => left_wer
                        .total_cmp(&right_wer)
                        .then_with(|| by_name(left, right)),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => by_name(left, right),
                }
            }),
            SortOrder::Speed => visible.sort_by(|left, right| {
                let key =
                    |selection: &Selection| model_catalog::speed(*selection).map(|row| row.xrt);
                match (key(left), key(right)) {
                    // Highest throughput first; only the missing value sorts last.
                    (Some(left_xrt), Some(right_xrt)) => right_xrt
                        .total_cmp(&left_xrt)
                        .then_with(|| by_name(left, right)),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => by_name(left, right),
                }
            }),
            SortOrder::Size => visible.sort_by(|left, right| {
                left.artifact
                    .size_bytes
                    .cmp(&right.artifact.size_bytes)
                    .then_with(|| by_name(left, right))
            }),
        }

        visible
    }
}

pub fn update(app: &mut App, action: Action) -> Vec<Task<Message>> {
    match action {
        Action::SelectModel(id) => return app.select_artifact(id),
        Action::SelectQuant { variant, quant } => {
            return select_quant(app, variant, quant);
        }
        Action::Download(id) | Action::RetryDownload(id) => {
            if let Some(selection) = model_catalog::artifact(id) {
                return vec![app.start_download(selection)];
            }
        }
        Action::CancelDownload => app.cancel_download(),
        Action::ToggleDetail(variant) => {
            app.settings.list.expanded = if app.settings.list.expanded == Some(variant) {
                None
            } else {
                Some(variant)
            };
        }
        Action::SetSearch(query) => app.settings.list.search = query,
        Action::SetLanguageFilter(filter) => app.settings.list.language = filter,
        Action::SetSort(order) => app.settings.list.sort = order,
        Action::SelectLanguage(language) => return select_language(app, language),
        Action::SelectIdleUnload(value) => return select_idle_unload(app, value),
        Action::SelectInputDevice(device) => return select_input_device(app, device),
        Action::OpenModelDir => return app.open_model_dir(),
        // The portal binds the configured chord; the compositor's own dialog
        // reports which trigger it actually registered (Scenario 1.3).
        Action::BindViaPortal => {
            let chord = app.cfg.snapshot().hotkey;
            return vec![crate::app::bind_task(
                &chord,
                Some(config::HotkeySource::Portal),
            )];
        }
        Action::StartCapture => app.settings.capture.start(),
    }

    Vec::new()
}

/// Pick the quant a card shows (FR-003): on the active variant that is a real
/// selection, on any other card it only changes what the card will download.
fn select_quant(app: &mut App, variant: &'static str, quant: &'static str) -> Vec<Task<Message>> {
    let Some(found) = model_catalog::VARIANTS
        .iter()
        .find(|candidate| candidate.id == variant)
    else {
        tracing::warn!(variant, "unknown catalog variant; ignoring the quant");
        return Vec::new();
    };
    let Some(artifact) = found
        .downloads
        .iter()
        .find(|candidate| candidate.quant == quant)
    else {
        tracing::warn!(variant, quant, "unknown quant; ignoring the selection");
        return Vec::new();
    };

    if found.id == app.model.variant.id {
        return app.select_artifact(artifact.id);
    }
    app.settings.list.set_quant(found.id, quant);
    Vec::new()
}

/// Switch the language hint (FR-007). The configured model does not have to
/// serve it: a mismatch is a warning on the model card, not a rejection
/// (FR-011), and the engine runs without a hint in that case.
fn select_language(app: &mut App, language: Language) -> Vec<Task<Message>> {
    let config = app.cfg.snapshot();
    if config.language == language {
        return Vec::new();
    }

    if let Err(error) = app.cfg.update(|cfg| cfg.language = language) {
        tracing::error!(%error, "saving the language selection failed");
        app.settings.status = Some(ui::SettingsStatus {
            text: format!("Saving failed: {error:#}"),
            tone: Tone::Error,
            section: ui::SettingsSection::Language,
        });
        return Vec::new();
    }

    app.settings.status = None;
    app.refresh_checks();
    Vec::new()
}

/// Persist the idle-unload timeout (FR-007). The running countdown reads the
/// shared config on every tick, so the change applies without a restart, and a
/// shorter timeout that is already exceeded unloads within the next tick
/// (Scenario 3.3).
fn select_idle_unload(app: &mut App, value: IdleUnload) -> Vec<Task<Message>> {
    if app.cfg.snapshot().idle_unload == value {
        return Vec::new();
    }
    if let Err(error) = app.cfg.update(|cfg| cfg.idle_unload = value) {
        tracing::error!(%error, "saving the idle-unload timeout failed");
        app.settings.status = Some(ui::SettingsStatus {
            text: format!("Saving failed: {error:#}"),
            tone: Tone::Error,
            section: ui::SettingsSection::Model,
        });
        return Vec::new();
    }
    app.settings.status = None;
    tracing::info!(timeout = value.label(), "idle unload timeout changed");
    Vec::new()
}

/// Persist the chosen capture device (FR-007): a plain config write, so the running recording
/// keeps its device and the next one opens the new one — nothing is re-bound or restarted.
fn select_input_device(app: &mut App, device: String) -> Vec<Task<Message>> {
    if app.cfg.snapshot().input_device == device {
        return Vec::new();
    }

    if let Err(error) = app.cfg.update(|cfg| cfg.input_device = device) {
        tracing::error!(%error, "saving the input device selection failed");
        app.settings.status = Some(ui::SettingsStatus {
            text: format!("Saving failed: {error:#}"),
            tone: Tone::Error,
            section: ui::SettingsSection::Microphone,
        });
        return Vec::new();
    }

    app.settings.status = None;
    Vec::new()
}

pub fn view(app: &App) -> Element<'_, Action> {
    column![
        scrollable(dictation_section(app))
            .height(Length::Fill)
            .direction(ui::scroll_direction())
            .style(style::scrollable)
    ]
    .padding(metrics::SPACE_XL)
    .into()
}

fn dictation_section(app: &App) -> Element<'_, Action> {
    let ui = &app.settings;
    let config = app.cfg.snapshot();
    let active = app.model;
    let visible = ui.list.visible(active, config.language);

    let mut model_section = column![ui::heading::<Action>("Model")].spacing(metrics::SPACE_MD);
    if let Some(status) = ui.status_for(ui::SettingsSection::Model) {
        model_section = model_section.push(ui::banner(status.text.clone(), status.tone));
    }
    // FR-011: a mismatch is reported, but nothing is greyed out or hidden.
    if let Some(warning) = model_catalog::language_warning(active.variant, config.language) {
        model_section = model_section.push(ui::banner(warning, Tone::Warning));
    }
    model_section = model_section.push(controls(app));
    model_section = model_section.push(list(app, &visible, config.language));

    // FR-007: one entry per language the catalog can serve, `Auto-detect` (the
    // default) first — the same vocabulary the model filter uses.
    let language_picker = pick_list(Language::ALL, Some(config.language), Action::SelectLanguage)
        .width(Length::Fill)
        .padding(metrics::BUTTON_PADDING)
        .text_size(metrics::TEXT_BODY)
        .style(style::pick_list)
        .menu_style(style::pick_list_menu);

    let mut language_card = column![ui::card(language_picker)].spacing(metrics::SPACE_MD);
    if let Some(status) = ui.status_for(ui::SettingsSection::Language) {
        language_card = language_card.push(ui::banner(status.text.clone(), status.tone));
    } else if let Some(model) = model_catalog::language_warning(active.variant, config.language) {
        // A hand-edited config can arrive in a mismatched state; the warning
        // stays until one side is changed.
        language_card = language_card.push(ui::banner(model, Tone::Warning));
    }

    let mut details = column![
        row![ui::key::<Action>("Chord"), ui::chord(config.hotkey.clone())]
            .spacing(metrics::SPACE_SM)
            .align_y(iced::Alignment::Center),
    ]
    .spacing(metrics::SPACE_SM);

    if let Some(active) = &app.binding {
        details = details.push(ui::lead::<Action>(format!(
            "Active mechanism: {} — {}",
            active.source.label(),
            active.trigger_description
        )));
    }

    if let Some(error) = &app.hotkey_error {
        details = details.push(ui::banner(error.detail.clone(), Tone::Error));
        details = details.push(ui::mono::<Action>(error.remediation.clone()));
    } else if let Some(error) = ui.capture.error() {
        details = details.push(ui::banner(error.to_owned(), Tone::Error));
    }

    if let Some(status) = ui.status_for(ui::SettingsSection::Hotkey) {
        details = details.push(ui::banner(status.text.clone(), status.tone));
    }

    let bind_action: Element<'_, Action> = if ui.capture.is_capturing() {
        let preview = ui
            .capture
            .preview()
            .map_or_else(|| "Press the combination…".to_owned(), str::to_owned);
        ui::banner(preview, Tone::Info)
    } else if app.portal_available == Some(true) {
        row![
            button(ui::stamp("Bind…"))
                .style(style::primary_button)
                .padding(metrics::BUTTON_PADDING)
                .on_press(Action::BindViaPortal),
            ui::muted::<Action>("The compositor asks for the shortcut in its own dialog."),
        ]
        .spacing(metrics::SPACE_MD)
        .align_y(iced::Alignment::Center)
        .into()
    } else {
        button(ui::stamp("Bind…"))
            .style(style::primary_button)
            .padding(metrics::BUTTON_PADDING)
            .on_press(Action::StartCapture)
            .into()
    };

    let hotkey_card = ui::card(column![details, bind_action].spacing(metrics::SPACE_MD));

    let mut storage = column![
        column![
            ui::key::<Action>(SNAPSHOT_LABEL),
            ui::mono::<Action>(model_catalog::SNAPSHOT_VERSION),
        ]
        .spacing(metrics::SPACE_XS),
        column![
            ui::key::<Action>("Models are stored in"),
            button(ui::mono::<Action>(app.model_dir_display()))
                .style(style::link)
                .padding([metrics::SPACE_XS, 0.0])
                .width(Length::Fill)
                .on_press(Action::OpenModelDir),
        ]
        .spacing(metrics::SPACE_XS),
        column![
            ui::key::<Action>("Space used"),
            ui::mono::<Action>(app.model_dir_usage_display()),
        ]
        .spacing(metrics::SPACE_XS),
        column![
            ui::key::<Action>("Current model file"),
            ui::mono::<Action>(app.model.artifact.file_name),
        ]
        .spacing(metrics::SPACE_XS),
    ]
    .spacing(metrics::SPACE_MD);

    if let Some(status) = ui.status_for(ui::SettingsSection::Storage) {
        storage = storage.push(ui::banner(status.text.clone(), status.tone));
    }

    column![
        capability_plate(app),
        model_section,
        // The idle-unload setting is its own section: inside the model section it
        // read as another row of the list (same plate, same 12 px rhythm).
        column![
            ui::heading::<Action>(IDLE_UNLOAD_LABEL),
            idle_unload_card(app)
        ]
        .spacing(metrics::SPACE_MD),
        column![ui::heading::<Action>("Language"), language_card].spacing(metrics::SPACE_MD),
        column![ui::heading::<Action>("Microphone"), microphone_section(app)]
            .spacing(metrics::SPACE_MD),
        column![ui::heading::<Action>("Hotkey"), hotkey_card].spacing(metrics::SPACE_MD),
        column![ui::heading::<Action>("Storage"), ui::card(storage)].spacing(metrics::SPACE_MD),
    ]
    .spacing(metrics::SPACE_XXL)
    .into()
}

/// The capability plate (FR-005/FR-022): the machine's current truth, stamped
/// once — model, backend, device and hotkey as label/value fields with one
/// state lamp each. A fallback or a load failure keeps its reason in a placard
/// under the frame; nothing here is ever a stale line.
fn capability_plate(app: &App) -> Element<'_, Action> {
    let config = app.cfg.snapshot();
    let selection = app.model;
    // The plate states the engine's line in its own short form; the pinned
    // lifecycle strings stay with `model_state_line` (and its tests), whose note
    // is the only part the plate reuses.
    let backend_text = match &app.model_status {
        crate::app::ModelStatus::Resident(info) => format!("{} — {}", info.backend, info.device),
        crate::app::ModelStatus::Loading => "Loading…".to_owned(),
        crate::app::ModelStatus::Failed(_) => "Load failed".to_owned(),
        crate::app::ModelStatus::NotResident => {
            "Not loaded — the next dictation loads it".to_owned()
        }
    };
    let note = ui::model_state_line(&app.model_status, &selection.label()).note;

    let model_lamp: Element<'_, Action> = match app.model_status {
        crate::app::ModelStatus::Resident(_) => ui::lamp(Tone::Success),
        crate::app::ModelStatus::Loading => ui::lamp(Tone::Warning),
        crate::app::ModelStatus::Failed(_) => ui::lamp(Tone::Error),
        crate::app::ModelStatus::NotResident => ui::lamp_off(),
    };
    let backend_lamp: Element<'_, Action> = match &app.model_status {
        crate::app::ModelStatus::Resident(info) if info.fallback.is_some() => {
            ui::lamp(Tone::Warning)
        }
        crate::app::ModelStatus::Resident(_) => ui::lamp(Tone::Success),
        crate::app::ModelStatus::Loading => ui::lamp(Tone::Warning),
        crate::app::ModelStatus::Failed(_) => ui::lamp(Tone::Error),
        crate::app::ModelStatus::NotResident => ui::lamp_off(),
    };
    let device_lamp: Element<'_, Action> = if config.input_device.is_empty() {
        ui::lamp(Tone::Error)
    } else {
        ui::lamp(Tone::Success)
    };
    let hotkey_lamp: Element<'_, Action> = if app.binding.is_some() {
        ui::lamp(Tone::Success)
    } else if app.hotkey_error.is_some() {
        ui::lamp(Tone::Error)
    } else {
        ui::lamp(Tone::Warning)
    };

    let device = if config.input_device == config::INPUT_DEVICE_SYSTEM {
        "System default".to_owned()
    } else {
        config.input_device.clone()
    };
    let hotkey = match &app.binding {
        Some(binding) => format!("{} · {}", config.hotkey, binding.source.label()),
        None => format!("{} · not bound", config.hotkey),
    };

    let fields = row![
        column![
            plate_field(model_lamp, "Model", ui::strong::<Action>(selection.label())),
            plate_field(backend_lamp, "Backend", ui::mono::<Action>(backend_text)),
        ]
        .spacing(metrics::SPACE_SM)
        .width(Length::Fill),
        column![
            plate_field(device_lamp, "Device", ui::mono::<Action>(device)),
            plate_field(hotkey_lamp, "Hotkey", ui::mono::<Action>(hotkey)),
        ]
        .spacing(metrics::SPACE_SM)
        .width(Length::Fill),
    ]
    .spacing(metrics::SPACE_XL);

    let mut plate = column![ui::plate(fields)].spacing(metrics::SPACE_MD);
    if let Some((note, tone)) = note {
        plate = plate.push(ui::banner(note, tone));
    }
    plate.into()
}

/// One plate field: the lamp, the key locked in the label column, the value.
fn plate_field<'a>(
    lamp: Element<'a, Action>,
    label: &str,
    value: Element<'a, Action>,
) -> Element<'a, Action> {
    row![
        lamp,
        container(ui::key::<Action>(label)).width(Length::Fixed(metrics::FIELD_LABEL_WIDTH)),
        value,
    ]
    .spacing(metrics::SPACE_SM)
    .align_y(iced::Alignment::Center)
    .into()
}

/// One option of the Microphone pick list (FR-004/FR-006): the value the config stores when it is
/// picked, plus the parts of the one line it renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDeviceRow {
    /// `system` or a backend device identifier — exactly what [`Action::SelectInputDevice`] carries.
    pub id: String,
    /// The device name, or the identifier itself when the device is unknown.
    pub label: String,
    /// The technical identifier; `None` when the label already is it.
    pub identifier: Option<String>,
    /// A note such as [`CURRENTLY_UNAVAILABLE`]; `None` for a usable device.
    pub note: Option<&'static str>,
}

impl fmt::Display for InputDeviceRow {
    /// The option as the pick list renders it — both in the closed field and in the open menu:
    /// the label, then the identifier when it says more than the label, then the note.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label)?;
        if let Some(identifier) = &self.identifier {
            write!(f, "{SEPARATOR}{identifier}")?;
        }
        if let Some(note) = self.note {
            write!(f, "{SEPARATOR}{note}")?;
        }
        Ok(())
    }
}

/// The options the Microphone pick list offers: "System default" first (FR-004), then every device
/// that is openable right now, then — while the configured value is not among them — the
/// configured value itself, marked [`CURRENTLY_UNAVAILABLE`] (FR-006). `devices` is `None` while
/// the enumeration runs; the configured value is never substituted by a different entry.
pub fn input_device_rows(
    config: &Config,
    devices: Option<&[audio::InputDevice]>,
) -> Vec<InputDeviceRow> {
    let mut rows = vec![InputDeviceRow {
        id: config::INPUT_DEVICE_SYSTEM.to_owned(),
        label: "System default".to_owned(),
        identifier: None,
        note: None,
    }];

    let Some(devices) = devices else {
        return rows;
    };

    let mut configured_is_selectable = config.input_device == config::INPUT_DEVICE_SYSTEM;
    for device in devices {
        if device.id == config.input_device {
            configured_is_selectable = true;
        }
        rows.push(InputDeviceRow {
            id: device.id.clone(),
            label: device.name.clone(),
            identifier: Some(device.id.clone()),
            note: None,
        });
    }
    if !configured_is_selectable {
        let label = if config.input_device.is_empty() {
            EMPTY_DEVICE_LABEL.to_owned()
        } else {
            config.input_device.clone()
        };
        rows.push(InputDeviceRow {
            id: config.input_device.clone(),
            label,
            identifier: None,
            note: Some(CURRENTLY_UNAVAILABLE),
        });
    }
    rows
}

/// The Microphone section (FR-004/FR-006): one pick list over the offered devices, and picking
/// one autosaves it (FR-007). The list is the whole choice — the section has no second control —
/// so the card carries the picker alone, plus the two notes that can accompany it.
fn microphone_section(app: &App) -> Element<'static, Action> {
    let config = app.cfg.snapshot();
    let rows = input_device_rows(&config, app.settings.input_devices.as_deref());
    let selected = rows
        .iter()
        .find(|row| row.id == config.input_device)
        .cloned();

    let picker = pick_list(rows, selected, |row| Action::SelectInputDevice(row.id))
        .width(Length::Fill)
        .padding(metrics::BUTTON_PADDING)
        .text_size(metrics::TEXT_BODY)
        .style(style::pick_list)
        .menu_style(style::pick_list_menu);

    let mut entries = column![picker].spacing(metrics::SPACE_SM);
    if app.settings.input_devices.is_none() {
        entries = entries.push(ui::banner(SCANNING_INPUT_DEVICES, Tone::Info));
    }
    if let Some(status) = app.settings.status_for(ui::SettingsSection::Microphone) {
        entries = entries.push(ui::banner(status.text.clone(), status.tone));
    }

    ui::card(entries)
}

/// Heading of the idle-unload section and caption of its card (FR-006).
const IDLE_UNLOAD_LABEL: &str = "Unload idle model";
/// The card's one explanation line.
const IDLE_UNLOAD_LEAD: &str =
    "Frees the model's memory after this much idling — the next dictation loads it again.";

/// The idle-unload timeout (FR-006/FR-007): one radio per preset, autosaved.
fn idle_unload_card(app: &App) -> Element<'_, Action> {
    let config = app.cfg.snapshot();
    let mut options = column![].spacing(metrics::SPACE_SM);
    for value in IdleUnload::ALL {
        options = options.push(
            radio(
                value.display_name(),
                value,
                Some(config.idle_unload),
                Action::SelectIdleUnload,
            )
            .style(style::radio)
            .text_size(metrics::TEXT_BODY)
            .spacing(metrics::SPACE_SM),
        );
    }

    ui::card(column![options, ui::lead::<Action>(IDLE_UNLOAD_LEAD),].spacing(metrics::SPACE_MD))
}

/// FR-005: the language filter, the sorting and the text search.
fn controls(app: &App) -> Element<'_, Action> {
    let list = &app.settings.list;

    let search = text_input("Search models…", &list.search)
        .on_input(Action::SetSearch)
        .style(style::text_input)
        .padding(metrics::SPACE_SM)
        .size(metrics::TEXT_BODY);

    // FR-005: the filter offers the languages the catalog can serve, not a
    // hand-picked pair — the list below is exactly those languages.
    let language_picker = pick_list(
        LanguageFilter::options(),
        Some(list.language),
        Action::SetLanguageFilter,
    )
    .width(Length::Fill)
    .padding(metrics::BUTTON_PADDING)
    .text_size(metrics::TEXT_BODY)
    .style(style::pick_list)
    .menu_style(style::pick_list_menu);

    let sort_chips =
        SortOrder::ALL
            .into_iter()
            .fold(row![].spacing(metrics::SPACE_SM), |chips, order| {
                chips.push(chip(order.label(), order == list.sort, move |_| {
                    Action::SetSort(order)
                }))
            });

    // SC-008: only one download runs at a time; the row says which one.
    let running: Option<Element<'_, Action>> = app
        .download
        .as_ref()
        .filter(|state| state.is_running())
        .map(|state| {
            ui::muted(format!(
                "Downloading {} — {:.0} %",
                state.selection.label(),
                state.percent()
            ))
        });

    // The text filter sits closest to the list it narrows: language and sorting
    // first, then the search over what is left (FR-005).
    let mut row = column![
        controls_row("Language", language_picker),
        controls_row("Sort by", sort_chips),
        controls_row("Search", search),
    ]
    .spacing(metrics::SPACE_SM);

    if let Some(running) = running {
        row = row.push(running);
    }

    row.into()
}

fn controls_row<'a>(name: &'a str, control: impl Into<Element<'a, Action>>) -> Element<'a, Action> {
    row![ui::key::<Action>(name), control.into()]
        .spacing(metrics::SPACE_MD)
        .align_y(iced::Alignment::Center)
        .into()
}

/// A filter/sort/quant chip: accent when it is the active value.
fn chip<'a>(
    label: &'a str,
    selected: bool,
    on_press: impl Fn(()) -> Action + 'a,
) -> Element<'a, Action> {
    let chip = button(ui::stamp(label)).padding(metrics::BUTTON_PADDING);
    let chip = if selected {
        chip.style(style::primary_button)
    } else {
        chip.style(style::secondary_button)
    };
    chip.on_press(on_press(())).into()
}

/// The row list, or the reason it is empty (SC-005).
fn list<'a>(app: &'a App, visible: &[Selection], language: Language) -> Element<'a, Action> {
    let mut rows = column![].spacing(metrics::SPACE_SM);
    if visible.is_empty() {
        return rows
            .push(ui::banner("No model matches the filters.", Tone::Info))
            .into();
    }

    let best_wer = visible
        .iter()
        .filter_map(|selection| {
            model_catalog::quality(*selection, language).map(|cell| cell.err_pct)
        })
        .fold(None, |best: Option<f32>, value| {
            Some(best.map_or(value, |best| best.min(value)))
        });
    let best_xrt = visible
        .iter()
        .filter_map(|selection| model_catalog::speed(*selection).map(|row| row.xrt))
        .fold(None, |best: Option<f32>, value| {
            Some(best.map_or(value, |best| best.max(value)))
        });

    for selection in ordered_rows(app.model, visible) {
        rows = rows.push(field_row(app, selection, language, best_wer, best_xrt));
    }
    rows.into()
}

/// Display order of the rows (spec `model-list-field-rows` FR-002): the active
/// model leads and is never listed twice; a filter that hides it hides it here
/// too — the pin never overrides the filters (FR-010).
fn ordered_rows(active: Selection, visible: &[Selection]) -> Vec<Selection> {
    let id = active.artifact.id;
    visible
        .iter()
        .find(|selection| selection.artifact.id == id)
        .into_iter()
        .chain(
            visible
                .iter()
                .filter(|selection| selection.artifact.id != id),
        )
        .copied()
        .collect()
}

/// One variant as a two-line field row (spec `model-list-field-rows` FR-001).
///
/// Line 1 is identity and state — radio, label, size, the state tag and the one
/// control the state allows; line 2 the facts and the detail trigger — the
/// metadata line, the offered quants, the quality/speed comparison, the language
/// marker and `Details`. Line 2 wraps rather than clipping, so the minimum
/// window keeps every fact and every control reachable (FR-008).
fn field_row(
    app: &App,
    selection: Selection,
    language: Language,
    best_wer: Option<f32>,
    best_xrt: Option<f32>,
) -> Element<'_, Action> {
    let variant = selection.variant;
    let artifact = selection.artifact;
    let id = artifact.id;
    let is_active = app.model.artifact.id == id;
    let expanded = app.settings.list.expanded == Some(variant.id);
    let verified = is_active && matches!(app.model_status, crate::app::ModelStatus::Resident(_));
    let busy = ui::download_busy_elsewhere(app, id);
    // `data`, not `mono`: the size is a value in a row of values and must never
    // take the free width and wrap its own digits (FR-008).
    let size = || ui::data::<Action>(models::human_size(artifact.size_bytes));

    // The state's faces (FR-004): while a transfer runs, its progress takes the
    // size's place so line 2 keeps every static fact. The tags are the row's
    // short forms; the sentences live in the model card and in onboarding.
    let state = ui::model_state(app, selection, verified);
    let (lamp, mark, label, primary) = state_faces(&state);
    let message = match &state {
        ui::ModelState::Downloading { .. } | ui::ModelState::Verifying => {
            Some(Action::CancelDownload)
        }
        ui::ModelState::Failed(_) => (!busy).then_some(Action::RetryDownload(id)),
        _ => (!busy).then_some(Action::Download(id)),
    };
    let state_fact: Element<'_, Action> = match &state {
        ui::ModelState::Downloading { percent } => row![
            progress_bar(0.0..=100.0, *percent)
                .length(Length::Fixed(TRANSFER_BAR_WIDTH))
                .style(style::progress),
            ui::mono::<Action>(format!("{percent:.0} %")),
        ]
        .spacing(metrics::SPACE_SM)
        .align_y(iced::Alignment::Center)
        .into(),
        _ => size(),
    };
    let failure = match &state {
        ui::ModelState::Failed(error) => Some(error.clone()),
        _ => None,
    };
    // The control is an icon-only button (FR-012): the row's state tag states the
    // state, the mark carries the action, the tooltip keeps the word.
    let control = ui_icons::icon_button(
        mark,
        if primary {
            palette::ACCENT
        } else {
            palette::TEXT
        },
        message,
        label,
        if primary {
            style::primary_button
        } else {
            style::secondary_button
        },
    );

    // The label takes the width the state's facts leave and clips inside it, so
    // the size, the tag and the control keep the row's right edge and its padding
    // at every window width (FR-008).
    let identity = row![
        container(
            radio(
                variant.label,
                id,
                Some(app.model.artifact.id),
                Action::SelectModel,
            )
            .style(style::radio)
            .text_size(metrics::TEXT_LEAD)
            .spacing(metrics::SPACE_SM),
        )
        .width(Length::Fill)
        .clip(true),
        state_fact,
        state_lamp(lamp),
        control,
    ]
    .spacing(metrics::SPACE_SM)
    .align_y(iced::Alignment::Center);

    let mut quants = row![].spacing(metrics::SPACE_XS);
    for candidate in variant.downloads {
        let selected = candidate.quant == artifact.quant;
        let quant = candidate.quant;
        quants = quants.push(
            button(ui::stamp(quant))
                .padding(CHIP_PADDING)
                .style(if selected {
                    style::primary_button
                } else {
                    style::secondary_button
                })
                .on_press(Action::SelectQuant {
                    variant: variant.id,
                    quant,
                }),
        );
    }

    let mut facts = row![ui::muted::<Action>(meta_line(variant)), quants]
        .spacing(metrics::SPACE_MD)
        .align_y(iced::Alignment::Center)
        .width(Length::Fill);
    if let Some(marker) = warning_marker(variant, language) {
        facts = facts.push(ui::state_tag::<Action>(marker, Tone::Warning));
    }

    // The fact line takes the width the detail trigger leaves and wraps inside
    // it, so the trigger keeps the row's right edge — under the state's control
    // on line 1 — at every window width (FR-008). The indicators hold their own
    // slot between the wrapping facts and the trigger: anchored to the row's
    // right edge, the Quality and Speed bars stand on one axis across every row,
    // whatever the language summary and the quants before them measure.
    let facts_line = row![
        facts.wrap().vertical_spacing(metrics::SPACE_SM),
        indicators(selection, language, best_wer, best_xrt),
        ui_icons::icon_button(
            if expanded {
                Icon::ChevronUp
            } else {
                Icon::ChevronDown
            },
            palette::TEXT,
            Some(Action::ToggleDetail(variant.id)),
            if expanded {
                "Hide details"
            } else {
                "Show details"
            },
            style::secondary_button,
        ),
    ]
    .spacing(metrics::SPACE_MD)
    .align_y(iced::Alignment::Start);

    // The two lines sit tight: with the lifted type the row still has to keep the
    // list short (spec `model-list-field-rows` SC-001).
    let mut body = column![identity, facts_line].spacing(metrics::SPACE_XS);

    if let Some(error) = failure {
        body = body.push(ui::banner(error, Tone::Error));
        body = body.push(ui::muted::<Action>(
            "The partial download is kept and resumed.",
        ));
    }

    if expanded {
        if let Some(warning) = model_catalog::language_warning(variant, language) {
            body = body.push(ui::banner(warning, Tone::Warning));
        }
        body = body.push(detail(selection));
    }

    if is_active {
        ui::field_row_active(body)
    } else {
        ui::field_row(body)
    }
}

/// The row's state, as the row states it (spec FR-004/FR-012): the lamp's tone
/// (`None` = the hollow ring, the world's designed-off lamp), the mark and the
/// word of the control the state allows, and whether that control is the primary
/// one. One table, so every state reads the same way and a new state cannot slip
/// in unmarked — the state is the lamp and the mark, its word is the control's
/// tooltip.
fn state_faces(state: &ui::ModelState) -> (Option<Tone>, Icon, &'static str, bool) {
    match state {
        ui::ModelState::Downloading { .. } => (Some(Tone::Info), Icon::Cross, "Cancel", true),
        ui::ModelState::Verifying => (Some(Tone::Info), Icon::Cross, "Cancel", true),
        ui::ModelState::Failed(_) => (Some(Tone::Error), Icon::Retry, "Retry", false),
        ui::ModelState::Verified => (Some(Tone::Success), Icon::Check, "Verify", false),
        // On disk, not checked yet: the designed-off lamp, never a dimmed dot.
        ui::ModelState::OnDisk => (None, Icon::Check, "Verify", false),
        ui::ModelState::NotDownloaded => (Some(Tone::Warning), Icon::Download, "Download", true),
    }
}

/// The lamp of a state: solid in its tone, or the hollow ring of "designed off".
fn state_lamp<M: 'static>(tone: Option<Tone>) -> Element<'static, M> {
    match tone {
        Some(tone) => ui::lamp(tone),
        None => ui::lamp_off(),
    }
}

/// The metadata line of a row (spec FR-009): the language summary only. The
/// parameter count and the license are not scan fields — they live in the detail
/// block with the rest of the catalog record.
fn meta_line(variant: &model_catalog::Variant) -> String {
    model_catalog::languages_label(variant)
}

/// The row's compact language warning (spec FR-005): the fact alone, so it fits
/// the row; the sentence it abbreviates lives in the expanded detail.
fn warning_marker(variant: &model_catalog::Variant, language: Language) -> Option<String> {
    model_catalog::language_warning(variant, language).map(|_| format!("No {}", language.label()))
}

/// FR-004: the quality and speed indicators — a caption next to a thin, quiet
/// bar scaled to the best listed model.
///
/// The bars are the comparison the list is read for; the exact values, the
/// datasets they come from, the reference machine and the backend are in the
/// card's detail. A quant the catalog has no published number for shows
/// [`NO_MEASUREMENT`] instead of a bar, so "unmeasured" is never read as "worst"
/// (SC-007).
fn indicators(
    selection: Selection,
    language: Language,
    best_wer: Option<f32>,
    best_xrt: Option<f32>,
) -> Element<'static, Action> {
    let quality_fraction = indicator_fraction(
        model_catalog::quality(selection, language).map(|cell| cell.err_pct),
        best_wer,
        // Quality is the error rate: the smallest one is the best.
        true,
    );
    let speed_fraction = indicator_fraction(
        model_catalog::speed(selection).map(|row| row.xrt),
        best_xrt,
        false,
    );

    row![
        indicator("Quality", quality_fraction),
        indicator("Speed", speed_fraction),
    ]
    .spacing(metrics::SPACE_XL)
    .into()
}

/// The fill of one indicator bar: the measurement relative to the best listed
/// value on the same axis, or `None` when either side has no published number —
/// an unmeasured bar must not read as a zero one (SC-007). `lower_is_better`
/// separates the axes: the error rate is compared by its smallest value, the
/// throughput by its largest.
fn indicator_fraction(
    measured: Option<f32>,
    best: Option<f32>,
    lower_is_better: bool,
) -> Option<f32> {
    let (measured, best) = (measured?, best?);

    Some(if lower_is_better {
        if measured > 0.0 { best / measured } else { 1.0 }
    } else if best > 0.0 {
        measured / best
    } else {
        1.0
    })
}

/// One indicator: the caption next to its bar, or the dash of an unmeasured
/// quant — the caption keeps both cases on the same line and in the same column.
fn indicator(caption: &'static str, fraction: Option<f32>) -> Element<'static, Action> {
    let mark: Element<'static, Action> = match fraction {
        Some(fraction) => progress_bar(0.0..=1.0, fraction)
            .length(Length::Fixed(INDICATOR_BAR_WIDTH))
            .girth(Length::Fixed(INDICATOR_BAR_HEIGHT))
            .style(style::progress_subtle)
            .into(),
        None => container(ui::muted::<Action>(NO_MEASUREMENT))
            .width(Length::Fixed(INDICATOR_BAR_WIDTH))
            .into(),
    };

    row![ui::key::<Action>(caption), mark]
        .spacing(metrics::SPACE_SM)
        .align_y(iced::Alignment::Center)
        .into()
}

/// The expanded detail: the exact published values (FR-004/SC-001), plus the
/// variant note the compact card leaves out.
fn detail(selection: Selection) -> Element<'static, Action> {
    let variant = selection.variant;
    let artifact = selection.artifact;

    let mut rows = column![
        ui::lead::<Action>(variant.note),
        ui::key::<Action>("Accuracy"),
        // The catalog record's headline cell: the measurement other tools quote.
        ui::muted::<Action>(format!(
            "headline: {} {} · {}",
            variant.headline.0, variant.headline.1, variant.headline.2,
        )),
    ]
    .spacing(metrics::SPACE_XS);

    for accuracy in variant.accuracy {
        rows = rows.push(ui::data::<Action>(format!(
            "{} {} · {} · {} · {:.2} % {}",
            accuracy.dataset,
            accuracy.split,
            accuracy.language,
            accuracy.quant,
            accuracy.err_pct,
            accuracy.metric,
        )));
    }

    rows = rows.push(ui::key::<Action>("Speed"));
    for speed in variant.speed {
        rows = rows.push(ui::data::<Action>(format!(
            "{} · {} · {:.2}× realtime",
            backend_label(speed.backend),
            speed.quant,
            speed.xrt,
        )));
    }
    rows = rows.push(ui::muted::<Action>(format!(
        "reference machine: {}",
        model_catalog::SPEED_MACHINE_LABEL
    )));

    rows = rows.push(ui::strong::<Action>("File"));
    rows = rows.push(ui::data::<Action>(format!(
        "File {} · {} bytes",
        artifact.file_name, artifact.size_bytes
    )));
    rows = rows.push(ui::mono::<Action>(artifact.sha256));
    rows = rows.push(ui::mono::<Action>(artifact.url));
    rows = rows.push(ui::data::<Action>(format!(
        "Languages: {}",
        variant.languages.join(", ")
    )));
    rows = rows.push(ui::data::<Action>(format!(
        "Params: {}",
        model_catalog::params_label(variant.params)
    )));
    rows = rows.push(ui::data::<Action>(format!("License: {}", variant.license)));
    rows = rows.push(ui::muted::<Action>(format!("Family: {}", variant.family)));
    rows = rows.push(ui::muted::<Action>(format!(
        "Snapshot: {}",
        model_catalog::SNAPSHOT_VERSION
    )));

    ui::card(rows)
}

fn backend_label(backend: &str) -> &'static str {
    match backend {
        "vulkan" => "Vulkan",
        "cpu" => "CPU",
        _ => "unknown backend",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The active selection every test starts from: the unchanged default.
    fn active() -> Selection {
        model_catalog::default_selection()
    }

    /// The variant ids a filtered/sorted list shows, in display order.
    fn ids(list: &ListState, language: Language) -> Vec<&'static str> {
        list.visible(active(), language)
            .iter()
            .map(|selection| selection.variant.id)
            .collect()
    }

    fn variant(id: &str) -> &'static Variant {
        model_catalog::VARIANTS
            .iter()
            .find(|candidate| candidate.id == id)
            .expect("the snapshot offers this variant")
    }

    #[test]
    fn a_language_filter_keeps_only_the_variants_that_serve_it() {
        let list = ListState {
            language: LanguageFilter::only(Language::new("de-DE")),
            ..ListState::default()
        };

        assert_eq!(
            ids(&list, Language::new("de-DE")),
            [
                "nemotron-3.5-asr-streaming-0.6b",
                "voxtral-mini-4b-realtime-2602"
            ]
        );
        // The filter keeps a variant whose advertised spelling differs from the
        // configured locale (`de` vs `de-DE`) and drops the English-only ones.
        for selection in list.visible(active(), Language::new("de-DE")) {
            assert!(model_catalog::supports(
                selection.variant,
                Language::new("de-DE")
            ));
        }

        // A language only one variant serves keeps exactly that one.
        let vietnamese = ListState {
            language: LanguageFilter::only(Language::new("vi-VN")),
            ..ListState::default()
        };
        assert_eq!(
            ids(&vietnamese, Language::new("vi-VN")),
            ["nemotron-3.5-asr-streaming-0.6b"]
        );
    }

    #[test]
    fn the_all_and_english_filters_pick_their_variants() {
        assert_eq!(ids(&ListState::default(), Language::AUTO).len(), 8);
        let list = ListState {
            language: LanguageFilter::only(Language::new("en-US")),
            ..ListState::default()
        };
        // Every offered variant serves English, so the filter cannot narrow it.
        assert_eq!(ids(&list, Language::new("en-US")).len(), 8);
    }

    #[test]
    fn the_language_filter_offers_every_language_without_auto() {
        let options = LanguageFilter::options();
        // All languages first, then one entry per table language, same order.
        assert_eq!(options.len(), Language::ALL.len());
        assert_eq!(options[0], LanguageFilter::ALL);
        assert_eq!(options[0].to_string(), "All languages");
        assert_eq!(options[1].to_string(), Language::ALL[1].display_name());
        assert_eq!(
            options.last().map(ToString::to_string),
            Some(
                Language::ALL[Language::ALL.len() - 1]
                    .display_name()
                    .to_owned()
            )
        );
        assert!(
            options[1..].iter().all(|filter| matches!(
                filter.0,
                Some(language) if !language.is_auto()
            )),
            "auto is not a language to filter by"
        );
    }

    #[test]
    fn search_matches_label_id_and_family() {
        let search = |query: &str| {
            ids(
                &ListState {
                    search: query.to_owned(),
                    ..ListState::default()
                },
                Language::AUTO,
            )
        };

        // Label match ("Moonshine Streaming …").
        assert_eq!(search("moonshine").len(), 3);
        // Family match: four variants share the `parakeet` family.
        assert_eq!(search("parakeet").len(), 4);
        // Id match, case-insensitively and ignoring surrounding blanks.
        assert_eq!(search("  3.5-ASR  "), ["nemotron-3.5-asr-streaming-0.6b"]);
        // No match: the list renders empty instead of showing everything.
        assert!(search("whisper").is_empty());
        assert_eq!(search("").len(), 8);
    }

    #[test]
    fn sorting_by_size_is_ascending_and_ties_by_name() {
        let list = ListState {
            sort: SortOrder::Size,
            ..ListState::default()
        };
        let listed = list.visible(active(), Language::AUTO);

        assert_eq!(listed[0].artifact.size_bytes, 50_462_816);
        assert_eq!(listed[0].variant.id, "moonshine-streaming-tiny");
        assert_eq!(
            listed.last().map(|selection| selection.variant.id),
            Some("voxtral-mini-4b-realtime-2602")
        );
        assert!(
            listed
                .windows(2)
                .all(|pair| { pair[0].artifact.size_bytes <= pair[1].artifact.size_bytes })
        );
    }

    #[test]
    fn sorting_by_speed_puts_unrated_quants_last() {
        // Only Q8_0 and Q4_K_M have published speed rows, so switching two
        // cards to an unmeasured quant makes them the unrated tail (FR-004).
        let mut list = ListState {
            sort: SortOrder::Speed,
            ..ListState::default()
        };
        // Neither variant may be the active one: an active card always shows the
        // artifact that runs.
        assert_ne!(
            active().variant.id,
            "multitalker-parakeet-streaming-0.6b-v1"
        );
        list.set_quant("multitalker-parakeet-streaming-0.6b-v1", "Q6_K");
        list.set_quant("parakeet-unified-en-0.6b", "Q5_K_M");

        let listed = list.visible(active(), Language::AUTO);
        assert_eq!(listed[0].variant.id, "moonshine-streaming-tiny");
        assert_eq!(listed[0].artifact.quant, "Q8_0");
        assert!(model_catalog::speed(listed[0]).is_some_and(|row| row.xrt > 38.0));

        let unrated = &listed[listed.len() - 2..];
        assert_eq!(
            unrated
                .iter()
                .map(|selection| selection.variant.id)
                .collect::<Vec<_>>(),
            [
                "multitalker-parakeet-streaming-0.6b-v1",
                "parakeet-unified-en-0.6b"
            ],
            "unrated cards sort last, tied by name"
        );
        assert!(
            unrated
                .iter()
                .all(|selection| model_catalog::speed(*selection).is_none())
        );

        // Throughput is descending among the rated ones.
        let rated: Vec<f32> = listed
            .iter()
            .filter_map(|selection| model_catalog::speed(*selection).map(|row| row.xrt))
            .collect();
        assert!(rated.len() == 6, "{rated:?}");
        assert!(rated.windows(2).all(|pair| pair[0] >= pair[1]), "{rated:?}");
    }

    #[test]
    fn sorting_by_quality_uses_the_configured_language_then_the_headline() {
        let list = ListState {
            sort: SortOrder::Quality,
            ..ListState::default()
        };

        // German: Voxtral's published German result (4.24) beats the Nemotron
        // 3.5 German result (10.33), even though the Nemotron leads in English.
        let german = ids(&list, Language::new("de-DE"));
        let rank = |id: &str| german.iter().position(|candidate| *candidate == id);
        assert!(rank("voxtral-mini-4b-realtime-2602") < rank("nemotron-3.5-asr-streaming-0.6b"));

        // English: the same two models swap places (Nemotron 3.05 vs Voxtral 2.07
        // minus the quant rows); the best English WER leads either way.
        let english = list.visible(active(), Language::new("en-US"));
        assert_eq!(english[0].variant.id, "parakeet-unified-en-0.6b");
        assert!(english.windows(2).all(|pair| {
            model_catalog::quality(pair[0], Language::new("en-US")).map(|cell| cell.err_pct)
                <= model_catalog::quality(pair[1], Language::new("en-US")).map(|cell| cell.err_pct)
        }));

        // A locale without published cells ranks by its language's cells, exactly
        // like the hint falls back to the bare language.
        assert_eq!(
            ids(&list, Language::new("en-GB")),
            ids(&list, Language::new("en-US"))
        );

        // Auto-detect sorts by the headline cell (fleurs for Nemotron 3.5).
        let auto = list.visible(active(), Language::AUTO);
        assert_eq!(auto[0].variant.id, "parakeet-unified-en-0.6b");
        assert_eq!(auto.len(), 8);
    }

    #[test]
    fn quant_choice_defaults_to_the_recommended_then_the_override() {
        let active = active();
        let mut list = ListState::default();
        let parakeet = variant("parakeet-unified-en-0.6b");
        // Parakeet Unified's card preselects the catalog's recommendation for it
        // (Q4_K_M), not the Q8_0 of the shipped default model.
        // The Moonshine variants ship a single quant, so their card is a fixed
        // Q8_0 whatever the overrides say.
        let moonshine = variant("moonshine-streaming-medium");

        assert_eq!(list.quant_for(parakeet, active).quant, "Q4_K_M");
        // The CPU row carries the same quant today (see `model_catalog`).
        assert_eq!(list.quant_for(parakeet, active).quant, "Q4_K_M");
        list.set_quant(parakeet.id, "Q4_K_M");
        assert_eq!(list.quant_for(parakeet, active).quant, "Q4_K_M");
        // One entry per variant, replaced rather than appended.
        assert_eq!(list.quant_overrides.len(), 1);
        list.set_quant(parakeet.id, "Q6_K");
        assert_eq!(list.quant_overrides.len(), 1);
        assert_eq!(list.quant_for(parakeet, active).quant, "Q6_K");

        // A quant the variant does not publish falls back to the recommended one
        // instead of rendering a card without a file.
        list.set_quant(parakeet.id, "F32");
        assert_eq!(list.quant_for(parakeet, active).quant, "Q4_K_M");
        list.set_quant(moonshine.id, "Q4_K_M");
        assert_eq!(list.quant_for(moonshine, active).quant, "Q8_0");

        // The active variant always shows the active artifact: the user's own
        // model is what runs, whatever was clicked on other cards.
        list.set_quant(active.variant.id, "Q4_K_M");
        assert_eq!(list.quant_for(active.variant, active), active.artifact);
        assert_eq!(
            list.quant_for(active.variant, active).id,
            active.artifact.id
        );
    }

    #[test]
    fn filters_sort_and_expansion_are_independent() {
        let list = ListState {
            expanded: Some("voxtral-mini-4b-realtime-2602"),
            search: "voxtral".to_owned(),
            ..ListState::default()
        };

        // Expanding a card does not change what is listed, and filtering does
        // not close it.
        assert_eq!(
            ids(&list, Language::AUTO),
            ["voxtral-mini-4b-realtime-2602"]
        );
        assert_eq!(list.expanded, Some("voxtral-mini-4b-realtime-2602"));
    }

    #[test]
    fn empty_result_is_representable() {
        let list = ListState {
            search: "nothing-matches-this".to_owned(),
            ..ListState::default()
        };
        assert!(list.visible(active(), Language::AUTO).is_empty());
    }

    #[test]
    fn every_download_state_has_its_face_in_the_row() {
        let faces = [
            (
                ui::ModelState::NotDownloaded,
                (Some(Tone::Warning), Icon::Download, "Download", true),
            ),
            (ui::ModelState::OnDisk, (None, Icon::Check, "Verify", false)),
            (
                ui::ModelState::Verified,
                (Some(Tone::Success), Icon::Check, "Verify", false),
            ),
            (
                ui::ModelState::Failed("sha mismatch".to_owned()),
                (Some(Tone::Error), Icon::Retry, "Retry", false),
            ),
            (
                ui::ModelState::Verifying,
                (Some(Tone::Info), Icon::Cross, "Cancel", true),
            ),
            (
                ui::ModelState::Downloading { percent: 42.0 },
                (Some(Tone::Info), Icon::Cross, "Cancel", true),
            ),
        ];

        for (state, expected) in faces {
            assert_eq!(state_faces(&state), expected, "{state:?}");
        }
    }

    #[test]
    fn the_active_model_leads_the_rows_and_is_not_repeated() {
        // The default selection sorts into the middle of the name-sorted list.
        let visible = ListState::default().visible(active(), Language::AUTO);
        let ordered = ordered_rows(active(), &visible);
        let ids = |rows: &[Selection]| {
            rows.iter()
                .map(|selection| selection.artifact.id)
                .collect::<Vec<_>>()
        };

        assert_eq!(ordered.len(), visible.len());
        assert_eq!(ordered[0].artifact.id, active().artifact.id);
        assert_eq!(
            ids(&ordered[1..]),
            ids(&visible)
                .into_iter()
                .filter(|id| *id != active().artifact.id)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn rows_without_the_active_model_keep_their_order() {
        // A filter that hides the active model hides it here too: the pin never
        // overrides the filters (FR-010).
        let visible: Vec<Selection> = ListState::default()
            .visible(active(), Language::AUTO)
            .into_iter()
            .filter(|selection| selection.artifact.id != active().artifact.id)
            .collect();
        let ordered = ordered_rows(active(), &visible);

        assert_eq!(ordered.len(), visible.len());
        assert!(
            ordered
                .iter()
                .zip(&visible)
                .all(|(row, visible)| row.artifact.id == visible.artifact.id)
        );
    }

    #[test]
    fn the_meta_line_is_only_the_language_summary() {
        let selection =
            model_catalog::artifact("moonshine-streaming-medium-Q8_0").expect("offered");
        assert_eq!(meta_line(selection.variant), "1 language");
        // Params and license are detail facts, not scan fields: they must not
        // leak back into the row (spec FR-009).
        let line = meta_line(selection.variant);
        assert!(!line.contains(&model_catalog::params_label(selection.variant.params)));
        assert!(!line.contains(selection.variant.license));
    }

    #[test]
    fn a_language_the_variant_cannot_serve_gets_a_compact_marker() {
        let moonshine =
            model_catalog::artifact("moonshine-streaming-medium-Q8_0").expect("offered");
        assert_eq!(
            warning_marker(moonshine.variant, Language::new("de-DE")),
            Some("No de-DE".to_owned())
        );
        assert_eq!(
            warning_marker(moonshine.variant, Language::new("en-US")),
            None
        );

        // Nemotron 3.5 serves every language the table offers.
        let nemotron =
            model_catalog::artifact("nemotron-3.5-asr-streaming-0.6b-Q8_0").expect("offered");
        for language in Language::ALL {
            assert_eq!(warning_marker(nemotron.variant, language), None);
        }
    }

    #[test]
    fn filter_and_sort_labels_are_frozen() {
        assert_eq!(LanguageFilter::ALL.to_string(), "All languages");
        assert_eq!(
            LanguageFilter::only(Language::new("de-DE")).to_string(),
            "German"
        );
        assert_eq!(SortOrder::default(), SortOrder::Name);
        assert_eq!(LanguageFilter::default(), LanguageFilter::ALL);
        assert_eq!(SortOrder::ALL.map(SortOrder::label).len(), 4);
    }

    fn device(id: &str, name: &str) -> audio::InputDevice {
        audio::InputDevice {
            id: id.to_owned(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn system_default_row_leads_the_list() {
        let devices = [
            device("alsa:pipewire", "PipeWire"),
            device("alsa:usb", "USB Mic"),
        ];
        let rows = input_device_rows(&Config::default(), Some(&devices));

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, config::INPUT_DEVICE_SYSTEM);
        assert_eq!(rows[0].label, "System default");
        assert_eq!(rows[0].identifier, None);
        assert_eq!(rows[0].note, None);
        for (row, device) in rows[1..].iter().zip(&devices) {
            assert_eq!(row.id, device.id);
            assert_eq!(row.identifier, Some(device.id.clone()));
            assert_eq!(row.note, None);
        }
    }

    #[test]
    fn while_the_list_is_loading_only_the_system_default_is_visible() {
        let rows = input_device_rows(&Config::default(), None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, config::INPUT_DEVICE_SYSTEM);
    }

    #[test]
    fn a_configured_device_that_is_listed_is_not_duplicated() {
        let config = Config {
            input_device: "alsa:pipewire".to_owned(),
            ..Config::default()
        };
        let devices = [device("alsa:pipewire", "PipeWire")];
        let rows = input_device_rows(&config, Some(&devices));

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter().filter(|row| row.id == "alsa:pipewire").count(),
            1
        );
        assert_eq!(rows[1].note, None);
    }

    #[test]
    fn a_configured_device_that_is_missing_stays_visible_as_unavailable() {
        let config = Config {
            input_device: "alsa:headset".to_owned(),
            ..Config::default()
        };
        let devices = [device("alsa:pipewire", "PipeWire")];
        let rows = input_device_rows(&config, Some(&devices));

        let last = rows.last().expect("the configured device row");
        assert_eq!(last.id, "alsa:headset");
        assert_eq!(last.label, "alsa:headset");
        assert_eq!(last.note, Some(CURRENTLY_UNAVAILABLE));
        assert_eq!(
            rows.iter().filter(|row| row.id == "alsa:headset").count(),
            1
        );
    }

    #[test]
    fn an_empty_configured_device_gets_a_visible_label() {
        let config = Config {
            input_device: String::new(),
            ..Config::default()
        };
        let none: Vec<audio::InputDevice> = Vec::new();
        let rows = input_device_rows(&config, Some(&none));

        let last = rows.last().expect("the configured device row");
        assert_eq!(last.label, EMPTY_DEVICE_LABEL);
        assert_eq!(last.note, Some(CURRENTLY_UNAVAILABLE));
        assert!(rows[..rows.len() - 1].iter().all(|row| !row.id.is_empty()));
    }

    /// The option line the pick list renders: name, identifier, note, joined by this.
    const OPTION_SEPARATOR: &str = "  ·  ";

    #[test]
    fn one_option_line_carries_name_identifier_and_note() {
        assert_eq!(OPTION_SEPARATOR, super::SEPARATOR);

        let none: Vec<audio::InputDevice> = Vec::new();
        let default_row = &input_device_rows(&Config::default(), Some(&none))[0];
        assert_eq!(
            default_row.to_string(),
            "System default",
            "the system default has no identifier to add"
        );

        let devices = [device("alsa:pipewire", "PipeWire")];
        let listed = &input_device_rows(&Config::default(), Some(&devices))[1];
        assert_eq!(
            listed.to_string(),
            format!("PipeWire{OPTION_SEPARATOR}alsa:pipewire")
        );

        let config = Config {
            input_device: "alsa:headset".to_owned(),
            ..Config::default()
        };
        let rows = input_device_rows(&config, Some(&devices));
        let missing = rows.last().expect("the configured device row");
        assert_eq!(
            missing.to_string(),
            format!("alsa:headset{OPTION_SEPARATOR}{CURRENTLY_UNAVAILABLE}"),
            "an unopenable device keeps its note on the option line"
        );
    }

    #[test]
    fn an_unmeasured_indicator_has_no_bar_and_the_best_value_fills_one() {
        // SC-007: no published number on either side → no bar. A 0.0 bar would
        // read as "worst value in the list".
        assert_eq!(indicator_fraction(None, Some(2.0), true), None);
        assert_eq!(indicator_fraction(Some(2.0), None, true), None);
        assert_eq!(indicator_fraction(None, None, false), None);

        // The error rate scales against the smallest, the throughput against the
        // largest, so the best listed model fills its bar.
        assert_eq!(indicator_fraction(Some(2.0), Some(2.0), true), Some(1.0));
        assert_eq!(indicator_fraction(Some(4.0), Some(2.0), true), Some(0.5));
        assert_eq!(indicator_fraction(Some(8.0), Some(8.0), false), Some(1.0));
        assert_eq!(indicator_fraction(Some(4.0), Some(8.0), false), Some(0.5));

        // A perfect (zero) error rate is measured, not missing.
        assert_eq!(indicator_fraction(Some(0.0), Some(2.0), true), Some(1.0));
    }
}
