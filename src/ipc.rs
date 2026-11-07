//! Single-instance IPC over an XDG runtime-directory socket (FR-015).
//!
//! The running instance listens on `$XDG_RUNTIME_DIR/littera/ipc.sock`; a second
//! launch connects, sends one request line, and exits with status 0.

use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::AppEvent;

const SOCKET_DIR_MODE: u32 = 0o700;

/// A request a second instance can send to the running one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// The user launched the application again: surface the settings window.
    OpenSettings,
}

impl Request {
    /// The wire representation: one JSON object per line.
    pub fn encode(self) -> String {
        match self {
            Request::OpenSettings => "{\"cmd\":\"open_settings\"}\n".to_owned(),
        }
    }

    /// Parse one request line; unknown or malformed lines are ignored.
    pub fn parse(line: &str) -> Option<Request> {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        match value.get("cmd")?.as_str()? {
            "open_settings" => Some(Request::OpenSettings),
            _ => None,
        }
    }
}

/// `$XDG_RUNTIME_DIR/littera/ipc.sock`.
pub fn socket_path() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context(
        "cannot determine the XDG runtime directory ($XDG_RUNTIME_DIR unset — is this a Wayland session?)",
    )?;
    Ok(PathBuf::from(runtime).join("littera").join("ipc.sock"))
}

/// Tell a running instance about `request`.
///
/// Returns `Ok(true)` when a live instance accepted the request and `Ok(false)`
/// when no socket (or only a stale one) is present, i.e. this process is the
/// first instance.
pub fn notify_running_instance(request: Request) -> Result<bool> {
    let path = socket_path()?;

    match std::os::unix::net::UnixStream::connect(&path) {
        Ok(mut stream) => {
            stream
                .write_all(request.encode().as_bytes())
                .with_context(|| {
                    format!(
                        "talking to the running instance at {} failed",
                        path.display()
                    )
                })?;
            stream.flush().ok();
            Ok(true)
        }
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Err(error) => {
            Err(error).with_context(|| format!("connecting to {} failed", path.display()))
        }
    }
}

/// Serve requests for the lifetime of the process.
///
/// Callers must have established that no live instance holds the socket; a stale
/// socket file is removed before binding.
pub async fn serve(events: UnboundedSender<AppEvent>) -> Result<()> {
    let path = socket_path()?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating {} failed", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(SOCKET_DIR_MODE))
            .with_context(|| format!("restricting {} to the current user failed", dir.display()))?;
    }

    match std::fs::remove_file(&path) {
        Ok(()) => tracing::info!(path = %path.display(), "removed stale ipc socket"),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("removing the stale socket {} failed", path.display()));
        }
    }

    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {} failed", path.display()))?;
    tracing::info!(path = %path.display(), "ipc listening");

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "accepting an ipc connection failed");
                continue;
            }
        };

        let events = events.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, events).await {
                tracing::warn!(%error, "handling an ipc connection failed");
            }
        });
    }
}

async fn handle_connection(stream: UnixStream, events: UnboundedSender<AppEvent>) -> Result<()> {
    let mut lines = BufReader::new(stream).lines();

    while let Some(line) = lines.next_line().await? {
        match Request::parse(&line) {
            Some(Request::OpenSettings) => {
                tracing::info!("second instance requested the settings window");
                if events.send(AppEvent::IpcOpenSettings).is_err() {
                    return Ok(());
                }
            }
            None => tracing::debug!(line, "ignoring unknown ipc request"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_settings_roundtrip() {
        let encoded = Request::OpenSettings.encode();
        assert_eq!(encoded, "{\"cmd\":\"open_settings\"}\n");
        assert_eq!(
            Request::parse(encoded.trim_end()),
            Some(Request::OpenSettings)
        );
    }

    #[test]
    fn tolerates_whitespace_and_unknown_commands() {
        assert_eq!(
            Request::parse("  {\"cmd\": \"open_settings\"}  "),
            Some(Request::OpenSettings)
        );
        assert_eq!(Request::parse("{\"cmd\":\"quit\"}"), None);
        assert_eq!(Request::parse("not json"), None);
        assert_eq!(Request::parse("{\"nope\":1}"), None);
    }
}
