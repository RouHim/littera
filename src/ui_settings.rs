//! Settings surface (FR-016/FR-022, Scenario 3).

use iced::widget::{Space, button, column, radio, row, scrollable, text};
use iced::{Element, Length, Task};

use crate::app::{App, AppEvent};
use crate::config::{self, BackendPreference, Language};
use crate::hotkey;
use crate::models;
use crate::overlay::OverlayBackend;
use crate::ui::{self, Message, Section, Tone, metrics, style};

/// Actions the settings surface can produce.
#[derive(Debug, Clone)]
pub enum Action {
    SelectSection(Section),
    SelectModel(String),
    Download,
    CancelDownload,
    RetryDownload,
    SelectLanguage(Language),
    SelectBackend(BackendPreference),
    BindViaPortal,
    StartCapture,
    Save,
    Recheck,
    ReopenOnboarding,
}

pub fn update(app: &mut App, action: Action) -> Vec<Task<Message>> {
    match action {
        Action::SelectSection(section) => app.settings.section = section,
        Action::SelectModel(id) => app.settings.model = id,
        Action::Download | Action::RetryDownload => {
            let entry = app.settings.entry();
            return vec![app.start_download(entry)];
        }
        Action::CancelDownload => app.cancel_download(),
        Action::SelectLanguage(language) => app.settings.language = language,
        Action::SelectBackend(backend) => app.settings.backend = backend,
        Action::BindViaPortal => {
            let chord = app.settings.effective_hotkey();
            return vec![crate::app::bind_task(
                &chord,
                Some(config::HotkeySource::Portal),
            )];
        }
        Action::StartCapture => app.settings.capture.start(),
        Action::Save => return save(app),
        Action::Recheck => return app.recheck(),
        Action::ReopenOnboarding => return vec![Task::done(Message::ReopenOnboarding)],
    }

    Vec::new()
}

fn save(app: &mut App) -> Vec<Task<Message>> {
    if app.settings.language_conflict().is_some() {
        return Vec::new();
    }

    let model = app.settings.model.clone();
    let language = app.settings.language;
    let backend = app.settings.backend;
    let hotkey = app.settings.effective_hotkey();
    let previous = app.cfg.snapshot();

    if let Err(error) = app.cfg.update(|cfg| {
        cfg.model = model.clone();
        cfg.language = language;
        cfg.backend = backend;
        cfg.hotkey = hotkey.clone();
    }) {
        tracing::error!(%error, "saving settings failed");
        app.settings.note = Some(format!("Saving failed: {error:#}"));
        return Vec::new();
    }

    app.settings.hotkey = hotkey.clone();
    app.settings.hotkey_draft = None;
    app.settings.capture.reset();
    app.settings.note = None;

    let mut tasks = Vec::new();

    if previous.model != model || previous.backend != backend {
        let entry = models::entry(&model).unwrap_or_else(models::default_entry);
        tasks.extend(app.adopt_model(entry));
    }

    if previous.hotkey != hotkey {
        let preferred = app.cfg.snapshot().hotkey_source;
        tasks.push(crate::app::bind_task(&hotkey, preferred));
    }

    tasks.push(Task::done(Message::Event(AppEvent::SettingsSaved(
        app.cfg.snapshot(),
    ))));
    tasks
}

pub fn view(app: &App) -> Element<'_, Action> {
    let ui = &app.settings;

    let mut tabs = row![].spacing(metrics::SPACE_SM);
    for section in Section::ALL {
        let selected = section == ui.section;
        tabs = tabs.push(
            button(text(section.title()).size(metrics::TEXT_BODY))
                .style(style::segment(selected))
                .padding(metrics::SEGMENT_PADDING)
                .on_press(Action::SelectSection(section)),
        );
    }

    let content: Element<'_, Action> = match ui.section {
        Section::Dictation => dictation_section(app),
        Section::SystemCheck => system_section(app),
        Section::Diagnostics => diagnostics_section(app),
    };

    column![
        ui::title::<Action>("Lingua settings"),
        tabs,
        scrollable(content)
            .height(Length::Fill)
            .direction(ui::scroll_direction())
            .style(style::scrollable),
    ]
    .spacing(metrics::SPACE_LG)
    .padding(metrics::SPACE_XL)
    .into()
}

