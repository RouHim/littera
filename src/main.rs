//! Lingua — local voice dictation for Linux/Wayland.
//!
//! `main` resolves the two verification flags (`--diagnostics`,
//! `--audio-file`) before anything else, otherwise it enforces single-instance
//! behaviour and runs the iced daemon (FR-015).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod app;
mod audio;
mod clipboard;
mod config;
mod diagnostics;
mod engine;
mod hotkey;
mod hotkey_device;
mod hotkey_portal;
mod inserter;
mod ipc;
mod logging;
mod model_catalog;
mod models;
mod overlay;
mod overlay_render;
mod overlay_wayland;
mod overlay_x11;
mod stream_lease;
mod tray;
mod ui;
mod ui_icons;
mod ui_onboarding;
mod ui_problems;
mod ui_settings;
mod vulkan_icd;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::app::App;
use crate::config::Config;

/// The only two command-line flags the binary accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cli {
    /// Print the system checks and exit 0 (FR-014 verification).
    Diagnostics,
    /// Run the production pipeline on a WAV file instead of the microphone.
    AudioFile {
        path: PathBuf,
        print_transcript: bool,
        latency: bool,
    },
    /// Run the daemon.
    Run,
}

fn main() -> Result<()> {
    logging::init().context("initialising logging failed")?;

    match parse_args(std::env::args().skip(1)) {
        Ok(Cli::Diagnostics) => diagnostics_cli(),
        Ok(Cli::AudioFile {
            path,
            print_transcript,
            latency,
        }) => audio_file_cli(&path, print_transcript, latency),
        Ok(Cli::Run) => run_daemon(),
        Err(error) => {
            emit(&format!("{error:#}"));
            emit(
                "usage: lingua [--diagnostics | --audio-file <WAV> [--print-transcript] [--latency]]",
            );
            std::process::exit(2);
        }
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Cli> {
    let mut cli = Cli::Run;
    let mut args = args.peekable();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--diagnostics" => cli = Cli::Diagnostics,
            "--audio-file" => {
                let path = args
                    .next()
                    .context("--audio-file needs the path of a 16 kHz mono PCM WAV")?;
                let (print_transcript, latency) = match cli {
                    Cli::AudioFile {
                        print_transcript,
                        latency,
                        ..
                    } => (print_transcript, latency),
                    _ => (false, false),
                };
                cli = Cli::AudioFile {
                    path: PathBuf::from(path),
                    print_transcript,
                    latency,
                };
            }
            "--print-transcript" => match &mut cli {
                Cli::AudioFile {
                    print_transcript, ..
                } => *print_transcript = true,
                _ => bail!("--print-transcript only makes sense together with --audio-file"),
            },
            "--latency" => match &mut cli {
                Cli::AudioFile { latency, .. } => *latency = true,
                _ => bail!("--latency only makes sense together with --audio-file"),
            },
            other => bail!("unknown argument '{other}'"),
        }
    }

    Ok(cli)
}

/// `lingua --diagnostics`: print the four system checks and exit 0.
fn diagnostics_cli() -> Result<()> {
    let config = load_config_or_default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime failed")?;

    let bind_result = runtime.block_on(hotkey::bind(&config.hotkey, config.hotkey_source));
    let device_ok = hotkey::device_available();

    let hotkey_check = match &bind_result {
        Ok(handle) => diagnostics::hotkey_check(Some(&handle.binding), None, true),
        Err(error) => diagnostics::hotkey_check(None, Some(error), device_ok),
    };

    let (engine_info, engine_error) = engine::probe_devices(config.backend);
    let (overlay_backend, overlay_reason) = overlay::select_backend();

    let checks = [
        hotkey_check,
        diagnostics::paste_check(),
        diagnostics::gpu_check_probed(&config, engine_info.as_ref(), engine_error.as_deref()),
        diagnostics::overlay_check(overlay_backend, &overlay_reason),
    ];

    for check in &checks {
        let marker = if check.ok { "ok" } else { "fail" };
        emit(&format!("[{marker}] {}: {}", check.name, check.detail));
        if let Some(remediation) = &check.remediation {
            emit(&format!("       remediation: {remediation}"));
        }
    }

    // FR-022: the effective model, language and backend preference, so the one
    // command answers support questions without the settings window.
    let selection =
        model_catalog::artifact(&config.model).unwrap_or_else(model_catalog::default_selection);
    let model_path = models::path_for(selection.artifact)?;
    emit(&format!(
        "model: {} — {} ({})",
        selection.artifact.id,
        model_path.display(),
        if model_path.exists() {
            "on disk"
        } else {
            "not downloaded"
        }
    ));
    emit(&format!(
        "language: {} ({})",
        config.language.label(),
        config.language.display_name()
    ));
    emit(&format!("backend preference: {}", config.backend.label()));

    Ok(())
}

