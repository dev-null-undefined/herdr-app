//! Where the window was: its size, its place on which display, and whether it
//! was maximized or full screen. Kept in `window.json` (main) or
//! `window-<name>.json` (another Herdr window) so the next start opens it the
//! same way.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{App, Bounds, DisplayId, Pixels, Task, Window, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};

/// The first start's size.
const FIRST: (f32, f32) = (1280.0, 820.0);
/// Writes wait for a drag or resize to settle.
const SETTLE: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum State {
    Windowed,
    Maximized,
    Fullscreen,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    state: State,
    /// x, y, width, height of the window when not maximized (what un-maximizing
    /// goes back to), in the display's coordinates.
    bounds: [f32; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display: Option<String>,
}

thread_local! {
    /// Each window's place now, as last seen; written to disk once it settles.
    static NOW: RefCell<HashMap<String, Option<Place>>> = RefCell::new(HashMap::new());
}

fn path(name: &str) -> PathBuf {
    if name == crate::screens::MAIN {
        crate::mods::app_dir().join("window.json")
    } else {
        crate::mods::app_dir().join(format!("window-{name}.json"))
    }
}

fn load(name: &str) -> Option<Place> {
    std::fs::read_to_string(path(name)).ok().and_then(|s| serde_json::from_str(&s).ok())
}

fn with_now<R>(name: &str, f: impl FnOnce(&mut Option<Place>) -> R) -> R {
    NOW.with_borrow_mut(|n| f(n.entry(name.to_owned()).or_insert_with(|| load(name))))
}

fn uuid(d: &dyn gpui::PlatformDisplay) -> Option<String> {
    d.uuid().ok().map(|u| u.to_string())
}

/// How to open the window: last time's place if its display is still there and
/// the window's corner is on it, else last time's size centered; maximized or
/// full screen as it was. Linux: always centered (Wayland doesn't let a window
/// place itself, and X11 window managers shift it by their frame each time).
pub fn initial(name: &str, cx: &App) -> (WindowBounds, Option<DisplayId>) {
    let Some(p) = with_now(name, |n| n.clone()) else {
        return (WindowBounds::Windowed(Bounds::centered(None, size(px(FIRST.0), px(FIRST.1)), cx)), None);
    };
    let [x, y, w, h] = p.bounds;
    let (w, h) = (w.max(480.0), h.max(320.0));
    let display = p.display.as_deref().and_then(|want| cx.displays().into_iter().find(|d| uuid(d.as_ref()).as_deref() == Some(want)));
    let at = point(px(x), px(y));
    let bounds = match &display {
        Some(d) if !cfg!(target_os = "linux") && d.bounds().contains(&at) => Bounds::new(at, size(px(w), px(h))),
        _ => Bounds::centered(display.as_ref().map(|d| d.id()), size(px(w), px(h)), cx),
    };
    let wb = match p.state {
        State::Windowed => WindowBounds::Windowed(bounds),
        State::Maximized => WindowBounds::Maximized(bounds),
        State::Fullscreen => WindowBounds::Fullscreen(bounds),
    };
    (wb, display.map(|d| d.id()))
}

/// Notes where the window is now; the place to write if it changed.
fn note(name: &str, window: &Window, cx: &App) -> Option<Place> {
    let (state, b): (State, Bounds<Pixels>) = match window.inner_window_bounds() {
        WindowBounds::Windowed(b) => (State::Windowed, b),
        WindowBounds::Maximized(b) => (State::Maximized, b),
        WindowBounds::Fullscreen(b) => (State::Fullscreen, b),
    };
    let display = window.display(cx).and_then(|d| uuid(d.as_ref()));
    with_now(name, |n| {
        // Maximized on X11 reports the maximized size: keep the windowed one.
        let bounds = match (state, n.as_ref()) {
            (State::Windowed, _) | (_, None) => [f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)],
            (_, Some(prev)) => prev.bounds,
        };
        let place = Place { state, bounds, display };
        if n.as_ref() == Some(&place) || b.size.width <= px(0.0) {
            return None;
        }
        *n = Some(place.clone());
        Some(place)
    })
}

fn write(name: &str, p: &Place) {
    if let Ok(s) = serde_json::to_string_pretty(p) {
        let _ = crate::persist::write_atomic(&path(name), s + "\n");
    }
}

/// After the window moved or resized: written once it settles (the task
/// replaces the last one, which cancels it).
pub fn changed(name: &str, window: &Window, cx: &App) -> Option<Task<()>> {
    let p = note(name, window, cx)?;
    let ex = cx.background_executor().clone();
    let name = name.to_owned();
    Some(cx.background_executor().spawn(async move {
        ex.timer(SETTLE).await;
        write(&name, &p);
    }))
}

/// Writes it now (the window is closing).
pub fn save(name: &str, window: &Window, cx: &App) {
    note(name, window, cx);
    if let Some(p) = with_now(name, |n| n.clone()) {
        write(name, &p);
    }
}
