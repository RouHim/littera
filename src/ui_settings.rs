//! Settings surface (FR-016/FR-022, Scenario 3).
//!
//! Every change applies immediately — there is no Save button. A change that
//! cannot apply (a failed config write) keeps the previous value and is reported
//! in the pinned footer instead.
//!
//! The model section renders the bundled [`model_catalog`] snapshot: one card per
//! streaming variant with metadata, quality/speed indicators, a quant selector,
//! an expandable detail, filters and sorting (FR-003/FR-004/FR-005). A language
//! mismatch is a warning on the card and never blocks a selection (FR-011).
//!
//! Below the list the section names the compute backend the configured model
//! runs on (FR-005/FR-014): the engine's report once a model is loaded, the
//! startup device probe before that — the probe enumerates the registered
//! devices without loading a model, so "GPU or CPU?" is answered while the first
//! load is still in flight.

use iced::widget::{Space, button, column, progress_bar, radio, row, scrollable, text, text_input};
use iced::{Element, Font, Length, Task};

use crate::app::App;
use crate::config::{self, Language};
use crate::engine::EngineInfo;
use crate::model_catalog::{self, Artifact, Selection, Variant};
use crate::models;
use crate::ui::{self, Message, Tone, metrics, style};

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
    SetMaxSize(SizeFilter),
    SetSort(SortOrder),
    SelectLanguage(Language),
    BindViaPortal,
    StartCapture,
    OpenModelDir,
}

/// Which variants the language filter keeps (FR-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanguageFilter {
    #[default]
    All,
    German,
    English,
}

impl LanguageFilter {
    /// Every option, in the order the chips are rendered.
    pub const ALL: [LanguageFilter; 3] = [Self::All, Self::German, Self::English];

    const fn label(self) -> &'static str {
        match self {
            LanguageFilter::All => "All languages",
            LanguageFilter::German => "German",
            LanguageFilter::English => "English",
        }
    }

    /// Whether a variant is listed under this filter. A German-capable model
    /// also serves English in practice, but the filter answers the question the
    /// config asks: which models can transcribe the configured language.
    fn keeps(self, variant: &Variant) -> bool {
        match self {
            LanguageFilter::All => true,
            LanguageFilter::German => model_catalog::supports(variant, Language::DeDe),
            LanguageFilter::English => model_catalog::supports(variant, Language::EnUs),
        }
    }
}

/// Maximum download size of the card's selected quant (FR-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeFilter {
    #[default]
    All,
    UpTo500Mb,
    UpTo1Gb,
    UpTo3Gb,
}

impl SizeFilter {
    /// Every option, in the order the chips are rendered.
    pub const ALL: [SizeFilter; 4] = [Self::All, Self::UpTo500Mb, Self::UpTo1Gb, Self::UpTo3Gb];

    const fn label(self) -> &'static str {
        match self {
            SizeFilter::All => "Any size",
            SizeFilter::UpTo500Mb => "≤ 500 MB",
            SizeFilter::UpTo1Gb => "≤ 1 GB",
            SizeFilter::UpTo3Gb => "≤ 3 GB",
        }
    }

    /// The largest artifact a card may select to stay listed.
    pub const fn threshold(self) -> Option<u64> {
        match self {
            SizeFilter::All => None,
            SizeFilter::UpTo500Mb => Some(500_000_000),
            SizeFilter::UpTo1Gb => Some(1_000_000_000),
            SizeFilter::UpTo3Gb => Some(3_000_000_000),
        }
    }
}

/// Order of the card list (FR-005).
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

/// The indicator bars are reference marks, not progress: thin and short.
const INDICATOR_BAR_WIDTH: f32 = 120.0;
const INDICATOR_BAR_HEIGHT: f32 = 4.0;

/// Session-only list state: the filters, the sorting, the expanded detail and
/// the quants the user picked on cards that are not active.
#[derive(Debug, Clone, Default)]
pub struct ListState {
    pub search: String,
    pub language: LanguageFilter,
    pub max_size: SizeFilter,
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