/// `lingua --audio-file <WAV>`: the production pipeline on a file.
fn audio_file_cli(path: &Path, print_transcript: bool, latency: bool) -> Result<()> {
    let config = load_config_or_default();
    let samples = audio::read_wav_16k_mono(path)?;
    let duration = samples.len() as f32 / audio::TARGET_RATE as f32;
    tracing::info!(file = %path.display(), seconds = duration, "loaded wav for offline dictation");

    let selection =
        model_catalog::artifact(&config.model).unwrap_or_else(model_catalog::default_selection);
    let model_path = models::path_for(selection.artifact)?;
    if !model_path.exists() {
        // Same behaviour as pressing the hotkey for the first time: fetch and
        // verify the artifact (FR-003), then continue with the file.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("starting the async runtime failed")?;
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress: std::sync::Arc<dyn Fn(u64, u64) + Send + Sync> =
            std::sync::Arc::new(|received: u64, total: u64| {
                tracing::info!(received, total, "downloading model");
            });
        tracing::info!(
            model = selection.artifact.id,
            size = models::human_size(selection.artifact.size_bytes),
            path = %model_path.display(),
            "downloading the model before transcribing"
        );
        runtime
            .block_on(models::ensure(selection.artifact, cancel, progress))
            .context("downloading the model failed")?;
    }

    let (events_tx, events_rx) = std::sync::mpsc::channel();
    let engine = engine::spawn(selection, model_path, config.backend, events_tx);

    let clipboard = clipboard::Clipboard::new().context("owning the Wayland clipboard failed")?;
    let clipboard = std::sync::Arc::new(clipboard);
    let (inserter, paste_note) = inserter::select(std::sync::Arc::clone(&clipboard));
    if let Some(note) = &paste_note {
        tracing::warn!(
            note,
            "auto-paste unavailable; the transcript is copied only"
        );
    }

    let started = Instant::now();
    let mut first_partial_ms = None;
    let mut transcript = String::new();
    let mut language: Option<String> = None;

    engine.send(engine::EngineCmd::Start {
        language: config.language,
    });

    let chunks = audio::chunks(&samples);

    // Feed the audio at live-capture pace so partials appear the way they do
    // for a real microphone stream.
    for (index, chunk) in chunks.iter().enumerate() {
        engine.send(engine::EngineCmd::Audio(chunk.samples.clone()));

        let target = std::time::Duration::from_millis(100 * (index as u64 + 1));
        let elapsed = started.elapsed();
        if target > elapsed {
            std::thread::sleep(target - elapsed);
        }

        while let Ok(event) = events_rx.try_recv() {
            match event {
                engine::EngineEvent::Partial { text } => {
                    if first_partial_ms.is_none() && !text.trim().is_empty() {
                        first_partial_ms = Some(started.elapsed().as_millis());
                    }
                }
                engine::EngineEvent::Failed { message } => bail!("{message}"),
                engine::EngineEvent::LoadFailed { message, .. } => bail!("{message}"),
                _ => {}
            }
        }
    }

    let stop_sent = Instant::now();
    engine.send(engine::EngineCmd::Stop);

    let mut final_ms = None;
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    while Instant::now() < deadline {
        match events_rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(engine::EngineEvent::Final {
                text,
                language: used,
            }) => {
                final_ms = Some(stop_sent.elapsed().as_millis());
                transcript = text;
                language = used;
                break;
            }
            Ok(engine::EngineEvent::Partial { text }) => {
                if first_partial_ms.is_none() && !text.trim().is_empty() {
                    first_partial_ms = Some(started.elapsed().as_millis());
                }
            }
            Ok(engine::EngineEvent::Failed { message }) => bail!("{message}"),
            Ok(engine::EngineEvent::LoadFailed { message, .. }) => bail!("{message}"),
            Ok(_) => {}
            // A quiet window is not a failure: finalizing a long recording can
            // take longer than the poll interval (Moonshine re-decodes its whole
            // prefix), so only the deadline ends the wait.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                bail!("the engine worker stopped before producing a transcript");
            }
        }
    }
    if final_ms.is_none() {
        bail!("the engine produced no final transcript within 60 s");
    }

    let outcome = inserter.insert(&transcript)?;
    tracing::info!(
        inserted = matches!(outcome, inserter::InsertOutcome::Inserted),
        chars = transcript.chars().count(),
        "dictation finished"
    );

    if print_transcript {
        emit(&transcript);
        // The language the model reported for this recording (Spec: Dictation
        // session — "the language actually used"): the detected tag, or the
        // configured hint when the model tagged nothing.
        match language {
            Some(code) => emit(&format!("language={code}")),
            None => emit("language=unknown"),
        }
    }

    if latency {
        match first_partial_ms {
            Some(ms) => emit(&format!("first_partial_ms={ms}")),
            None => emit("first_partial_ms=never"),
        }
        match final_ms {
            Some(ms) => emit(&format!("final_ms={ms}")),
            None => emit("final_ms=never"),
        }
    }

    Ok(())
}

