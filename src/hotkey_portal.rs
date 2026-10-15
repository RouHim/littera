//! Global-shortcut binding through `org.freedesktop.portal.GlobalShortcuts`.
//!
//! The portal is the preferred mechanism: the compositor owns the chord, shows
//! its own binding dialog, reports the trigger it actually bound, and delivers
//! both the press and the release (hold-to-talk).
//!
//! `GlobalShortcuts::new()` fails with [`ashpd::Error::PortalNotFound`] when the
//! running portal does not implement the interface (the reference machine's
//! case), which is what makes [`PortalError::Unavailable`] detectable without
//! ever opening a dialog.

use std::sync::mpsc::{Receiver, Sender};

use ashpd::desktop::global_shortcuts::{
    Activated, BindShortcutsOptions, Deactivated, GlobalShortcuts, NewShortcut,
};
use ashpd::desktop::{CreateSessionOptions, Session};

use crate::hotkey::HotkeyEvent;

/// Application-provided id of the single shortcut Lingua binds.
const SHORTCUT_ID: &str = "dictate";

/// User-readable text shown by the compositor's own binding dialog.
const SHORTCUT_DESCRIPTION: &str = "Dictate";

/// Reported when the compositor bound no trigger at all.
pub const NOTHING_BOUND: &str = "nothing bound";

/// Portal interface required for this mechanism.
const PORTAL_INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

/// Why the portal path is not usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalError {
    /// The running portal does not implement `GlobalShortcuts`.
    Unavailable,
    /// The portal answered but did not bind the shortcut (cancelled, or the
    /// compositor bound nothing).
    Refused(String),
    /// The D-Bus conversation itself failed.
    Failed(String),
}

impl std::fmt::Display for PortalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(
                f,
                "the running xdg-desktop-portal does not implement {PORTAL_INTERFACE}"
            ),
            Self::Refused(reason) => {
                write!(f, "the shortcut portal did not bind the shortcut: {reason}")
            }
            Self::Failed(reason) => write!(f, "shortcut portal error: {reason}"),
        }
    }
}

impl std::error::Error for PortalError {}

/// Whether the portal interface is present. Opens no dialog.
pub async fn available() -> bool {
    GlobalShortcuts::new().await.is_ok()
}

/// A live portal binding.
pub struct PortalBinding {
    /// The trigger the compositor actually bound (may differ from the request).
    pub trigger_description: String,
    /// Press/release events for that trigger.
    pub rx: Receiver<HotkeyEvent>,
    /// Keeps the portal session and its forwarding task alive.
    pub(crate) keepalive: PortalKeepAlive,
}

/// Dropping this retires the forwarder task (the `oneshot` sender drops, so the
/// task stops waiting), which closes the portal session on its way out.
pub(crate) struct PortalKeepAlive {
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl PortalKeepAlive {
    /// Whether the forwarding task still holds the session: a dropped receiver
    /// means the task ended, so the shortcut stopped delivering events.
    pub(crate) fn is_alive(&self) -> bool {
        !self._stop.is_closed()
    }
}

/// Create a portal session, bind `chord` as the preferred trigger, and start
/// forwarding its press/release signals.
pub async fn bind(chord: &str) -> Result<PortalBinding, PortalError> {
    let shortcuts = GlobalShortcuts::new()
        .await
        .map_err(|_| PortalError::Unavailable)?;

    let session = shortcuts
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(classify)?;

    let request = shortcuts
        .bind_shortcuts(
            &session,
            &[NewShortcut::new(SHORTCUT_ID, SHORTCUT_DESCRIPTION).preferred_trigger(chord)],
            None,
            BindShortcutsOptions::default(),
        )
        .await
        .map_err(classify)?;

    let bind_response = request.response().map_err(classify)?;
    let bound_trigger = bind_response
        .shortcuts()
        .iter()
        .find(|shortcut| shortcut.id() == SHORTCUT_ID)
        .map(|shortcut| shortcut.trigger_description().trim().to_owned())
        .filter(|description| !description.is_empty());

    let Some(trigger_description) = bound_trigger else {
        tracing::info!(
            chord,
            "the shortcut portal bound no trigger for this chord; the device listener will take over"
        );
        close_session(&session).await;
        return Err(PortalError::Refused(NOTHING_BOUND.to_owned()));
    };

    // Subscribe before spawning the forwarder so no activation can be missed.
    let activated = shortcuts.receive_activated().await.map_err(classify)?;
    let deactivated = shortcuts.receive_deactivated().await.map_err(classify)?;

    let (events, rx) = std::sync::mpsc::channel();
    let (stop, stop_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::runtime::Handle::try_current().map_err(|err| {
        PortalError::Failed(format!(
            "no tokio runtime is active, cannot forward portal shortcut events: {err}"
        ))
    })?;
    handle.spawn(forward(session, events, activated, deactivated, stop_rx));

    tracing::info!(
        chord,
        trigger = %trigger_description,
        "shortcut portal session bound"
    );
    Ok(PortalBinding {
        trigger_description,
        rx,
        keepalive: PortalKeepAlive { _stop: stop },
    })
}

/// Forward portal activations to the chord event channel until the binding is
/// dropped or the signals stop arriving.
async fn forward(
    session: Session<GlobalShortcuts>,
    events: Sender<HotkeyEvent>,
    activated: impl futures_util::Stream<Item = Activated>,
    deactivated: impl futures_util::Stream<Item = Deactivated>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) {
    use futures_util::StreamExt;

    let mut activated = Box::pin(activated);
    let mut deactivated = Box::pin(deactivated);

    loop {
        tokio::select! {
            // The handle was dropped: retire.
            _ = &mut stop => break,
            activation = activated.next() => match activation {
                Some(_) => {
                    if events.send(HotkeyEvent::Press).is_err() {
                        break;
                    }
                }
                None => {
                    tracing::warn!(
                        "the shortcut portal stopped sending Activated signals; the portal hotkey is disabled for this session"
                    );
                    break;
                }
            },
            deactivation = deactivated.next() => match deactivation {
                Some(_) => {
                    if events.send(HotkeyEvent::Release).is_err() {
                        break;
                    }
                }
                None => {
                    tracing::warn!(
                        "the shortcut portal stopped sending Deactivated signals; the portal hotkey is disabled for this session"
                    );
                    break;
                }
            },
        }
    }

    close_session(&session).await;
}

async fn close_session(session: &Session<GlobalShortcuts>) {
    if let Err(err) = session.close().await {
        tracing::debug!(error = %err, "closing the shortcut portal session failed");
    }
}

/// Map an `ashpd` error onto the portal-path taxonomy used by `hotkey::bind`.
fn classify(err: ashpd::Error) -> PortalError {
    match err {
        ashpd::Error::PortalNotFound(_) => PortalError::Unavailable,
        ashpd::Error::Response(response_error) => PortalError::Refused(response_error.to_string()),
        other => PortalError::Failed(other.to_string()),
    }
}