    /// The cards to render: filters applied, then sorted (SC-005).
    pub fn visible(&self, active: Selection, language: Language) -> Vec<Selection> {
        let query = self.search.trim().to_lowercase();
        let mut visible: Vec<Selection> = model_catalog::VARIANTS
            .iter()
            .filter(|variant| {
                self.language.keeps(variant)
                    && self
                        .max_size
                        .threshold()
                        .is_none_or(|limit| self.quant_for(variant, active).size_bytes <= limit)
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
        Action::SetMaxSize(filter) => app.settings.list.max_size = filter,
        Action::SetSort(order) => app.settings.list.sort = order,
        Action::SelectLanguage(language) => return select_language(app, language),
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
    model_section = model_section.push(backend_hint(app));

    let mut languages = column![].spacing(metrics::SPACE_MD);
    for language in Language::ALL {
        languages = languages.push(
            radio(
                format!("{}  ({})", language.display_name(), language.label()),
                language,
                Some(config.language),
                Action::SelectLanguage,
            )
            .style(style::radio)
            .text_size(metrics::TEXT_BODY)
            .spacing(metrics::SPACE_SM),
        );
    }

    let mut language_card = column![ui::card(languages)].spacing(metrics::SPACE_MD);
    if let Some(status) = ui.status_for(ui::SettingsSection::Language) {
        language_card = language_card.push(ui::banner(status.text.clone(), status.tone));
    } else if let Some(model) = model_catalog::language_warning(active.variant, config.language) {
        // A hand-edited config can arrive in a mismatched state; the warning
        // stays until one side is changed.
        language_card = language_card.push(ui::banner(model, Tone::Warning));
    }

    let mut details = column![
        row![
            ui::muted::<Action>("Chord"),
            ui::chord(config.hotkey.clone())
        ]
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
            button("Bind…")
                .style(style::primary_button)
                .padding(metrics::BUTTON_PADDING)
                .on_press(Action::BindViaPortal),
            ui::muted::<Action>("The compositor asks for the shortcut in its own dialog."),
        ]
        .spacing(metrics::SPACE_MD)
        .align_y(iced::Alignment::Center)
        .into()
    } else {
        button("Bind…")
            .style(style::primary_button)
            .padding(metrics::BUTTON_PADDING)
            .on_press(Action::StartCapture)
            .into()
    };

    let hotkey_card = ui::card(column![details, bind_action].spacing(metrics::SPACE_MD));

    let mut storage = column![
        column![
            ui::muted::<Action>("Catalog snapshot"),
            ui::mono::<Action>(model_catalog::SNAPSHOT_VERSION),
        ]
        .spacing(metrics::SPACE_XS),
        column![
            ui::muted::<Action>("Models are stored in"),
            button(ui::mono::<Action>(app.model_dir_display()))
                .style(style::link)
                .padding([metrics::SPACE_XS, 0.0])
                .width(Length::Fill)
                .on_press(Action::OpenModelDir),
        ]
        .spacing(metrics::SPACE_XS),
        column![
            ui::muted::<Action>("Space used"),
            ui::mono::<Action>(app.model_dir_usage_display()),
        ]
        .spacing(metrics::SPACE_XS),
        column![
            ui::muted::<Action>("Current model file"),
            ui::mono::<Action>(app.model.artifact.file_name),
        ]
        .spacing(metrics::SPACE_XS),
    ]
    .spacing(metrics::SPACE_MD);

    if let Some(status) = ui.status_for(ui::SettingsSection::Storage) {
        storage = storage.push(ui::banner(status.text.clone(), status.tone));
    }

    column![
        model_section,
        column![ui::heading::<Action>("Language"), language_card].spacing(metrics::SPACE_MD),
        column![ui::heading::<Action>("Hotkey"), hotkey_card].spacing(metrics::SPACE_MD),
        column![ui::heading::<Action>("Storage"), ui::card(storage)].spacing(metrics::SPACE_MD),
    ]
    .spacing(metrics::SPACE_XXL)
    .into()
}

/// Label of the backend hint below the model list (FR-005/FR-014).
const BACKEND_LABEL: &str = "Backend";
/// Marks a backend that only the startup device probe has answered for; the
/// engine has not loaded a model yet, so a fallback may still happen.
const BACKEND_EXPECTED: &str = "expected";
/// Shown while neither the engine nor the device probe has named a backend.
const BACKEND_NOT_LOADED: &str = "not loaded";
/// Prefix of the fallback notice; the reason follows after an em dash (FR-005).
const BACKEND_FALLBACK: &str = "CPU fallback";

/// The backend hint under the model list: which compute backend serves the
/// configured model, and why it is not the GPU when it is not (FR-005, SC-004).
fn backend_hint(app: &App) -> Element<'_, Action> {
    let hint = backend_line(
        app.engine_info.as_ref(),
        app.probe_info.as_ref(),
        app.engine_error.as_deref(),
    );

    let mut card = column![
        column![
            ui::muted::<Action>(BACKEND_LABEL),
            ui::mono::<Action>(hint.text),
        ]
        .spacing(metrics::SPACE_XS)
    ]
    .spacing(metrics::SPACE_MD);

    if let Some((note, tone)) = hint.note {
        card = card.push(ui::banner(note, tone));
    }

    ui::card(card)
}

/// The hint's line and its optional notice.
#[derive(Debug, PartialEq, Eq)]
struct BackendLine {
    text: String,
    note: Option<(String, Tone)>,
}

/// `engine` is the engine's report for the loaded model, `probe` the startup
/// device enumeration, `error` the last engine failure.
///
/// The engine's report wins: only a load knows whether the requested GPU
/// backend held up, and only it carries the fallback reason. The probe answers
/// the same question without touching the model file, so the hint is not blank
/// while the first load is still running.
fn backend_line(
    engine: Option<&EngineInfo>,
    probe: Option<&EngineInfo>,
    error: Option<&str>,
) -> BackendLine {
    if let Some(info) = engine {
        return BackendLine {
            text: format!("{} — {}", info.backend, info.device),
            note: info.fallback.as_deref().map(fallback_note),
        };
    }
    if let Some(info) = probe {
        return BackendLine {
            text: format!("{} ({BACKEND_EXPECTED}) — {}", info.backend, info.device),
            note: info.fallback.as_deref().map(fallback_note),
        };
    }
    BackendLine {
        text: BACKEND_NOT_LOADED.to_owned(),
        note: error.map(|error| (error.to_owned(), Tone::Error)),
    }
}

/// FR-005: a fallback is never silent — this is the reason the GPU path was
/// abandoned, spelled the same way in the engine's notice.
fn fallback_note(reason: &str) -> (String, Tone) {
    (format!("{BACKEND_FALLBACK} — {reason}"), Tone::Warning)
}

/// FR-005: search, language filter, maximum size and sorting.
fn controls(app: &App) -> Element<'_, Action> {
    let list = &app.settings.list;