fn run_daemon() -> Result<()> {
    // Single instance: a second launch forwards "open settings" and exits 0
    // (FR-015, Scenario 3.1).
    if ipc::notify_running_instance(ipc::Request::OpenSettings)
        .context("checking for a running instance failed")?
    {
        tracing::info!("another instance is running; asked it to open settings");
        return Ok(());
    }

    tracing::info!("starting lingual daemon");
    iced::daemon(App::boot, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .font(ui::FONT_VALUE)
        .font(ui::FONT_LEGEND)
        .run()
        .context("running the iced daemon failed")
}

fn load_config_or_default() -> Config {
    match Config::load() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%error, "settings could not be loaded; using defaults");
            Config::default()
        }
    }
}

/// Write one line of user-facing CLI output without `println!` (FR-019/FR-020).
fn emit(line: &str) {
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{line}").is_err() {
        tracing::warn!("writing to stdout failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parses_the_diagnostics_flag() {
        assert_eq!(parse(&["--diagnostics"]).ok(), Some(Cli::Diagnostics));
    }

    #[test]
    fn parses_the_audio_file_flag_with_modifiers() {
        assert_eq!(
            parse(&["--audio-file", "s.wav", "--print-transcript", "--latency"]).ok(),
            Some(Cli::AudioFile {
                path: PathBuf::from("s.wav"),
                print_transcript: true,
                latency: true,
            })
        );
        assert_eq!(
            parse(&["--audio-file", "s.wav"]).ok(),
            Some(Cli::AudioFile {
                path: PathBuf::from("s.wav"),
                print_transcript: false,
                latency: false,
            })
        );
    }

    #[test]
    fn rejects_unknown_or_misplaced_arguments() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--audio-file"]).is_err());
        assert!(parse(&["--print-transcript"]).is_err());
        assert!(parse(&[]).is_ok(), "no arguments means the daemon");
    }
}
