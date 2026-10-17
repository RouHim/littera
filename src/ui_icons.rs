//! The drawn icon set (spec `model-list-field-rows` FR-012): small vector marks
//! for the controls whose meaning carries itself — disclosure, download, verify,
//! cancel, retry — so the dense list spends less text on the obvious. The marks
//! are drawn with iced's canvas on a 16-unit grid (no assets, no font glyphs, no
//! framework icon dependency), inked in the palette's role colours; every
//! icon-only control keeps its word in a tooltip, so nothing becomes a riddle.

use iced::widget::canvas::{self, Canvas, Frame, Path, Stroke, path};
use iced::widget::{button, text, tooltip};
use iced::{Color, Element, Length, Point, Radians, Renderer, Theme, Vector};

use crate::ui::{metrics, palette};

/// The design grid every mark is drawn on, in units.
const GRID: f32 = 16.0;
/// Stroke weight, in grid units.
const WEIGHT: f32 = 1.5;
/// The box a mark is drawn into, in logical pixels.
const ICON_SIZE: f32 = 18.0;

/// One drawn mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// Details collapsed: the panel opens below the row.
    ChevronDown,
    /// Details expanded.
    ChevronUp,
    /// Fetch the artifact.
    Download,
    /// Check the artifact on disk.
    Check,
    /// Abort the running transfer.
    Cross,
    /// Run the failed transfer again.
    Retry,
}

/// Draw one mark in `color`, `size` logical pixels square.
pub fn icon<M: 'static + Clone>(icon: Icon, color: Color, size: f32) -> Element<'static, M> {
    Canvas::new(Mark { icon, color })
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}

/// An icon-only control: the same stamped button as the text controls, its word
/// kept in a tooltip. A missing `message` renders the mark dimmed, like the
/// button's own disabled label.
pub fn icon_button<M: 'static + Clone>(
    icon_kind: Icon,
    ink: Color,
    message: Option<M>,
    label: &'static str,
    style: fn(&Theme, button::Status) -> button::Style,
) -> Element<'static, M> {
    let ink = if message.is_none() {
        palette::alpha(ink, 0.35)
    } else {
        ink
    };
    let mut control = button(icon::<M>(icon_kind, ink, ICON_SIZE))
        .padding([metrics::SPACE_XS, metrics::SPACE_SM])
        .style(style);
    if let Some(message) = message {
        control = control.on_press(message);
    }

    tooltip(
        control,
        text(label).size(metrics::TEXT_CAPTION),
        tooltip::Position::Top,
    )
    .gap(metrics::SPACE_SM)
    .into()
}

/// The canvas program that draws one mark.
struct Mark {
    icon: Icon,
    color: Color,
}

impl<M> canvas::Program<M> for Mark {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let unit = bounds.width.min(bounds.height) / GRID;
        let stroke = Stroke::default()
            .with_width(WEIGHT * unit)
            .with_color(self.color);
        let line = |points: &[(f32, f32)]| {
            Path::new(|builder| {
                for (index, (x, y)) in points.iter().enumerate() {
                    let point = Point::new(x * unit, y * unit);
                    if index == 0 {
                        builder.move_to(point);
                    } else {
                        builder.line_to(point);
                    }
                }
            })
        };

        match self.icon {
            Icon::ChevronDown => {
                frame.stroke(&line(&[(4.5, 6.5), (8.0, 10.0), (11.5, 6.5)]), stroke);
            }
            Icon::ChevronUp => {
                frame.stroke(&line(&[(4.5, 9.5), (8.0, 6.0), (11.5, 9.5)]), stroke);
            }
            Icon::Download => {
                frame.stroke(&line(&[(8.0, 3.0), (8.0, 10.0)]), stroke);
                frame.stroke(&line(&[(4.5, 6.5), (8.0, 10.0), (11.5, 6.5)]), stroke);
                frame.stroke(&line(&[(3.5, 13.0), (12.5, 13.0)]), stroke);
            }
            Icon::Check => {
                frame.stroke(&line(&[(3.5, 8.5), (6.5, 11.5), (12.5, 4.5)]), stroke);
            }
            Icon::Cross => {
                frame.stroke(&line(&[(5.0, 5.0), (11.0, 11.0)]), stroke);
                frame.stroke(&line(&[(11.0, 5.0), (5.0, 11.0)]), stroke);
            }
            Icon::Retry => {
                // An almost-closed circle with a filled head where it breaks:
                // the gap sits at the right, so the mark reads as a cycle.
                let center = Point::new(8.0 * unit, 8.0 * unit);
                let radius = 5.0 * unit;
                let gap = 1.0;
                let start = Radians(-gap / 2.0);
                let end = Radians(std::f32::consts::TAU - gap - gap / 2.0);
                frame.stroke(
                    &Path::new(|builder| {
                        builder.arc(path::Arc {
                            center,
                            radius,
                            start_angle: start,
                            end_angle: end,
                        });
                    }),
                    stroke,
                );

                let direction = Vector::new(-start.0.sin(), start.0.cos());
                let normal = Vector::new(direction.y, -direction.x);
                let at = |along: f32, across: f32| {
                    center
                        + Vector::new(start.0.cos(), start.0.sin()) * radius
                        + direction * (along * unit)
                        + normal * (across * unit)
                };
                frame.fill(
                    &Path::new(|builder| {
                        builder.move_to(at(2.0, 0.0));
                        builder.line_to(at(0.0, 1.7));
                        builder.line_to(at(0.0, -1.7));
                        builder.close();
                    }),
                    self.color,
                );
            }
        }

        vec![frame.into_geometry()]
    }
}