    let search = text_input("Search models…", &list.search)
        .on_input(Action::SetSearch)
        .style(style::text_input)
        .padding(metrics::SPACE_SM)
        .size(metrics::TEXT_BODY);

    let language_chips =
        LanguageFilter::ALL
            .into_iter()
            .fold(row![].spacing(metrics::SPACE_SM), |chips, filter| {
                chips.push(chip(filter.label(), filter == list.language, move |_| {
                    Action::SetLanguageFilter(filter)
                }))
            });

    let size_chips =
        SizeFilter::ALL
            .into_iter()
            .fold(row![].spacing(metrics::SPACE_SM), |chips, filter| {
                chips.push(chip(filter.label(), filter == list.max_size, move |_| {
                    Action::SetMaxSize(filter)
                }))
            });

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

    let mut row = column![
        search,
        controls_row("Language", language_chips),
        controls_row("Max download size", size_chips),
        controls_row("Sort by", sort_chips),
    ]
    .spacing(metrics::SPACE_SM);

    if let Some(running) = running {
        row = row.push(running);
    }

    row.into()
}

fn controls_row<'a>(name: &'a str, chips: iced::widget::Row<'a, Action>) -> Element<'a, Action> {
    row![ui::muted::<Action>(name), chips]
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
    let chip = button(label).padding(metrics::BUTTON_PADDING);
    let chip = if selected {
        chip.style(style::primary_button)
    } else {
        chip.style(style::secondary_button)
    };
    chip.on_press(on_press(())).into()
}

