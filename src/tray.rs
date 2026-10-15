//! StatusNotifierItem tray icon (FR-017).
//!
//! Without a tray host the application keeps working; the failure is only
//! logged (Scenario 3.3).

use ksni::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::AppEvent;

/// A cheap, `Send` handle for pushing dictation-state text to the tray tooltip.
#[derive(Debug, Clone)]
pub struct TrayState(tokio::sync::mpsc::UnboundedSender<String>);

impl TrayState {
    /// Update the state shown in the tray tooltip. Never blocks.
    pub fn set(&self, state: &str) {
        let _ = self.0.send(state.to_owned());
    }
}

/// The tray item itself.
pub struct LinguaTray {
    state: String,
    events: UnboundedSender<AppEvent>,
}

impl LinguaTray {
    fn new(events: UnboundedSender<AppEvent>) -> Self {
        Self {
            state: "Idle".to_owned(),
            events,
        }
    }
}

impl ksni::Tray for LinguaTray {
    fn id(&self) -> String {
        crate::config::APP_ID.to_owned()
    }

    fn title(&self) -> String {
        "Lingua".to_owned()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: crate::config::APP_ID.to_owned(),
            icon_pixmap: self.icon_pixmap(),
            title: "Lingua".to_owned(),
            description: format!("Dictation: {}", self.state),
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![tray_icon()]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.events.send(AppEvent::IpcOpenSettings);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open settings".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(AppEvent::IpcOpenSettings);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(AppEvent::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Register the tray icon and return the state channel.
///
/// `None` when no StatusNotifier host answers: the application carries on
/// without a tray icon (FR-017, Scenario 3.3).
pub async fn spawn(events: UnboundedSender<AppEvent>) -> Option<TrayState> {
    let handle = match LinguaTray::new(events).spawn().await {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(%error, "no tray host available; settings stay reachable by relaunching");
            return None;
        }
    };

    tracing::info!("tray icon registered");

    let (state_tx, mut state_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(state) = state_rx.recv().await {
            if handle.update(|tray| tray.state = state).await.is_none() {
                tracing::debug!("tray service stopped; state updates disabled");
                break;
            }
        }
    });

    Some(TrayState(state_tx))
}

/// Decode the embedded PNG into the ARGB32 (network byte order) tray pixmap.
fn tray_icon() -> ksni::Icon {
    static ICON: std::sync::LazyLock<ksni::Icon> = std::sync::LazyLock::new(|| {
        let empty = ksni::Icon {
            width: 0,
            height: 0,
            data: Vec::new(),
        };
        let Some((rgba, width, height)) = crate::ui::icon_rgba() else {
            tracing::warn!("embedded tray icon could not be decoded");
            return empty;
        };

        let mut data = Vec::with_capacity(rgba.len());
        for pixel in rgba.as_chunks::<4>().0 {
            let (red, green, blue, alpha) = (pixel[0], pixel[1], pixel[2], pixel[3]);
            let scale =
                |channel: u8| -> u8 { ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8 };
            data.extend_from_slice(&[alpha, scale(red), scale(green), scale(blue)]);
        }

        ksni::Icon {
            width: *width as i32,
            height: *height as i32,
            data,
        }
    });

    ICON.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_decodes_to_argb32() {
        let icon = tray_icon();
        assert_eq!(icon.width, 64);
        assert_eq!(icon.height, 64);
        assert_eq!(icon.data.len(), 64 * 64 * 4);
    }

    #[test]
    fn transparent_pixels_stay_transparent() {
        let icon = tray_icon();
        let first = &icon.data[..4];
        assert_eq!(first[0], 0, "corner pixel must have zero alpha");
        assert_eq!(
            first[1..],
            [0, 0, 0],
            "premultiplied rgb of a transparent pixel is zero"
        );
    }
}
