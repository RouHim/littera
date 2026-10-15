//! Application state machine and background wiring.
//!
//! The gesture rules of FR-010 live in [`DictationMachine`] (pure, unit-tested);
//! [`App`] connects that machine to the engine, capture, overlay, inserter,
//! tray and IPC.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use iced::window;
use iced::{Subscription, Task};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::config::{self, SharedConfig};
use crate::models;
use crate::ui::{self, Message};
use crate::{audio, clipboard, diagnostics, engine, hotkey, inserter, ipc, overlay, tray};

/// Minimum hold time that distinguishes a hold-to-talk release from the release
/// of a tap that started the recording (FR-010).
pub const MIN_HOLD: Duration = Duration::from_millis(300);

/// How long the terminal dictation states stay on screen.
const NO_SPEECH_TTL: Duration = Duration::from_secs(2);
const COPIED_TTL: Duration = Duration::from_secs(4);
const FAILED_TTL: Duration = Duration::from_secs(6);
/// Transient notices (download progress, model ready, …).
const NOTICE_TTL: Duration = Duration::from_secs(8);

/// Everything that can reach the application from the outside.
#[derive(Clone)]
pub enum AppEvent {
    Hotkey(hotkey::HotkeyEvent),
    /// Audio for one specific recording; stale generations are dropped so a
    /// stopped capture's tail can never leak into the next dictation.
    Chunk {
        generation: u64,
        chunk: audio::Chunk,
    },
    Eng(engine::EngineEvent),
    Tick,
    IpcOpenSettings,
    IpcFailed(String),
    Quit,
    SettingsSaved(config::Config),
    ModelProgress {
        id: &'static str,
        received: u64,
        total: u64,
    },
    ModelFinished {
        id: &'static str,
        result: Result<PathBuf, String>,
    },
    HotkeyBound(Result<hotkey::HotkeyHandle, hotkey::BindError>),
    PortalProbed(bool),
    TrayReady(Option<tray::TrayState>),
}

impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            AppEvent::Hotkey(_) => "Hotkey",
            AppEvent::Chunk { .. } => "Chunk",
            AppEvent::Eng(_) => "Eng",
            AppEvent::Tick => "Tick",
            AppEvent::IpcOpenSettings => "IpcOpenSettings",
            AppEvent::IpcFailed(_) => "IpcFailed",
            AppEvent::Quit => "Quit",
            AppEvent::SettingsSaved(_) => "SettingsSaved",
            AppEvent::ModelProgress { .. } => "ModelProgress",
            AppEvent::ModelFinished { .. } => "ModelFinished",
            AppEvent::HotkeyBound(_) => "HotkeyBound",
            AppEvent::PortalProbed(_) => "PortalProbed",
            AppEvent::TrayReady(_) => "TrayReady",
        };
        write!(f, "AppEvent::{name}")
    }
}

/// The single channel every background producer feeds.
static EVENT_CHANNEL: OnceLock<(
    UnboundedSender<AppEvent>,
    tokio::sync::Mutex<UnboundedReceiver<AppEvent>>,
)> = OnceLock::new();

fn event_channel() -> &'static (
    UnboundedSender<AppEvent>,
    tokio::sync::Mutex<UnboundedReceiver<AppEvent>>,
) {
    EVENT_CHANNEL.get_or_init(|| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (tx, tokio::sync::Mutex::new(rx))
    })
}

/// A `Send` handle for background producers.
pub fn events() -> UnboundedSender<AppEvent> {
    event_channel().0.clone()
}

fn spawn_pump<T: Send + 'static>(
    name: &str,
    rx: std::sync::mpsc::Receiver<T>,
    tx: UnboundedSender<AppEvent>,
    map: impl Fn(T) -> AppEvent + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            while let Ok(item) = rx.recv() {
                if tx.send(map(item)).is_err() {
                    break;
                }
            }
        });

    if let Err(error) = spawned {
        tracing::error!(%error, name, "background forwarder thread could not be started");
    }
}

/// Recording gesture state (FR-010).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// Release behaviour not yet decided: it was either a hold or a tap.
    Undetermined,
    /// The triggering release was short, so the recording continues until the
    /// next press.
    TapStarted,
}

/// High-level dictation state (the plan's `Dictation`, with the transient data
/// of the recording phase kept as machine fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictationState {
    Idle,
    Recording,
    AwaitingFinal,
    Inserting,
    NoSpeech,
    /// Transcript is on the clipboard only; `String` is the user-visible reason.
    Copied(String),
    Failed(String),
}

/// Side effects the machine asks [`App`] to carry out, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum DictationEffect {
    StartCapture,
    StopCapture,
    BeginEngine,
    FeedEngine(Vec<f32>),
    StopEngine,
    Insert(String),
    /// Insert a transcript that belongs to an earlier cycle (a dictation that
    /// was still finalising when the current one started, Scenario 4.6). It
    /// must not change the current cycle's state.
    InsertDetached(String),
    OverlayChanged,
}

/// Pure gesture/state machine for one dictation cycle (FR-010/FR-018).
#[derive(Debug)]
pub struct DictationMachine {
    state: DictationState,
    entered: Instant,
    started: Instant,
    mode: Gesture,
    samples: u64,
    level: f32,
    queued: bool,
    partial: String,
    /// Dictations that were still finalising when a newer one started; each one
    /// still owes exactly one `Final`.
    pending_finals: u32,
}