/// The card list, or the reason it is empty (SC-005).
fn list<'a>(app: &'a App, visible: &[Selection], language: Language) -> Element<'a, Action> {
    let mut cards = column![].spacing(metrics::SPACE_MD);
    if visible.is_empty() {
        return cards
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

    for selection in visible {
        cards = cards.push(card(app, *selection, language, best_wer, best_xrt));
    }
    cards.into()
}

/// One variant card (FR-003/FR-004).
fn card(
    app: &App,
    selection: Selection,
    language: Language,
    best_wer: Option<f32>,
    best_xrt: Option<f32>,
) -> Element<'_, Action> {
    let variant = selection.variant;
    let artifact = selection.artifact;
    let active = app.model;
    let is_active = active.artifact.id == artifact.id;
    let expanded = app.settings.list.expanded == Some(variant.id);

    let header = row![
        radio(
            variant.label,
            artifact.id,
            Some(active.artifact.id),
            Action::SelectModel,
        )
        .style(style::radio)
        .text_size(metrics::TEXT_LEAD)
        .spacing(metrics::SPACE_SM),
        Space::new().width(Length::Fill),
        ui::muted::<Action>("Download size"),
        ui::data(models::human_size(artifact.size_bytes)),
    ]
    .spacing(metrics::SPACE_SM)
    .align_y(iced::Alignment::Center);

    let meta = ui::muted::<Action>(format!(
        "{} · {} params · {}",
        model_catalog::languages_label(variant),
        model_catalog::params_label(variant.params),
        variant.license,
    ));

    let mut quants = row![ui::muted::<Action>("Quant")].spacing(metrics::SPACE_SM);
    for candidate in variant.downloads {
        let selected = candidate.quant == artifact.quant;
        let id = candidate.quant;
        quants = quants.push(
            button(id)
                .padding(metrics::BUTTON_PADDING)
                .style(if selected {
                    style::primary_button
                } else {
                    style::secondary_button
                })
                .on_press(Action::SelectQuant {
                    variant: variant.id,
                    quant: id,
                }),
        );
    }

    let mut body = column![
        header,
        ui::lead(variant.note),
        meta,
        quants,
        indicators(selection, language, best_wer, best_xrt),
        action_panel(app, selection),
    ]
    .spacing(metrics::SPACE_MD);

    body = body.push(
        button(if expanded {
            "Details ▴"
        } else {
            "Details ▾"
        })
        .style(style::secondary_button)
        .padding(metrics::BUTTON_PADDING)
        .on_press(Action::ToggleDetail(variant.id)),
    );

    if let Some(warning) = model_catalog::language_warning(variant, language) {
        body = body.push(ui::banner(warning, Tone::Warning));
    }

    if expanded {
        body = body.push(detail(selection));
    }

    if is_active {
        ui::card_active(body)
    } else {
        ui::card(body)
    }
}