fn dictation_section(app: &App) -> Element<'_, Action> {
    let ui = &app.settings;
    let config = app.cfg.snapshot();

    let selected_index = models::CATALOG
        .iter()
        .position(|entry| entry.id == ui.model)
        .unwrap_or(0);

    let mut models_column = column![].spacing(metrics::SPACE_MD);
    for (index, entry) in models::CATALOG.iter().enumerate() {
        let selected = entry.id == ui.model;
        let header = row![
            radio(entry.label, index, Some(selected_index), move |index| {
                Action::SelectModel(models::CATALOG[index].id.to_owned())
            })
            .style(style::radio)
            .text_size(metrics::TEXT_BODY)
            .spacing(metrics::SPACE_SM),
            Space::new().width(Length::Fill),
            ui::muted::<Action>("Download size"),
            ui::data(entry.approx_size_label),
        ]
        .spacing(metrics::SPACE_SM)
        .align_y(iced::Alignment::Center);

        let mut body = column![header, ui::lead(entry.note)].spacing(metrics::SPACE_SM);

        if selected {
            let verified = app
                .engine_info
                .as_ref()
                .is_some_and(|_| app.model_entry.id == entry.id);
            body = body.push(ui::model_panel(
                app,
                entry,
                verified,
                Action::Download,
                Action::CancelDownload,
                Action::RetryDownload,
            ));
        }

        models_column = models_column.push(if selected {
            ui::card_active(body)
        } else {
            ui::card(body)
        });
    }

    let mut languages = column![].spacing(metrics::SPACE_MD);
    for language in Language::ALL {
        languages = languages.push(
            radio(
                format!("{}  ({})", language.display_name(), language.label()),
                language,
                Some(ui.language),
                Action::SelectLanguage,
            )
            .style(style::radio)
            .text_size(metrics::TEXT_BODY)
            .spacing(metrics::SPACE_SM),
        );
    }

    let mut language_card = column![ui::card(languages)].spacing(metrics::SPACE_MD);
    if let Some(warning) = ui.language_conflict() {
        language_card = language_card.push(ui::banner(warning, Tone::Warning));
    }

    let mut backends = column![].spacing(metrics::SPACE_MD);
    for preference in BackendPreference::ALL {
        let label = match preference {
            BackendPreference::Auto => "auto — Vulkan when available, CPU otherwise",
            BackendPreference::Vulkan => "vulkan — require the GPU backend",
            BackendPreference::Cpu => "cpu — force the CPU backend",
        };
        backends = backends.push(
            radio(label, preference, Some(ui.backend), Action::SelectBackend)
                .style(style::radio)
                .text_size(metrics::TEXT_BODY)
                .spacing(metrics::SPACE_SM),
        );
    }

    let mut binding = column![
        row![ui::muted::<Action>("Chord"), ui::chord(ui.hotkey.clone()),]
            .spacing(metrics::SPACE_SM)
            .align_y(iced::Alignment::Center),
        row![
            ui::muted::<Action>("Configured"),
            ui::data(config.hotkey.clone()),
        ]
        .spacing(metrics::SPACE_SM)
        .align_y(iced::Alignment::Center),
    ]
    .spacing(metrics::SPACE_SM);

    if let Some(active) = &app.binding {
        binding = binding.push(ui::lead::<Action>(format!(
            "Active mechanism: {} — {}",
            active.source.label(),
            active.trigger_description
        )));
    }

    if let Some(draft) = &ui.hotkey_draft {
        binding = binding.push(ui::banner(
            format!("Captured {draft} — press Save to apply it."),
            Tone::Info,
        ));
    }

    if let Some(error) = &app.hotkey_error {
        binding = binding.push(ui::banner(error.detail.clone(), Tone::Error));
        binding = binding.push(ui::mono::<Action>(error.remediation.clone()));
    } else if let Some(error) = ui.capture.error() {
        binding = binding.push(ui::banner(error.to_owned(), Tone::Error));
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
        row![
            button("Bind…")
                .style(style::primary_button)
                .padding(metrics::BUTTON_PADDING)
                .on_press(Action::StartCapture),
            ui::muted::<Action>(
                "No shortcut portal here: press the combination inside this window."
            ),
        ]
        .spacing(metrics::SPACE_MD)
        .align_y(iced::Alignment::Center)
        .into()
    };

    let mut hotkey_card = column![ui::card(binding), bind_action].spacing(metrics::SPACE_SM);
    if app.portal_available != Some(true) {
        hotkey_card = hotkey_card.push(ui::banner(
            format!(
                "{} — pick a chord your desktop does not already use.",
                hotkey::device_limitation_note()
            ),
            Tone::Warning,
        ));
    }

    let storage = column![
        column![
            ui::muted::<Action>("Models are stored in"),
            ui::mono::<Action>(app.model_dir_display()),
        ]
        .spacing(metrics::SPACE_XS),
        column![
            ui::muted::<Action>("Current model file"),
            ui::mono::<Action>(app.model_path.display().to_string()),
        ]
        .spacing(metrics::SPACE_XS),
    ]
    .spacing(metrics::SPACE_MD);

    let dirty = ui.dirty(&config);
    let save = if dirty && ui.language_conflict().is_none() {
        button("Save")
            .style(style::primary_button)
            .padding(metrics::BUTTON_PADDING)
            .on_press(Action::Save)
    } else {
        button("Save")
            .style(style::primary_button)
            .padding(metrics::BUTTON_PADDING)
    };

    let mut save_row = row![save]
        .spacing(metrics::SPACE_MD)
        .align_y(iced::Alignment::Center);
    if dirty {
        save_row = save_row.push(ui::muted::<Action>("Changes apply to the next dictation."));
    } else {
        save_row = save_row.push(ui::muted::<Action>("Everything is saved."));
    }
    if let Some(note) = &ui.note {
        save_row = save_row.push(ui::muted::<Action>(note.clone()));
    }

    column![
        ui::heading::<Action>("Model"),
        models_column,
        ui::heading::<Action>("Language"),
        language_card,
        ui::heading::<Action>("Backend"),
        ui::card(backends),
        ui::heading::<Action>("Hotkey"),
        hotkey_card,
        ui::heading::<Action>("Storage"),
        ui::card(storage),
        save_row,
    ]
    .spacing(metrics::SPACE_MD)
    .into()
}