impl Default for DictationMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl DictationMachine {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            state: DictationState::Idle,
            entered: now,
            started: now,
            mode: Gesture::Undetermined,
            samples: 0,
            level: 0.0,
            queued: false,
            partial: String::new(),
            pending_finals: 0,
        }
    }
    pub fn state(&self) -> &DictationState {
        &self.state
    }

    pub fn partial_text(&self) -> &str {
        &self.partial
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn is_queued(&self) -> bool {
        self.queued
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }

    pub fn is_recording(&self) -> bool {
        self.state == DictationState::Recording
    }

    /// Anything but `Idle`: the overlay is up and the periodic tick is needed.
    pub fn is_active(&self) -> bool {
        self.state != DictationState::Idle
    }

    fn enter(&mut self, state: DictationState, now: Instant) {
        self.state = state;
        self.entered = now;
    }

    fn begin(&mut self, now: Instant) -> Vec<DictationEffect> {
        self.enter(DictationState::Recording, now);
        self.started = now;
        self.mode = Gesture::Undetermined;
        self.samples = 0;
        self.level = 0.0;
        self.queued = false;
        self.partial.clear();
        vec![
            DictationEffect::StartCapture,
            DictationEffect::BeginEngine,
            DictationEffect::OverlayChanged,
        ]
    }

    fn stop(&mut self, now: Instant) -> Vec<DictationEffect> {
        let too_short = self.samples < audio::MIN_RECORDING_SAMPLES;
        self.level = 0.0;
        self.queued = false;

        if too_short {
            tracing::debug!(
                samples = self.samples,
                "recording discarded: too short (FR-018)"
            );
            self.enter(DictationState::Idle, now);
            // The engine already holds a stream for this recording. Closing it
            // releases the model lease, so the next dictation is not queued
            // behind a stream nobody owns; the resulting empty Final is
            // ignored because this state is no longer AwaitingFinal.
            return vec![
                DictationEffect::StopCapture,
                DictationEffect::StopEngine,
                DictationEffect::OverlayChanged,
            ];
        }

        self.enter(DictationState::AwaitingFinal, now);
        vec![
            DictationEffect::StopCapture,
            DictationEffect::StopEngine,
            DictationEffect::OverlayChanged,
        ]
    }

    /// A hotkey press/release arrived.
    pub fn hotkey(&mut self, event: hotkey::HotkeyEvent, now: Instant) -> Vec<DictationEffect> {
        match (event, &self.state) {
            (hotkey::HotkeyEvent::Press, DictationState::Recording) => self.stop(now),
            (hotkey::HotkeyEvent::Press, DictationState::AwaitingFinal) => {
                // The previous dictation is still finalising: its Final will
                // arrive later under load and must not disturb this new cycle
                // (Scenario 4.6).
                self.pending_finals += 1;
                self.begin(now)
            }
            (hotkey::HotkeyEvent::Press, _) => self.begin(now),
            (hotkey::HotkeyEvent::Release, DictationState::Recording) => {
                let held = self.elapsed(now);
                match self.mode {
                    Gesture::TapStarted => Vec::new(),
                    Gesture::Undetermined if held >= MIN_HOLD => self.stop(now),
                    Gesture::Undetermined => {
                        // The release belongs to the tap that started this
                        // recording; keep recording until the next press.
                        self.mode = Gesture::TapStarted;
                        Vec::new()
                    }
                }
            }
            (hotkey::HotkeyEvent::Release, _) => Vec::new(),
        }
    }

    /// One 100 ms audio chunk was captured.
    pub fn chunk(&mut self, chunk: &audio::Chunk) -> Vec<DictationEffect> {
        if self.state != DictationState::Recording {
            return Vec::new();
        }

        self.samples += chunk.samples.len() as u64;
        self.level = chunk.rms;

        let capped = self.samples >= u64::from(audio::TARGET_RATE) * audio::MAX_RECORDING_SECS;
        let mut effects = vec![DictationEffect::FeedEngine(chunk.samples.clone())];

        if capped {
            tracing::info!("five-minute recording cap reached; stopping automatically (FR-018)");
            effects.extend(self.stop(Instant::now()));
        } else {
            effects.push(DictationEffect::OverlayChanged);
        }

        effects
    }

    /// The engine emitted incremental text.
    pub fn partial(&mut self, text: String) -> Vec<DictationEffect> {
        if !self.is_recording() && self.state != DictationState::AwaitingFinal {
            return Vec::new();
        }
        self.queued = false;
        self.partial = text;
        vec![DictationEffect::OverlayChanged]
    }

    /// The engine queued this dictation behind a running one (Scenario 4.6).
    pub fn queued(&mut self) -> Vec<DictationEffect> {
        if self.state != DictationState::Recording {
            return Vec::new();
        }
        self.queued = true;
        vec![DictationEffect::OverlayChanged]
    }

    /// The engine produced the final transcript (FR-006: exactly one insertion).
    pub fn final_text(&mut self, text: String, now: Instant) -> Vec<DictationEffect> {
        if self.pending_finals > 0 {
            // This Final belongs to a dictation that was still finalising when
            // the current cycle started: insert it, but leave the current cycle
            // (its recording, overlay and own Final) untouched.
            self.pending_finals -= 1;
            let text = text.trim().to_owned();
            if text.is_empty() {
                tracing::debug!("the previous dictation produced no text; nothing inserted");
                return Vec::new();
            }
            return vec![DictationEffect::InsertDetached(text)];
        }

        if !matches!(
            self.state,
            DictationState::AwaitingFinal | DictationState::Recording
        ) {
            return Vec::new();
        }

        self.level = 0.0;
        self.queued = false;

        if text.trim().is_empty() {
            self.partial.clear();
            self.enter(DictationState::NoSpeech, now);
            return vec![DictationEffect::OverlayChanged];
        }

        self.partial.clone_from(&text);
        self.enter(DictationState::Inserting, now);
        vec![
            DictationEffect::Insert(text),
            DictationEffect::OverlayChanged,
        ]
    }

    /// The insertion attempt finished.
    pub fn insert_finished(
        &mut self,
        outcome: &Result<inserter::InsertOutcome, String>,
        now: Instant,
    ) -> Vec<DictationEffect> {
        if self.state != DictationState::Inserting {
            return Vec::new();
        }

        match outcome {
            Ok(inserter::InsertOutcome::Inserted) => {
                self.enter(DictationState::Idle, now);
            }
            Ok(inserter::InsertOutcome::CopiedOnly { reason, .. }) => {
                self.enter(DictationState::Copied(reason.clone()), now);
            }
            Err(error) => {
                self.enter(DictationState::Failed(error.clone()), now);
            }
        }

        vec![DictationEffect::OverlayChanged]
    }

    /// The engine failed (Scenario 4.5): discard the recording, insert nothing.
    pub fn failed(&mut self, message: String, now: Instant) -> Vec<DictationEffect> {
        self.level = 0.0;
        self.queued = false;
        self.partial.clear();
        // A failed engine clears its queue, so no earlier cycle owes a Final.
        self.pending_finals = 0;
        self.enter(DictationState::Failed(message), now);
        vec![DictationEffect::OverlayChanged]
    }

    /// Timeout handling for the terminal states.
    pub fn tick(&mut self, now: Instant) -> Vec<DictationEffect> {
        let ttl = match &self.state {
            DictationState::NoSpeech => Some(NO_SPEECH_TTL),
            DictationState::Copied(_) => Some(COPIED_TTL),
            DictationState::Failed(_) => Some(FAILED_TTL),
            _ => None,
        };

        if let Some(ttl) = ttl
            && now.saturating_duration_since(self.entered) >= ttl
        {
            self.partial.clear();
            self.enter(DictationState::Idle, now);
            return vec![DictationEffect::OverlayChanged];
        }

        Vec::new()
    }
}