/// FR-004: the quality and speed indicators — one compact value line plus a
/// thin, quiet bar scaled to the best listed model.
///
/// The exact values, the dataset they come from, the reference machine and the
/// backend are in the card's detail, so the card stays scannable.
fn indicators(
    selection: Selection,
    language: Language,
    best_wer: Option<f32>,
    best_xrt: Option<f32>,
) -> Element<'static, Action> {
    let quality = model_catalog::quality(selection, language);
    let quality_fraction = match (quality.map(|cell| cell.err_pct), best_wer) {
        (Some(value), Some(best)) if value > 0.0 => best / value,
        (Some(_), Some(_)) => 1.0,
        _ => 0.0,
    };
    let quality_value = quality.map_or_else(
        || "—".to_owned(),
        |cell| format!("{:.1} % {}", cell.err_pct, cell.metric),
    );

    let speed = model_catalog::speed(selection);
    let speed_fraction = match (speed.map(|row| row.xrt), best_xrt) {
        (Some(value), Some(best)) if best > 0.0 => value / best,
        (Some(_), Some(_)) => 1.0,
        _ => 0.0,
    };
    let speed_value =
        speed.map_or_else(|| "—".to_owned(), |row| format!("{:.1}× realtime", row.xrt));

    let column_of =
        |caption: &'static str, fraction: f32, value: String| -> Element<'static, Action> {
            column![
                row![
                    ui::muted::<Action>(caption),
                    Space::new().width(Length::Fill),
                    text(value)
                        .size(metrics::TEXT_CAPTION)
                        .font(Font::MONOSPACE)
                        .color(ui::palette::TEXT),
                ]
                .align_y(iced::Alignment::Center),
                progress_bar(0.0..=1.0, fraction)
                    .length(Length::Fixed(INDICATOR_BAR_WIDTH))
                    .girth(Length::Fixed(INDICATOR_BAR_HEIGHT))
                    .style(style::progress_subtle),
            ]
            .spacing(metrics::SPACE_XS)
            .width(Length::Fill)
            .into()
        };

    row![
        column_of("Quality", quality_fraction, quality_value),
        column_of("Speed", speed_fraction, speed_value),
    ]
    .spacing(metrics::SPACE_XL)
    .into()
}

/// The download/verify panel of one card; every card but the running one keeps
/// its action disabled while a download runs (FR-006/SC-008).
fn action_panel(app: &App, selection: Selection) -> Element<'static, Action> {
    let verified = app
        .engine_info
        .as_ref()
        .is_some_and(|_| app.model.artifact.id == selection.artifact.id);
    let busy = app.download.as_ref().is_some_and(|state| {
        state.is_running() && state.selection.artifact.id != selection.artifact.id
    });
    let id = selection.artifact.id;

    let panel = ui::model_panel(
        app,
        selection,
        verified,
        Action::Download(id),
        Action::CancelDownload,
        Action::RetryDownload(id),
    );

    if busy {
        return column![panel, ui::muted::<Action>("Another download is running.")].into();
    }
    panel
}

