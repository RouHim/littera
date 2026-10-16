//! Onboarding surface (FR-001/FR-002/FR-009/FR-014, Scenario 1).

use iced::widget::{Space, button, column, radio, row, scrollable};
use iced::{Element, Length, Task};

use crate::app::{App, AppEvent};
use crate::config::{self, Language};
use crate::hotkey;
use crate::model_catalog;
use crate::models;
use crate::ui::{self, ChipState, Message, Step, Tone, metrics, style};

/// Actions the onboarding surface can produce.
#[derive(Debug, Clone)]
pub enum Action {
    SelectModel(&'static str),
    Download,
    CancelDownload,
    RetryDownload,
    SelectLanguage(Language),
    /// Use the compositor's own binding dialog (Scenario 1.3).
    BindViaPortal,
    /// Capture the chord inside the window (Scenario 1.4).
    StartCapture,
    Back,
    Next,
    Recheck,
    Finish,
}

pub fn update(app: &mut App, action: Action) -> Vec<Task<Message>> {
    match action {
        Action::SelectModel(id) => {
            app.onboarding.model_ready = false;
            app.onboarding.model = id.to_owned();
        }
        Action::Download | Action::RetryDownload => {
            return vec![app.start_download(app.onboarding.selection())];
        }
        Action::CancelDownload => app.cancel_download(),
        Action::SelectLanguage(language) => app.onboarding.language = language,
        Action::BindViaPortal => {
            let chord = app.onboarding.hotkey.clone();
            return vec![crate::app::bind_task(
                &chord,
                Some(config::HotkeySource::Portal),
            )];
        }
        Action::StartCapture => app.onboarding.capture.start(),
        Action::Back => {
            if let Some(previous) = app.onboarding.step.previous() {
                app.onboarding.step = previous;
            }
        }
        Action::Next => {
            // A language mismatch is a warning, never a blocker (FR-011).
            if let Some(next) = app.onboarding.step.next() {
                app.onboarding.step = next;
            }
        }
        Action::Recheck => return app.recheck(),
        Action::Finish => return finish(app),
    }

    Vec::new()
}

fn finish(app: &mut App) -> Vec<Task<Message>> {
    let model = app.onboarding.model.clone();
    let language = app.onboarding.language;
    let hotkey = app.onboarding.hotkey.clone();
    let previous = app.cfg.snapshot();

    if let Err(error) = app.cfg.update(|cfg| {
        cfg.model = model.clone();
        cfg.language = language;
        cfg.hotkey = hotkey.clone();
        cfg.onboarding_complete = true;
    }) {
        tracing::error!(%error, "persisting the onboarding result failed");
    }

    let mut tasks = Vec::new();
    let selection =
        model_catalog::artifact(&model).unwrap_or_else(model_catalog::default_selection);
    if previous.model != selection.artifact.id || !app.model_path.exists() {
        tasks.extend(app.adopt(selection));
    }
    if previous.hotkey != hotkey {
        tasks.push(crate::app::bind_task(
            &hotkey,
            app.cfg.snapshot().hotkey_source,
        ));
    }

    tasks.push(app.close_surface(ui::Surface::Onboarding));
    tasks.push(Task::done(Message::Event(AppEvent::SettingsSaved(
        app.cfg.snapshot(),
    ))));
    tasks
}

pub fn view(app: &App) -> Element<'_, Action> {
    let ui = &app.onboarding;

    let content: Element<'_, Action> = match ui.step {
        Step::Model => model_step(app),
        Step::Language => language_step(app),
        Step::KeyBinding => binding_step(app),
        Step::SystemCheck => check_step(app),
    };

    column![
        ui::title::<Action>("Set up Lingua"),
        ui::lead::<Action>(
            "Local voice dictation: press a hotkey, speak, and the text lands at your cursor."
        ),
        step_indicator(ui.step),
        scrollable(content)
            .height(Length::Fill)
            .direction(ui::scroll_direction())
            .style(style::scrollable),
        footer(app),
    ]
    .spacing(metrics::SPACE_LG)
    .padding(metrics::SPACE_XL)
    .into()
}

/// The four steps as chips: the current one in the accent, finished ones on a
/// surface, upcoming ones quiet.
fn step_indicator(current: Step) -> Element<'static, Action> {
    let mut chips = row![].spacing(metrics::SPACE_SM);

    for step in Step::ALL {
        let state = match step.index().cmp(&current.index()) {
            std::cmp::Ordering::Less => ChipState::Done,
            std::cmp::Ordering::Equal => ChipState::Current,
            std::cmp::Ordering::Greater => ChipState::Upcoming,
        };
        chips = chips.push(ui::step_chip(step.index() + 1, step.title(), state));
    }

    chips.into()
}