/// The application.
pub struct App {
    /// Live settings (FR-016); every subsystem reads through this.
    pub cfg: SharedConfig,
    pub engine: engine::Engine,
    pub model_entry: &'static models::ModelEntry,
    pub model_path: PathBuf,
    pub engine_info: Option<engine::EngineInfo>,
    pub engine_error: Option<String>,
    pub probe_info: Option<engine::EngineInfo>,
    pub probe_error: Option<String>,

    pub capture: Option<audio::Capture>,
    /// Generation of the recording currently being captured.
    chunk_generation: u64,
    /// Sample rate and channel count of the last capture device (diagnostics).
    pub last_capture: Option<(u32, u16)>,

    pub clipboard: Arc<clipboard::Clipboard>,
    pub inserter: Box<dyn inserter::TextInserter>,
    pub paste_note: Option<String>,

    pub overlay: Box<dyn overlay::OverlayPresenter>,
    pub overlay_backend: overlay::OverlayBackend,
    pub overlay_reason: String,
    overlay_visible: bool,
    last_overlay_draw: Option<Instant>,

    pub hotkey: Option<hotkey::HotkeyHandle>,
    pub binding: Option<hotkey::Binding>,
    pub hotkey_error: Option<hotkey::BindError>,
    /// `None` until probed; drives the binding UX (compositor dialog vs. in-window capture).
    pub portal_available: Option<bool>,

    pub machine: DictationMachine,
    pub checks: Vec<diagnostics::Check>,
    pub windows: BTreeMap<window::Id, ui::Surface>,
    pub tray: Option<tray::TrayState>,
    tray_state: String,

    pub download: Option<DownloadState>,
    pub notice: Option<(String, Instant)>,
    pub onboarding: ui::OnboardingUi,
    pub settings: ui::SettingsUi,

    events: UnboundedSender<AppEvent>,
}

/// One model download in flight (FR-003).
pub struct DownloadState {
    pub entry: &'static models::ModelEntry,
    pub cancel: Arc<AtomicBool>,
    pub received: u64,
    pub total: u64,
    pub error: Option<String>,
}

impl DownloadState {
    pub fn percent(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.received as f64 / self.total as f64 * 100.0) as f32
        }
    }

    pub fn is_running(&self) -> bool {
        self.error.is_none()
    }
}

impl App {
    /// iced boot: build every subsystem and the initial task set.
    pub fn boot() -> (Self, Task<Message>) {
        let loaded = config::Config::load();
        let (cfg, load_error) = match loaded {
            Ok(cfg) => (cfg, None),
            Err(error) => (config::Config::default(), Some(format!("{error:#}"))),
        };

        let entry = models::entry(&cfg.model).unwrap_or_else(models::default_entry);
        let model_path = models::path_for(entry).unwrap_or_default();

        let runtime_events = events();

        let (engine_tx, engine_rx) = std::sync::mpsc::channel();
        spawn_pump(
            "engine-pump",
            engine_rx,
            runtime_events.clone(),
            AppEvent::Eng,
        );

        let engine = engine::spawn(entry, model_path.clone(), cfg.backend, engine_tx);
        let (probe_info, probe_error) = engine::probe_devices(cfg.backend);
        if probe_info.is_none() {
            tracing::warn!(
                error = probe_error.as_deref().unwrap_or("unknown"),
                "no compute device detected"
            );
        }

        let clipboard = match clipboard::Clipboard::new() {
            Ok(clipboard) => Arc::new(clipboard),
            Err(error) => {
                // Session/compositor gone: exit cleanly with a logged reason
                // instead of staying half-functional (spec edge case).
                tracing::error!(%error, "cannot own the Wayland clipboard; exiting");
                std::process::exit(1);
            }
        };

        let (inserter, paste_note) = inserter::select(Arc::clone(&clipboard));

        let (overlay_backend, overlay_reason) = overlay::select_backend();
        let overlay = match overlay::spawn(overlay_backend) {
            Ok(presenter) => presenter,
            Err(error) => {
                tracing::warn!(%error, "overlay backend failed to start; continuing without an indicator");
                overlay::noop()
            }
        };
        if let Some(note) = &paste_note {
            tracing::warn!(note, "auto-paste unavailable; running clipboard-only");
        }

        let mut app = Self {
            cfg: SharedConfig::new(cfg.clone()),
            engine,
            model_entry: entry,
            model_path,
            engine_info: None,
            engine_error: None,
            probe_info,
            probe_error,
            capture: None,
            chunk_generation: 0,
            last_capture: None,
            clipboard,
            inserter,
            paste_note,
            overlay,
            overlay_backend,
            overlay_reason,
            overlay_visible: false,
            last_overlay_draw: None,
            hotkey: None,
            binding: None,
            hotkey_error: None,
            portal_available: None,
            machine: DictationMachine::new(),
            checks: Vec::new(),
            windows: BTreeMap::new(),
            tray: None,
            tray_state: "Idle".to_owned(),
            download: None,
            notice: None,
            onboarding: ui::OnboardingUi::from_config(&cfg),
            settings: ui::SettingsUi::from_config(&cfg),
            events: runtime_events,
        };

        app.refresh_checks();
        app.refresh_tray();

        let mut tasks = vec![
            // Global hotkey (FR-008/FR-009).
            bind_task(&cfg.hotkey, cfg.hotkey_source),
            // Portal availability decides whether rebinding uses the compositor
            // dialog or the in-window capture (Scenario 1.3/1.4).
            Task::perform(hotkey::portal_available(), |available| {
                Message::Event(AppEvent::PortalProbed(available))
            }),
            // Tray icon (FR-017).
            Task::perform(tray::spawn(app.events.clone()), |tray| {
                Message::Event(AppEvent::TrayReady(tray))
            }),
            // Single-instance socket (FR-015).
            {
                let events = app.events.clone();
                Task::perform(
                    async move {
                        ipc::serve(events)
                            .await
                            .map_err(|error| format!("{error:#}"))
                    },
                    |result| {
                        Message::Event(match result {
                            Ok(()) => {
                                AppEvent::IpcFailed("ipc server stopped unexpectedly".to_owned())
                            }
                            Err(error) => AppEvent::IpcFailed(error),
                        })
                    },
                )
            },
        ];

        // Load an already downloaded model so the first dictation is instant.
        if app.model_path.exists() {
            app.engine.send(engine::EngineCmd::LoadModel(
                app.model_path.clone(),
                cfg.backend,
            ));
        } else {
            tracing::info!(model = entry.id, "model not downloaded yet");
        }

        // Onboarding on the very first run (FR-001); the daemon stays windowless
        // afterwards (Scenario 3.3, FR-015).
        if !cfg.onboarding_complete {
            tasks.push(Task::done(Message::OpenOnboarding));
        }

        if let Some(error) = load_error {
            tracing::error!(
                error,
                "settings could not be loaded; continuing with defaults"
            );
            app.set_notice(format!("Settings could not be loaded: {error}"));
        }

        (app, Task::batch(tasks))
    }