/// The expanded detail: the exact published values (FR-004/SC-001).
fn detail(selection: Selection) -> Element<'static, Action> {
    let variant = selection.variant;
    let artifact = selection.artifact;

    let mut rows = column![
        ui::strong::<Action>("Accuracy"),
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

    rows = rows.push(ui::strong::<Action>("Speed"));
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
    use crate::model_catalog::RECOMMENDED_QUANT;

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
    fn german_filter_keeps_only_the_german_capable_variants() {
        let list = ListState {
            language: LanguageFilter::German,
            ..ListState::default()
        };

        assert_eq!(
            ids(&list, Language::DeDe),
            [
                "nemotron-3.5-asr-streaming-0.6b",
                "voxtral-mini-4b-realtime-2602"
            ]
        );
        // The filter keeps a variant whose advertised spelling differs from the
        // configured locale (`de` vs `de-DE`) and drops the English-only ones.
        for selection in list.visible(active(), Language::DeDe) {
            assert!(model_catalog::supports(selection.variant, Language::DeDe));
        }
    }

    #[test]
    fn language_filter_all_and_english_keep_the_whole_list() {
        assert_eq!(ids(&ListState::default(), Language::Auto).len(), 8);
        let list = ListState {
            language: LanguageFilter::English,
            ..ListState::default()
        };
        // Every offered variant serves English, so the filter cannot narrow it.
        assert_eq!(ids(&list, Language::EnUs).len(), 8);
    }

    #[test]
    fn size_filter_hides_artifacts_above_the_threshold() {
        let one_gb = ListState {
            max_size: SizeFilter::UpTo1Gb,
            ..ListState::default()
        };
        let listed = ids(&one_gb, Language::Auto);
        assert_eq!(listed.len(), 7, "Voxtral's Q8_0 is 4.73 GB: {listed:?}");
        assert!(!listed.contains(&"voxtral-mini-4b-realtime-2602"));

        // 500 MB keeps the three Moonshine variants with their recommended quant.
        let half_gb = ListState {
            max_size: SizeFilter::UpTo500Mb,
            ..ListState::default()
        };
        assert_eq!(
            ids(&half_gb, Language::Auto),
            [
                "moonshine-streaming-medium",
                "moonshine-streaming-small",
                "moonshine-streaming-tiny",
            ]
        );

        // The threshold applies to the selected quant, not to the variant: the
        // 475 MB Q4_K_M of the Nemotron EN variant fits where its 730 MB Q8_0
        // does not.
        let mut with_override = half_gb.clone();
        with_override.set_quant("nemotron-speech-streaming-en-0.6b", "Q4_K_M");
        let listed = ids(&with_override, Language::Auto);
        assert!(
            listed.contains(&"nemotron-speech-streaming-en-0.6b"),
            "{listed:?}"
        );

        // A threshold is inclusive.
        assert_eq!(SizeFilter::All.threshold(), None);
        assert_eq!(SizeFilter::UpTo500Mb.threshold(), Some(500_000_000));
    }

    #[test]
    fn search_matches_label_id_and_family() {
        let search = |query: &str| {
            ids(
                &ListState {
                    search: query.to_owned(),
                    ..ListState::default()
                },
                Language::Auto,
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
        let listed = list.visible(active(), Language::Auto);

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

        let listed = list.visible(active(), Language::Auto);
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
        let german = ids(&list, Language::DeDe);
        let rank = |id: &str| german.iter().position(|candidate| *candidate == id);
        assert!(rank("voxtral-mini-4b-realtime-2602") < rank("nemotron-3.5-asr-streaming-0.6b"));

        // English: the same two models swap places (Nemotron 3.05 vs Voxtral 2.07
        // minus the quant rows); the best English WER leads either way.
        let english = list.visible(active(), Language::EnUs);
        assert_eq!(english[0].variant.id, "parakeet-unified-en-0.6b");
        assert!(english.windows(2).all(|pair| {
            model_catalog::quality(pair[0], Language::EnUs).map(|cell| cell.err_pct)
                <= model_catalog::quality(pair[1], Language::EnUs).map(|cell| cell.err_pct)
        }));

        // Auto-detect sorts by the headline cell (fleurs for Nemotron 3.5).
        let auto = list.visible(active(), Language::Auto);
        assert_eq!(auto[0].variant.id, "parakeet-unified-en-0.6b");
        assert_eq!(auto.len(), 8);
    }

    #[test]
    fn quant_choice_defaults_to_the_recommended_then_the_override() {
        let active = active();
        let mut list = ListState::default();
        let parakeet = variant("parakeet-unified-en-0.6b");
        // The Moonshine variants ship a single quant, so their card is a fixed
        // Q8_0 whatever the overrides say.
        let moonshine = variant("moonshine-streaming-medium");

        assert_eq!(list.quant_for(parakeet, active).quant, RECOMMENDED_QUANT);
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
        assert_eq!(list.quant_for(parakeet, active).quant, RECOMMENDED_QUANT);
        list.set_quant(moonshine.id, "Q4_K_M");
        assert_eq!(list.quant_for(moonshine, active).quant, RECOMMENDED_QUANT);

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
            ids(&list, Language::Auto),
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
        assert!(list.visible(active(), Language::Auto).is_empty());
    }

    #[test]
    fn size_filter_labels_name_their_threshold() {
        assert_eq!(SizeFilter::All.label(), "Any size");
        assert_eq!(SizeFilter::UpTo1Gb.label(), "≤ 1 GB");
        assert_eq!(LanguageFilter::German.label(), "German");
        assert_eq!(SortOrder::default(), SortOrder::Name);
        assert_eq!(LanguageFilter::default(), LanguageFilter::All);
        assert_eq!(SizeFilter::default(), SizeFilter::All);
        assert_eq!(SortOrder::ALL.map(SortOrder::label).len(), 4);
    }

    fn vulkan_info() -> EngineInfo {
        EngineInfo {
            backend: "Vulkan".to_owned(),
            device: "AMD Radeon RX 6950 XT (RADV NAVI21)".to_owned(),
            fallback: None,
        }
    }

    /// FR-005/SC-004: the loaded model names the backend it actually runs on.
    #[test]
    fn backend_line_reports_the_backend_of_the_loaded_model() {
        assert_eq!(
            backend_line(Some(&vulkan_info()), None, None),
            BackendLine {
                text: "Vulkan — AMD Radeon RX 6950 XT (RADV NAVI21)".to_owned(),
                note: None,
            }
        );
    }

    /// FR-005: a CPU fallback keeps its reason on screen instead of only in the
    /// one-shot notice.
    #[test]
    fn backend_line_keeps_the_fallback_reason() {
        let engine = EngineInfo {
            backend: "CPU".to_owned(),
            device: "AMD Ryzen 7 PRO 4750U".to_owned(),
            fallback: Some("failed to initialize the Vulkan backend".to_owned()),
        };

        let line = backend_line(Some(&engine), Some(&vulkan_info()), None);
        assert_eq!(line.text, "CPU — AMD Ryzen 7 PRO 4750U");
        assert_eq!(
            line.note,
            Some((
                "CPU fallback — failed to initialize the Vulkan backend".to_owned(),
                Tone::Warning
            ))
        );
    }

    /// The probe answers "GPU or CPU?" without loading a model — before the
    /// file is even on disk — but only the engine knows about a fallback, so the
    /// probe result is marked as an expectation.
    #[test]
    fn backend_line_marks_a_probe_only_result_as_expected() {
        assert_eq!(
            backend_line(None, Some(&vulkan_info()), None),
            BackendLine {
                text: "Vulkan (expected) — AMD Radeon RX 6950 XT (RADV NAVI21)".to_owned(),
                note: None,
            }
        );

        let cpu_probe = EngineInfo {
            backend: "CPU".to_owned(),
            device: "AMD Ryzen 7 PRO 4750U".to_owned(),
            fallback: Some("no Vulkan device found".to_owned()),
        };
        let line = backend_line(None, Some(&cpu_probe), None);
        assert_eq!(line.text, "CPU (expected) — AMD Ryzen 7 PRO 4750U");
        assert_eq!(
            line.note,
            Some((
                "CPU fallback — no Vulkan device found".to_owned(),
                Tone::Warning
            ))
        );
    }

    #[test]
    fn backend_line_without_any_report_or_error_stays_quiet() {
        assert_eq!(
            backend_line(None, None, None),
            BackendLine {
                text: "not loaded".to_owned(),
                note: None,
            }
        );
    }

    /// A failed load is the one place settings can explain why nothing runs.
    #[test]
    fn backend_line_names_the_engine_error_while_nothing_is_loaded() {
        let line = backend_line(None, None, Some("model load failed for /tmp/x.gguf: boom"));
        assert_eq!(line.text, "not loaded");
        assert_eq!(
            line.note,
            Some((
                "model load failed for /tmp/x.gguf: boom".to_owned(),
                Tone::Error
            ))
        );
    }
}
