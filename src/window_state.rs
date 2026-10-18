use crate::settings::{Settings, read_json};
use gpui_kit::{App, Bounds, Pixels, Window, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WindowState {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl WindowState {
    pub(crate) fn capture(window: &Window) -> Self {
        let bounds = window.window_bounds().get_bounds();
        // WindowOptions takes content size; the native frame includes the title bar.
        let content = window.viewport_size();
        Self {
            x: bounds.origin.x.into(),
            y: bounds.origin.y.into(),
            width: content.width.into(),
            height: content.height.into(),
        }
    }

    pub(crate) fn update(&mut self, window: &Window) {
        // Keep the last normal geometry while maximized or fullscreen.
        if !window.is_maximized() && !window.is_fullscreen() {
            *self = Self::capture(window);
        }
    }

    fn is_valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f32::is_finite)
            && self.width > 0.
            && self.height > 0.
    }

    pub(crate) fn load() -> io::Result<(Option<PathBuf>, Option<Self>)> {
        let path = Settings::path()?.with_file_name("window-state.json");
        let state = match read_json::<Self>(&path) {
            Ok(state) if state.is_valid() => Some(state),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid window geometry",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok((Some(path), state))
    }

    fn restore(&self, screens: &[Bounds<Pixels>]) -> Option<Bounds<Pixels>> {
        if !self.is_valid() {
            return None;
        }
        // Window coordinates are a physical platform boundary.
        let titlebar = point(px(self.x + self.width.min(200.) / 2.), px(self.y + 20.));
        let matching = screens.iter().find(|screen| screen.contains(&titlebar));
        let screen = matching.or_else(|| screens.first())?;
        let width = px(self.width.max(200.)).min(screen.size.width);
        let height = px(self.height.max(100.)).min(screen.size.height);
        let origin = if matching.is_some() {
            point(
                px(self.x).clamp(screen.left(), screen.right() - width),
                px(self.y).clamp(screen.top(), screen.bottom() - height),
            )
        } else {
            point(
                screen.left() + (screen.size.width - width) / 2.,
                screen.top() + (screen.size.height - height) / 2.,
            )
        };
        Some(Bounds::new(origin, size(width, height)))
    }

    pub(crate) fn window_bounds(state: Option<&Self>, cx: &App) -> WindowBounds {
        let mut screens = Vec::new();
        if let Some(display) = cx.primary_display() {
            screens.push(display.visible_bounds());
        }
        screens.extend(cx.displays().iter().map(|display| display.visible_bounds()));
        let bounds = state
            .and_then(|state| state.restore(&screens))
            .unwrap_or_else(|| Bounds::centered(None, size(px(1200.), px(800.)), cx));
        WindowBounds::Windowed(bounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_restores_on_connected_displays_and_fits_after_disconnect() {
        let primary = Bounds::new(point(px(0.), px(20.)), size(px(1200.), px(760.)));
        let secondary = Bounds::new(point(px(1200.), px(0.)), size(px(1000.), px(800.)));
        let state = WindowState {
            x: 1300.,
            y: 100.,
            width: 800.,
            height: 600.,
        };
        assert_eq!(
            state.restore(&[primary, secondary]).unwrap(),
            Bounds::new(point(px(1300.), px(100.)), size(px(800.), px(600.)))
        );
        assert_eq!(
            state.restore(&[primary]).unwrap(),
            Bounds::new(point(px(200.), px(100.)), size(px(800.), px(600.)))
        );
        let oversized = WindowState {
            width: 5000.,
            height: 3000.,
            ..state.clone()
        };
        assert_eq!(oversized.restore(&[primary]).unwrap(), primary);
        let negative_display = Bounds::new(point(px(-1000.), px(0.)), size(px(1000.), px(800.)));
        let left = WindowState {
            x: -900.,
            ..state.clone()
        };
        assert_eq!(
            left.restore(&[primary, negative_display]).unwrap().origin.x,
            px(-900.)
        );
        assert!(
            WindowState {
                x: f32::NAN,
                ..state.clone()
            }
            .restore(&[primary])
            .is_none()
        );
        assert!(
            WindowState {
                width: -1.,
                ..state.clone()
            }
            .restore(&[primary])
            .is_none()
        );
        assert!(state.restore(&[]).is_none());
        let decoded: WindowState =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(decoded, state);
    }
}
