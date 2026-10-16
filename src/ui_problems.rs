//! Startup problem notice (FR-014): the daemon opens this window by itself
//! whenever a capability check fails once the boot probes have settled — also
//! when it otherwise starts without any window.

use iced::widget::{button, column, row, scrollable};
use iced::{Element, Length, Task};

use crate::app::App;
use crate::ui::{self, Message, metrics, style};

/// The window's heading and lead line, kept as constants like every other
/// user-visible string.
const HEADING: &str = "Startup problems";
const LEAD: &str =
    "Dictation stays limited until these are fixed; each check names the exact command.";

/// Actions the problem notice can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Recheck,
    Close,
}

pub fn update(app: &mut App, action: Action) -> Vec<Task<Message>> {
    match action {
        Action::Recheck => app.recheck(),
        Action::Close => vec![app.close_surface(ui::Surface::Problems)],
    }
}

pub fn view(app: &App) -> Element<'_, Action> {
    let mut checks = column![].spacing(metrics::SPACE_SM);
    for check in app.checks.iter().filter(|check| !check.ok) {
        checks = checks.push(ui::check_row(check));
    }

    column![
        ui::heading::<Action>(HEADING),
        ui::lead::<Action>(LEAD),
        scrollable(checks)
            .height(Length::Fill)
            .direction(ui::scroll_direction())
            .style(style::scrollable),
        row![
            button("Re-check")
                .style(style::primary_button)
                .padding(metrics::BUTTON_PADDING)
                .on_press(Action::Recheck),
            button("Close")
                .style(style::secondary_button)
                .padding(metrics::BUTTON_PADDING)
                .on_press(Action::Close),
        ]
        .spacing(metrics::SPACE_MD),
    ]
    .spacing(metrics::SPACE_LG)
    .padding(metrics::SPACE_XL)
    .into()
}