    /// iced update: batch the machine's tasks.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        Task::batch(self.handle(message))
    }

    /// iced view dispatch.
    pub fn view(&self, id: window::Id) -> iced::Element<'_, Message> {
        ui::view(self, id)
    }

    /// iced window title.
    pub fn title(&self, id: window::Id) -> String {
        ui::window_title(self, id)
    }

    /// iced window theme.
    pub fn theme(&self, id: window::Id) -> iced::Theme {
        ui::theme(self, id)
    }

    /// iced update entry point.
    pub fn handle(&mut self, message: Message) -> Vec<Task<Message>> {
        match message {
            Message::Event(event) => self.handle_event(event),
            Message::WindowClosed(id) => {
                self.windows.remove(&id);
                Vec::new()
            }
            Message::WindowOpened(id) => {
                tracing::debug!(?id, "window opened");
                Vec::new()
            }
            Message::OpenOnboarding => {
                self.onboarding = ui::OnboardingUi::from_config(&self.cfg.snapshot());
                vec![self.open_surface(ui::Surface::Onboarding)]
            }
            Message::ReopenOnboarding => {
                if let Err(error) = self.cfg.update(|cfg| cfg.onboarding_complete = false) {
                    tracing::warn!(%error, "persisting the onboarding flag failed");
                }
                self.onboarding = ui::OnboardingUi::from_config(&self.cfg.snapshot());
                vec![self.open_surface(ui::Surface::Onboarding)]
            }
            Message::Key(event) => self.handle_key(event),
            Message::Onboarding(action) => crate::ui_onboarding::update(self, action),
            Message::Settings(action) => crate::ui_settings::update(self, action),
        }
    }

    fn handle_key(&mut self, event: iced::keyboard::Event) -> Vec<Task<Message>> {
        let Some(captured) = ui::capture_chord(
            &event,
            &mut self.onboarding.capture,
            &mut self.settings.capture,
        ) else {
            return Vec::new();
        };

        match captured {
            ui::CaptureTarget::Onboarding(chord) => {
                self.onboarding.hotkey = chord;
            }
            ui::CaptureTarget::Settings(chord) => {
                self.settings.hotkey_draft = Some(chord);
                self.settings.capture.reset();
            }
        }

        Vec::new()
    }

    /// Handle one background event.
    pub fn handle_event(&mut self, event: AppEvent) -> Vec<Task<Message>> {
        let now = Instant::now();

        match event {
            AppEvent::Hotkey(hotkey::HotkeyEvent::Press) => {
                if self.engine_info.is_none() {
                    return self.require_model();
                }
                let effects = self.machine.hotkey(hotkey::HotkeyEvent::Press, now);
                self.run(effects)
            }
            AppEvent::Hotkey(hotkey::HotkeyEvent::Release) => {
                let effects = self.machine.hotkey(hotkey::HotkeyEvent::Release, now);
                self.run(effects)
            }
            AppEvent::Chunk { generation, chunk } => {
                if generation != self.chunk_generation {
                    // A capture that was already stopped; its tail must not
                    // leak into the current dictation.
                    return Vec::new();
                }
                let effects = self.machine.chunk(&chunk);
                self.run(effects)
            }
            AppEvent::Eng(event) => self.handle_engine(event),
            AppEvent::Tick => {
                let effects = self.machine.tick(now);
                let tasks = self.run(effects);
                if let Some((_, until)) = &self.notice
                    && now.saturating_duration_since(*until) >= NOTICE_TTL
                {
                    self.notice = None;
                    self.refresh_overlay(false);
                }
                if self.machine.is_active() {
                    self.refresh_overlay(false);
                }
                tasks
            }
            AppEvent::IpcOpenSettings => vec![self.open_surface(ui::Surface::Settings)],
            AppEvent::IpcFailed(error) => {
                tracing::warn!(
                    error,
                    "ipc server unavailable; reinstancing is not possible"
                );
                self.set_notice(format!("Second-launch forwarding is unavailable: {error}"));
                Vec::new()
            }
            AppEvent::Quit => vec![iced::exit()],
            AppEvent::SettingsSaved(config) => {
                tracing::info!(
                    model = config.model,
                    language = config.language.label(),
                    "settings applied"
                );
                self.refresh_checks();
                self.refresh_tray();
                self.set_notice("Settings saved — they apply to the next dictation".to_owned());
                Vec::new()
            }
            AppEvent::ModelProgress {
                id,
                received,
                total,
            } => {
                self.update_download(id, received, total);
                Vec::new()
            }
            AppEvent::ModelFinished { id, result } => self.finish_download(id, result),
            AppEvent::HotkeyBound(result) => {
                match result {
                    Ok(handle) => {
                        tracing::info!(
                            chord = handle.binding.chord,
                            source = handle.binding.source.label(),
                            trigger = handle.binding.trigger_description,
                            "hotkey bound"
                        );
                        self.spawn_hotkey_forwarder(&handle);
                        self.binding = Some(handle.binding.clone());
                        self.hotkey = Some(handle);
                        self.hotkey_error = None;
                    }
                    Err(error) => {
                        tracing::warn!(detail = error.detail, "hotkey binding failed");
                        self.hotkey = None;
                        self.binding = None;
                        self.hotkey_error = Some(error);
                    }
                }
                let source = self.binding.as_ref().map(|binding| binding.source);
                if let Err(error) = self.cfg.update(|cfg| cfg.hotkey_source = source) {
                    tracing::warn!(%error, "persisting the hotkey source failed");
                }
                self.refresh_checks();
                Vec::new()
            }
            AppEvent::PortalProbed(available) => {
                tracing::info!(available, "global shortcut portal probed");
                self.portal_available = Some(available);
                Vec::new()
            }
            AppEvent::TrayReady(tray) => {
                if tray.is_none() {
                    tracing::info!("continuing without a tray icon");
                }
                self.tray = tray;
                self.refresh_tray();
                Vec::new()
            }
        }
    }

    fn handle_engine(&mut self, event: engine::EngineEvent) -> Vec<Task<Message>> {
        let now = Instant::now();

        match event {
            engine::EngineEvent::Ready(info) => {
                tracing::info!(
                    backend = info.backend,
                    device = info.device,
                    fallback = info.fallback.as_deref().unwrap_or("-"),
                    "engine ready"
                );
                self.engine_info = Some(info.clone());
                self.engine_error = None;
                self.refresh_checks();
                self.refresh_tray();
                Vec::new()
            }
            engine::EngineEvent::Partial { text } => {
                let effects = self.machine.partial(text);
                self.run(effects)
            }
            engine::EngineEvent::Final { text, .. } => {
                let effects = self.machine.final_text(text, now);
                self.run(effects)
            }
            engine::EngineEvent::Queued => {
                let effects = self.machine.queued();
                self.run(effects)
            }
            engine::EngineEvent::Failed { message } => {
                tracing::error!(message, "engine failure (recording discarded)");
                self.engine_error = Some(message.clone());
                self.refresh_checks();
                let effects = self.machine.failed(message, now);
                self.run(effects)
            }
        }
    }

    /// Carry out the machine's effects in order.
    fn run(&mut self, effects: Vec<DictationEffect>) -> Vec<Task<Message>> {
        let mut tasks = Vec::new();

        for effect in effects {
            match effect {
                DictationEffect::StartCapture => {
                    self.capture = None;
                    self.chunk_generation = self.chunk_generation.wrapping_add(1);
                    let generation = self.chunk_generation;
                    let (audio_tx, audio_rx) = std::sync::mpsc::channel();
                    // One forwarder per recording: the generation travels with
                    // the event, so a stale capture's tail is dropped instead of
                    // being attributed to the next dictation.
                    spawn_pump("audio-pump", audio_rx, self.events.clone(), move |chunk| {
                        AppEvent::Chunk { generation, chunk }
                    });
                    match audio::start(audio_tx) {
                        Ok(capture) => {
                            self.last_capture = Some((capture.rate_in(), capture.channels()));
                            self.capture = Some(capture);
                        }
                        Err(error) => {
                            let message = format!("{error:#}");
                            tracing::error!(message, "microphone capture failed");
                            let effects = self.machine.failed(message, Instant::now());
                            tasks.extend(self.run(effects));
                        }
                    }
                }
                DictationEffect::StopCapture => {
                    if let Some(mut capture) = self.capture.take() {
                        capture.stop();
                    }
                }
                DictationEffect::BeginEngine => {
                    let language = self.cfg.snapshot().language;
                    self.engine.send(engine::EngineCmd::Start { language });
                }
                DictationEffect::FeedEngine(samples) => {
                    self.engine.send(engine::EngineCmd::Audio(samples));
                }
                DictationEffect::StopEngine => {
                    self.engine.send(engine::EngineCmd::Stop);
                }
                DictationEffect::Insert(text) => {
                    let outcome = self
                        .inserter
                        .insert(&text)
                        .map_err(|error| format!("{error:#}"));
                    let effects = self.machine.insert_finished(&outcome, Instant::now());
                    tasks.extend(self.run(effects));
                }
                DictationEffect::InsertDetached(text) => {
                    let chars = text.chars().count();
                    let outcome = self
                        .inserter
                        .insert(&text)
                        .map_err(|error| format!("{error:#}"));
                    match outcome {
                        Ok(inserter::InsertOutcome::Inserted) => {
                            tracing::info!(chars, "queued dictation inserted");
                        }
                        Ok(inserter::InsertOutcome::CopiedOnly { reason, .. }) => {
                            self.set_notice(format!(
                                "Auto-paste blocked: {reason} — transcript copied, press Ctrl+V"
                            ));
                        }
                        Err(error) => {
                            tracing::error!(error, "inserting a queued dictation failed");
                            self.set_notice(format!("Insertion failed: {error}"));
                        }
                    }
                }
                DictationEffect::OverlayChanged => {
                    self.refresh_tray();
                    self.refresh_overlay(true);
                }
            }
        }

        tasks
    }

    /// The overlay frame for the current state, or `None` when it must be hidden.
    pub fn frame(&self) -> Option<overlay::OverlayFrame> {
        let now = Instant::now();

        let (state, elapsed, level, text) = match self.machine.state() {
            DictationState::Recording => (
                if self.machine.is_queued() {
                    overlay::OverlayState::Queued
                } else {
                    overlay::OverlayState::Recording
                },
                self.machine.elapsed(now),
                self.machine.level(),
                self.machine.partial_text().to_owned(),
            ),
            DictationState::AwaitingFinal => (
                overlay::OverlayState::Transcribing,
                Duration::ZERO,
                0.0,
                self.machine.partial_text().to_owned(),
            ),
            DictationState::Inserting => (
                overlay::OverlayState::Inserting,
                Duration::ZERO,
                0.0,
                self.machine.partial_text().to_owned(),
            ),
            DictationState::NoSpeech => (
                overlay::OverlayState::NoSpeech,
                Duration::ZERO,
                0.0,
                String::new(),
            ),
            DictationState::Copied(reason) => (
                overlay::OverlayState::Notice(format!(
                    "Auto-paste blocked: {reason} — transcript copied, press Ctrl+V"
                )),
                Duration::ZERO,
                0.0,
                String::new(),
            ),
            DictationState::Failed(message) => (
                overlay::OverlayState::Error(message.clone()),
                Duration::ZERO,
                0.0,
                String::new(),
            ),
            DictationState::Idle => {
                if let Some(download) = &self.download
                    && download.is_running()
                {
                    (
                        overlay::OverlayState::Notice(format!(
                            "Downloading {} ({}), quality note: {} — now {:.0}%",
                            download.entry.label,
                            download.entry.approx_size_label,
                            download.entry.note,
                            download.percent()
                        )),
                        Duration::ZERO,
                        0.0,
                        String::new(),
                    )
                } else if let Some((notice, _)) = &self.notice {
                    (
                        overlay::OverlayState::Notice(notice.clone()),
                        Duration::ZERO,
                        0.0,
                        String::new(),
                    )
                } else {
                    return None;
                }
            }
        };

        Some(overlay::OverlayFrame {
            state,
            elapsed,
            level,
            text,
        })
    }

    /// Push the current frame to the overlay presenter (FR-011).
    pub fn refresh_overlay(&mut self, force: bool) {
        let Some(frame) = self.frame() else {
            if self.overlay_visible {
                if let Err(error) = self.overlay.hide() {
                    tracing::warn!(%error, "hiding the overlay failed");
                }
                self.overlay_visible = false;
                self.last_overlay_draw = None;
            }
            return;
        };

        let due = self
            .last_overlay_draw
            .is_none_or(|last| last.elapsed() >= Duration::from_millis(100));

        let result = if !self.overlay_visible {
            self.overlay.show(&frame)
        } else if force || due {
            self.overlay.draw(&frame)
        } else {
            return;
        };

        match result {
            Ok(()) => {
                self.overlay_visible = true;
                self.last_overlay_draw = Some(Instant::now());
            }
            Err(error) => {
                tracing::warn!(%error, "overlay presentation failed; continuing without the indicator");
                self.overlay_visible = false;
                self.last_overlay_draw = None;
            }
        }
    }

    /// Show a transient notice in the overlay (download progress, save note, …).
    pub fn set_notice(&mut self, text: String) {
        self.notice = Some((text, Instant::now()));
        self.refresh_overlay(true);
    }

    /// The model is not loaded yet: start (or report) the download instead of
    /// recording into nothing (FR-003, Scenario 1.2).
    fn require_model(&mut self) -> Vec<Task<Message>> {
        if let Some(download) = &self.download
            && download.is_running()
        {
            self.set_notice(format!(
                "Downloading {} … {:.0}%",
                download.entry.label,
                download.percent()
            ));
            return vec![];
        }

        let entry = self.model_entry;
        let task = self.start_download(entry);
        self.set_notice(format!(
            "{} ({}), quality note: {} — downloading now, dictation works once it is ready",
            entry.label, entry.approx_size_label, entry.note
        ));
        vec![task]
    }

    /// Kick off a resumable, checksum-verified download (FR-003).
    pub fn start_download(&mut self, entry: &'static models::ModelEntry) -> Task<Message> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.download = Some(DownloadState {
            entry,
            cancel: Arc::clone(&cancel),
            received: 0,
            total: entry.size_bytes,
            error: None,
        });

        let progress_events = self.events.clone();
        let progress: Arc<dyn Fn(u64, u64) + Send + Sync> = Arc::new(move |received, total| {
            let _ = progress_events.send(AppEvent::ModelProgress {
                id: entry.id,
                received,
                total,
            });
        });

        let target = entry;
        Task::perform(
            async move {
                models::ensure(target, cancel, progress)
                    .await
                    .map_err(|error| format!("{error:#}"))
            },
            move |result| {
                Message::Event(AppEvent::ModelFinished {
                    id: target.id,
                    result,
                })
            },
        )
    }

    /// Cancel a running download, keeping the partial file for a later resume.
    pub fn cancel_download(&mut self) {
        if let Some(download) = &self.download {
            download.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn update_download(&mut self, id: &'static str, received: u64, total: u64) {
        let Some(download) = self.download.as_mut() else {
            return;
        };
        if download.entry.id != id {
            return;
        }

        download.received = received;
        download.total = total;

        if self.machine.state() == &DictationState::Idle {
            self.refresh_overlay(false);
        }
    }

    fn finish_download(
        &mut self,
        id: &'static str,
        result: Result<PathBuf, String>,
    ) -> Vec<Task<Message>> {
        let Some(download) = self.download.as_mut() else {
            return Vec::new();
        };
        if download.entry.id != id {
            return Vec::new();
        }

        match result {
            Ok(path) => {
                let entry = download.entry;
                self.download = None;
                self.model_entry = entry;
                self.model_path = path.clone();
                self.onboarding.model_ready = true;

                let backend = self.cfg.snapshot().backend;
                self.engine
                    .send(engine::EngineCmd::LoadModel(path, backend));
                self.set_notice(format!(
                    "{} is ready — press the hotkey to dictate",
                    entry.label
                ));
            }
            Err(error) => {
                let cancelled = error.contains("cancelled");
                if let Some(download) = self.download.as_mut() {
                    download.error = Some(error.clone());
                }
                if cancelled {
                    tracing::info!(
                        "model download cancelled; the partial file is kept for a resume"
                    );
                    self.refresh_overlay(false);
                } else {
                    tracing::error!(error, "model download failed");
                    self.set_notice(error);
                }
            }
        }

        Vec::new()
    }

    /// Rebuild the system-check list (FR-014/FR-022).
    pub fn refresh_checks(&mut self) {
        let config = self.cfg.snapshot();
        let state = diagnostics::State {
            binding: self.binding.clone(),
            hotkey_error: self.hotkey_error.clone(),
            device_ok: hotkey::device_available(),
            engine: self
                .engine_info
                .as_ref()
                .or(self.probe_info.as_ref())
                .cloned(),
            engine_error: self
                .engine_error
                .clone()
                .or_else(|| self.probe_error.clone()),
            overlay_backend: self.overlay_backend,
            overlay_reason: self.overlay_reason.clone(),
        };
        self.checks = diagnostics::collect(&config, &state);
    }

    /// Reflect the dictation state in the tray tooltip (FR-017).
    pub fn refresh_tray(&mut self) {
        let state = match self.machine.state() {
            DictationState::Idle => self.tray_state.clone(),
            DictationState::Recording => "Recording".to_owned(),
            DictationState::AwaitingFinal => "Transcribing".to_owned(),
            DictationState::Inserting => "Inserting".to_owned(),
            DictationState::NoSpeech => "No speech detected".to_owned(),
            DictationState::Copied(_) => "Copied to clipboard".to_owned(),
            DictationState::Failed(_) => "Error".to_owned(),
        };

        if let Some(tray) = &self.tray {
            tray.set(&state);
        }
    }

    /// Adopt a new model selection: load it when the file is present, otherwise
    /// start the verified download (FR-003, Scenario 1.2).
    pub fn adopt_model(&mut self, entry: &'static models::ModelEntry) -> Vec<Task<Message>> {
        self.model_entry = entry;
        self.model_path = models::path_for(entry).unwrap_or_default();

        if self.model_path.exists() {
            let backend = self.cfg.snapshot().backend;
            self.engine.send(engine::EngineCmd::LoadModel(
                self.model_path.clone(),
                backend,
            ));
            Vec::new()
        } else {
            vec![self.start_download(entry)]
        }
    }

    /// Re-probe every capability without restarting (FR-014, SC-006).
    pub fn recheck(&mut self) -> Vec<Task<Message>> {
        let (inserter, note) = inserter::select(Arc::clone(&self.clipboard));
        self.inserter = inserter;
        self.paste_note = note;

        let (probe_info, probe_error) = engine::probe_devices(self.cfg.snapshot().backend);
        self.probe_info = probe_info;
        self.probe_error = probe_error;
        self.refresh_checks();

        // Rebinding is only attempted while nothing is bound: on a portal
        // desktop every rebind would show the compositor's dialog.
        if self.hotkey.is_none() || self.hotkey_error.is_some() {
            let config = self.cfg.snapshot();
            return vec![bind_task(&config.hotkey, config.hotkey_source)];
        }

        Vec::new()
    }

    /// Close the window of `surface`, if it is open.
    pub fn close_surface(&mut self, surface: ui::Surface) -> Task<Message> {
        let open = self
            .windows
            .iter()
            .find(|(_, window_surface)| **window_surface == surface)
            .map(|(id, _)| *id);

        match open {
            Some(id) => {
                self.windows.remove(&id);
                window::close(id)
            }
            None => Task::none(),
        }
    }

    /// Where models live (FR-016).
    pub fn model_dir_display(&self) -> String {
        config::Config::models_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|error| format!("unavailable ({error:#})"))
    }

    /// Open (or focus) a surface, at most one window per surface kind.
    pub fn open_surface(&mut self, surface: ui::Surface) -> Task<Message> {
        if let Some((id, _)) = self.windows.iter().find(|(_, open)| **open == surface) {
            return window::gain_focus(*id);
        }

        if surface == ui::Surface::Settings {
            self.settings = ui::SettingsUi::from_config(&self.cfg.snapshot());
        }

        let (id, task) = window::open(ui::window_settings(&surface));
        self.windows.insert(id, surface);
        task.map(Message::WindowOpened)
    }

    fn spawn_hotkey_forwarder(&self, handle: &hotkey::HotkeyHandle) {
        // The receiver is moved into the forwarder; the handle stays in the app
        // purely as the keepalive that keeps the mechanism alive. Rebinding
        // replaces the handle and the old forwarder exits when its channel
        // closes.
        let Some(rx) = handle.take_receiver() else {
            return;
        };
        let tx = self.events.clone();
        spawn_pump("hotkey-pump", rx, tx, AppEvent::Hotkey);
    }

    /// Subscription set: event drain, close events, ticker, chord capture.
    pub fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            Subscription::run(event_stream),
            iced::window::close_events().map(Message::WindowClosed),
        ];

        if self.onboarding.capture.is_capturing() || self.settings.capture.is_capturing() {
            subscriptions.push(iced::keyboard::listen().map(Message::Key));
        }

        if self.machine.is_active() || self.notice.is_some() || self.download.is_some() {
            subscriptions.push(
                iced::time::every(Duration::from_millis(100))
                    .map(|_| Message::Event(AppEvent::Tick)),
            );
        }

        Subscription::batch(subscriptions)
    }
}