fn system_section(app: &App) -> Element<'_, Action> {
    let content = column![
        ui::heading::<Action>("System check"),
        ui::lead::<Action>("Each check names the exact command or configuration needed to fix it."),
    ]
    .spacing(metrics::SPACE_MD);

    if app.checks.is_empty() {
        // Loading: placeholder rows in the shape the results take.
        return content.push(ui::check_skeleton::<Action>(3)).into();
    }

    let mut checks = column![].spacing(metrics::SPACE_SM);
    for check in &app.checks {
        checks = checks.push(ui::check_row(check));
    }

    let failing = app.checks.iter().filter(|check| !check.ok).count();
    let summary = if failing == 0 {
        ui::banner("All capabilities are available.", Tone::Success)
    } else {
        ui::banner(
            format!(
                "{failing} check(s) failed — the affected capability stays limited until it is fixed."
            ),
            Tone::Warning,
        )
    };

    content
        .push(checks)
        .push(summary)
        .push(
            row![
                button("Re-check")
                    .style(style::secondary_button)
                    .padding(metrics::BUTTON_PADDING)
                    .on_press(Action::Recheck),
                button("Run onboarding again")
                    .style(style::secondary_button)
                    .padding(metrics::BUTTON_PADDING)
                    .on_press(Action::ReopenOnboarding),
            ]
            .spacing(metrics::SPACE_MD),
        )
        .into()
}

fn diagnostics_section(app: &App) -> Element<'_, Action> {
    let config = app.cfg.snapshot();
    let mut rows = column![].spacing(metrics::SPACE_LG);

    let engine = app.engine_info.as_ref().or(app.probe_info.as_ref());
    let engine_value = match engine {
        Some(info) => match &info.fallback {
            Some(fallback) => format!("{}: {} (fallback: {fallback})", info.backend, info.device),
            None => format!("{}: {}", info.backend, info.device),
        },
        None => app
            .engine_error
            .clone()
            .unwrap_or_else(|| "not loaded yet".to_owned()),
    };

    let hotkey_value = match (&app.binding, &app.hotkey_error) {
        (Some(binding), _) => format!(
            "{} via {} — {}",
            binding.chord,
            binding.source.label(),
            binding.trigger_description
        ),
        (None, Some(error)) => format!("unbound: {}", error.detail),
        (None, None) => "binding…".to_owned(),
    };

    let overlay_value = match app.overlay_backend {
        OverlayBackend::Layer => format!("layer-shell — {}", app.overlay_reason),
        OverlayBackend::X11 => format!("X11 non-activating window — {}", app.overlay_reason),
        OverlayBackend::None => format!("disabled — {}", app.overlay_reason),
    };

    let microphone = match app.last_capture {
        Some((rate, channels)) => format!("{rate} Hz, {channels} ch → 16000 Hz mono"),
        None => "not captured yet in this session".to_owned(),
    };

    let text_entry = match &app.paste_note {
        Some(reason) => format!("clipboard only ({reason})"),
        None => "synthesized Ctrl+V via /dev/uinput".to_owned(),
    };

    rows = rows
        .push(kv("Backend & device", engine_value))
        .push(kv(
            "Model",
            format!("{} ({})", config.model, app.model_path.display()),
        ))
        .push(kv("Model directory", app.model_dir_display()))
        .push(kv(
            "Language",
            format!(
                "{} ({})",
                config.language.label(),
                config.language.display_name()
            ),
        ))
        .push(kv("Hotkey", hotkey_value))
        .push(kv("Overlay", overlay_value))
        .push(kv("Text insertion", text_entry))
        .push(kv("Clipboard", app.clipboard.mechanism().to_owned()))
        .push(kv("Microphone", microphone));

    if let Some(error) = &app.engine_error {
        rows = rows.push(kv("Last engine error", error.clone()));
    }

    column![
        ui::heading::<Action>("Diagnostics"),
        ui::lead::<Action>("Effective values of the running instance — no log reading required."),
        ui::card(rows),
    ]
    .spacing(metrics::SPACE_MD)
    .into()
}

/// One diagnostics row: the field name above its value, so long paths wrap
/// instead of clipping at the window's minimum width.
fn kv(key: &'static str, value: String) -> Element<'static, Action> {
    column![ui::muted::<Action>(key), ui::mono::<Action>(value),]
        .spacing(metrics::SPACE_XS)
        .into()
}