/// FR-010: exactly the three curated models, plus a pointer at the full list.
fn model_step(app: &App) -> Element<'_, Action> {
    let ui = &app.onboarding;
    let selected = ui.selection();
    let mut entries = column![].spacing(metrics::SPACE_MD);

    for selection in model_catalog::ONBOARDING_ARTIFACT_IDS
        .iter()
        .filter_map(|id| model_catalog::artifact(id))
    {
        let active = selection == selected;

        let header = row![
            radio(
                selection.label(),
                selection.artifact.id,
                Some(selected.artifact.id),
                Action::SelectModel,
            )
            .style(style::radio)
            .text_size(metrics::TEXT_BODY)
            .spacing(metrics::SPACE_SM),
            Space::new().width(Length::Fill),
            ui::muted::<Action>("Download size"),
            ui::data(models::human_size(selection.artifact.size_bytes)),
        ]
        .spacing(metrics::SPACE_SM)
        .align_y(iced::Alignment::Center);

        let mut body = column![header, ui::lead(selection.variant.note)].spacing(metrics::SPACE_SM);

        if active {
            body = body.push(ui::model_panel(
                app,
                selection,
                ui.model_ready,
                Action::Download,
                Action::CancelDownload,
                Action::RetryDownload,
            ));
            if let Some(warning) = model_catalog::language_warning(selection.variant, ui.language) {
                // Informational only: the model stays selectable (FR-011).
                body = body.push(ui::banner(warning, Tone::Warning));
            }
        }

        entries = entries.push(if active {
            ui::card_active(body)
        } else {
            ui::card(body)
        });
    }

    column![
        ui::heading::<Action>("Model"),
        ui::lead::<Action>(
            "Speech recognition runs fully in-process from this binary — no separate runtime, no cloud."
        ),
        entries,
        ui::muted::<Action>("More models are available in the settings."),
    ]
    .spacing(metrics::SPACE_MD)
    .into()
}

fn language_step(app: &App) -> Element<'_, Action> {
    let ui = &app.onboarding;

    let mut options = column![].spacing(metrics::SPACE_MD);
    for language in Language::ALL {
        options = options.push(
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

    let mut content =
        column![ui::heading::<Action>("Language"), ui::card(options)].spacing(metrics::SPACE_MD);

    if let Some(warning) = ui.language_warning() {
        content = content.push(ui::banner(warning, Tone::Warning));
    } else {
        content = content.push(ui::lead::<Action>(
            "With auto-detect the model picks the language per dictation; language tags never reach the inserted text.",
        ));
    }

    content.into()
}

fn binding_step(app: &App) -> Element<'_, Action> {
    let ui = &app.onboarding;

    let mut bound = column![
        row![ui::muted::<Action>("Chord"), ui::chord(ui.hotkey.clone()),]
            .spacing(metrics::SPACE_SM)
            .align_y(iced::Alignment::Center)
    ]
    .spacing(metrics::SPACE_SM);

    if let Some(binding) = &app.binding {
        bound = bound.push(ui::lead::<Action>(format!(
            "Bound via {} — {}",
            binding.source.label(),
            binding.trigger_description
        )));
    }

    if let Some(error) = &app.hotkey_error {
        bound = bound.push(ui::banner(error.detail.clone(), Tone::Error));
        bound = bound.push(ui::mono::<Action>(error.remediation.clone()));
    } else if let Some(error) = ui.capture.error() {
        bound = bound.push(ui::banner(error.to_owned(), Tone::Error));
    }

    let action: Element<'_, Action> = if ui.capture.is_capturing() {
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
            ui::muted::<Action>("Your compositor will ask for the shortcut in its own dialog."),
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

    let mut content = column![
        ui::heading::<Action>("Key binding"),
        ui::lead::<Action>(format!(
            "Default: {} — both press-to-start/press-to-stop and hold-to-talk work.",
            hotkey::DEFAULT_CHORD
        )),
        ui::card(bound),
        action,
    ]
    .spacing(metrics::SPACE_MD);

    if app.portal_available != Some(true) {
        content = content.push(ui::banner(
            format!(
                "{} — pick a chord your desktop does not already use.",
                hotkey::device_limitation_note()
            ),
            Tone::Warning,
        ));
    }

    content.into()
}

fn check_step(app: &App) -> Element<'_, Action> {
    let mut content = column![ui::heading::<Action>("System check")].spacing(metrics::SPACE_MD);

    if app.checks.is_empty() {
        // Loading: placeholder rows in the shape the results take.
        return content
            .push(ui::lead::<Action>("Running the capability checks…"))
            .push(ui::check_skeleton::<Action>(3))
            .into();
    }

    let mut checks = column![].spacing(metrics::SPACE_SM);
    for check in &app.checks {
        checks = checks.push(ui::check_row(check));
    }

    let failing = app.checks.iter().filter(|check| !check.ok).count();

    content = content.push(checks);
    content = content.push(if failing > 0 {
        ui::banner(
            format!(
                "{failing} capability check(s) failed — the app still runs, but the affected feature stays limited."
            ),
            Tone::Warning,
        )
    } else {
        ui::banner("Everything checks out.", Tone::Success)
    });
    content = content.push(row![
        button("Re-check")
            .style(style::secondary_button)
            .padding(metrics::BUTTON_PADDING)
            .on_press(Action::Recheck)
    ]);

    content.into()
}

fn footer(app: &App) -> Element<'_, Action> {
    let ui = &app.onboarding;

    let back = match ui.step.previous() {
        Some(_) => button("Back")
            .style(style::secondary_button)
            .padding(metrics::BUTTON_PADDING)
            .on_press(Action::Back),
        None => button("Back")
            .style(style::secondary_button)
            .padding(metrics::BUTTON_PADDING),
    };

    let next: Element<'_, Action> = if ui.step.next().is_some() {
        button("Next")
            .style(style::primary_button)
            .padding(metrics::BUTTON_PADDING)
            .on_press(Action::Next)
            .into()
    } else {
        button("Finish")
            .style(style::primary_button)
            .padding(metrics::BUTTON_PADDING)
            .on_press(Action::Finish)
            .into()
    };

    row![back]
        .spacing(metrics::SPACE_MD)
        .align_y(iced::Alignment::Center)
        .push(Space::new().width(Length::Fill))
        .push(next)
        .into()
}