/// Start a fresh bind attempt and deliver the result as an event (FR-008/FR-009).
pub fn bind_task(chord: &str, preferred: Option<config::HotkeySource>) -> Task<Message> {
    let chord = chord.to_owned();
    Task::perform(
        async move { hotkey::bind(&chord, preferred).await },
        |result| Message::Event(AppEvent::HotkeyBound(result)),
    )
}

/// Async drain of the single event channel into iced messages.
fn event_stream() -> impl futures_util::Stream<Item = Message> {
    futures_util::stream::unfold((), |()| async move {
        let (_, receiver) = event_channel();
        let mut receiver = receiver.lock().await;
        receiver
            .recv()
            .await
            .map(|event| (Message::Event(event), ()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(len: usize) -> audio::Chunk {
        audio::Chunk {
            samples: vec![0.1; len],
            rms: 0.1,
        }
    }

    #[test]
    fn tap_starts_and_second_press_stops() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();

        let effects = machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        assert_eq!(
            effects,
            vec![
                DictationEffect::StartCapture,
                DictationEffect::BeginEngine,
                DictationEffect::OverlayChanged
            ]
        );
        assert_eq!(machine.state(), &DictationState::Recording);

        // Short release: the recording keeps running (tap semantics).
        assert!(
            machine
                .hotkey(hotkey::HotkeyEvent::Release, t0 + Duration::from_millis(80))
                .is_empty()
        );
        assert_eq!(machine.state(), &DictationState::Recording);

        // A stray release never stops a tap-started recording.
        assert!(
            machine
                .hotkey(
                    hotkey::HotkeyEvent::Release,
                    t0 + Duration::from_millis(200)
                )
                .is_empty()
        );
        assert_eq!(machine.state(), &DictationState::Recording);

        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        let effects = machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(2));
        assert_eq!(
            effects,
            vec![
                DictationEffect::StopCapture,
                DictationEffect::StopEngine,
                DictationEffect::OverlayChanged
            ]
        );
        assert_eq!(machine.state(), &DictationState::AwaitingFinal);
    }

    #[test]
    fn hold_to_talk_stops_on_release() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));

        let effects = machine.hotkey(
            hotkey::HotkeyEvent::Release,
            t0 + MIN_HOLD + Duration::from_millis(1),
        );
        assert!(effects.contains(&DictationEffect::StopEngine));
        assert_eq!(machine.state(), &DictationState::AwaitingFinal);
    }

    #[test]
    fn short_recordings_are_discarded_and_release_the_engine() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        machine.chunk(&chunk(16)); // ~1 ms of audio
        let effects = machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_millis(400));

        assert_eq!(
            effects,
            vec![
                DictationEffect::StopCapture,
                DictationEffect::StopEngine,
                DictationEffect::OverlayChanged
            ]
        );
        assert_eq!(machine.state(), &DictationState::Idle);

        // The engine's empty Final for that released stream must be ignored.
        assert!(
            machine
                .final_text(String::new(), t0 + Duration::from_millis(500))
                .is_empty()
        );
        assert_eq!(machine.state(), &DictationState::Idle);
    }

    #[test]
    fn empty_transcript_reports_no_speech_and_hides() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(1));
        machine.final_text("   ".to_owned(), t0 + Duration::from_secs(2));

        assert_eq!(machine.state(), &DictationState::NoSpeech);
        assert!(
            machine
                .tick(t0 + Duration::from_secs(2) + NO_SPEECH_TTL)
                .contains(&DictationEffect::OverlayChanged)
        );
        assert_eq!(machine.state(), &DictationState::Idle);
    }

    #[test]
    fn final_text_inserts_exactly_once() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(1));

        let effects = machine.final_text("Hello world.".to_owned(), t0 + Duration::from_secs(2));
        assert!(effects.contains(&DictationEffect::Insert("Hello world.".to_owned())));
        assert_eq!(machine.state(), &DictationState::Inserting);

        // A second Final for the same cycle must not insert again.
        assert!(
            machine
                .final_text("Hello world again.".to_owned(), t0 + Duration::from_secs(3))
                .is_empty()
        );

        machine.insert_finished(
            &Ok(inserter::InsertOutcome::Inserted),
            t0 + Duration::from_secs(4),
        );
        assert_eq!(machine.state(), &DictationState::Idle);
    }

    #[test]
    fn clipboard_only_outcome_states_the_blocked_path() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(1));
        machine.final_text("Hallo.".to_owned(), t0 + Duration::from_secs(2));

        let outcome = inserter::InsertOutcome::CopiedOnly {
            reason: "/dev/uinput (Permission denied (os error 13))".to_owned(),
            blocked_path: Some(PathBuf::from("/dev/uinput")),
        };
        machine.insert_finished(&Ok(outcome), t0 + Duration::from_secs(2));

        match machine.state() {
            DictationState::Copied(reason) => assert!(reason.contains("/dev/uinput")),
            other => panic!("expected the clipboard-only state, got {other:?}"),
        }

        machine.tick(t0 + COPIED_TTL + Duration::from_secs(3));
        assert_eq!(machine.state(), &DictationState::Idle);
    }

    #[test]
    fn engine_failure_discards_the_recording() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.partial("partial words".to_owned());

        machine.failed(
            "transcription failed: bad model".to_owned(),
            t0 + Duration::from_secs(1),
        );
        assert_eq!(
            machine.state(),
            &DictationState::Failed("transcription failed: bad model".to_owned())
        );
        assert!(
            machine.partial_text().is_empty(),
            "partial text must never be inserted after a failure"
        );
        // No insertion was requested.
        assert!(machine.final_text("late".to_owned(), t0).is_empty());
    }

    #[test]
    fn recording_cap_auto_stops() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);

        let total = u64::from(audio::TARGET_RATE) * audio::MAX_RECORDING_SECS;
        let mut fed = 0;
        let mut effects = Vec::new();
        while fed < total {
            effects = machine.chunk(&chunk(audio::CHUNK_SAMPLES));
            fed += audio::CHUNK_SAMPLES as u64;
        }

        assert!(effects.contains(&DictationEffect::StopEngine));
        assert_eq!(machine.state(), &DictationState::AwaitingFinal);
    }

    #[test]
    fn queued_state_is_shown_until_text_arrives() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);

        machine.queued();
        assert!(machine.is_queued());
        machine.partial("first words".to_owned());
        assert!(!machine.is_queued());
        assert_eq!(machine.partial_text(), "first words");
    }

    #[test]
    fn a_final_from_an_earlier_cycle_is_inserted_without_disturbing_the_new_recording() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();

        // Cycle A records and stops; its Final is still owed.
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(1));
        assert_eq!(machine.state(), &DictationState::AwaitingFinal);

        // Cycle B starts while A is finalising (Scenario 4.6).
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(2));
        assert_eq!(machine.state(), &DictationState::Recording);

        // A's transcript arrives during B's recording.
        let effects = machine.final_text("First sentence.".to_owned(), t0 + Duration::from_secs(3));
        assert_eq!(
            effects,
            vec![DictationEffect::InsertDetached(
                "First sentence.".to_owned()
            )]
        );
        assert_eq!(
            machine.state(),
            &DictationState::Recording,
            "the queued dictation's insertion must not end B's recording"
        );

        // B resolves through the normal path, exactly once.
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(4));
        let effects =
            machine.final_text("Second sentence.".to_owned(), t0 + Duration::from_secs(5));
        assert!(effects.contains(&DictationEffect::Insert("Second sentence.".to_owned())));
        assert_eq!(machine.state(), &DictationState::Inserting);
    }

    #[test]
    fn an_empty_final_from_an_earlier_cycle_inserts_nothing() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(1));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(2));
        assert_eq!(machine.state(), &DictationState::Recording);

        assert!(
            machine
                .final_text("   ".to_owned(), t0 + Duration::from_secs(3))
                .is_empty()
        );
        assert_eq!(machine.state(), &DictationState::Recording);

        // The current cycle's own Final is still routed normally.
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(4));
        let effects = machine.final_text("Now.".to_owned(), t0 + Duration::from_secs(5));
        assert!(effects.contains(&DictationEffect::Insert("Now.".to_owned())));
    }

    #[test]
    fn an_engine_failure_clears_the_pending_finals() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(1));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(2));

        machine.failed(
            "transcription failed: broken".to_owned(),
            t0 + Duration::from_secs(3),
        );
        assert_eq!(
            machine.state(),
            &DictationState::Failed("transcription failed: broken".to_owned())
        );

        // A late Final for the discarded cycle must not be treated as detached.
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(4));
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(5));
        let effects = machine.final_text("Fresh.".to_owned(), t0 + Duration::from_secs(6));
        assert!(effects.contains(&DictationEffect::Insert("Fresh.".to_owned())));
        assert_eq!(machine.state(), &DictationState::Inserting);
    }

    #[test]
    fn a_new_press_while_finalising_starts_a_new_recording() {
        let mut machine = DictationMachine::new();
        let t0 = Instant::now();
        machine.hotkey(hotkey::HotkeyEvent::Press, t0);
        machine.chunk(&chunk(audio::MIN_RECORDING_SAMPLES as usize));
        machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(1));
        assert_eq!(machine.state(), &DictationState::AwaitingFinal);

        let effects = machine.hotkey(hotkey::HotkeyEvent::Press, t0 + Duration::from_secs(2));
        assert!(effects.contains(&DictationEffect::StartCapture));
        assert_eq!(machine.state(), &DictationState::Recording);
        assert!(machine.partial_text().is_empty());
    }
}
