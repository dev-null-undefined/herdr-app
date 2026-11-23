//! Dragging a space, a tab or a machine from one Herdr window into another.
//! A mod arms its own in-window drag (`dragArm`); while armed and the left
//! button is held, a move out of the window hands it to the platform (a
//! Wayland drag carrying `application/x-herdr-drag`). The window under the
//! pointer, the source too, tells the copy whose block is there: `over`,
//! `leave` and `drop` (`nextDragEvent`); the source copy gets `end`. Herdr
//! moves nothing itself: the copy runs `window.move`.

use super::*;

/// The MIME type of a Herdr drag: JSON `{ kind, id, from }`.
pub const DRAG_MIME: &str = "application/x-herdr-drag";

/// What a Herdr drag carries.
pub(super) fn payload(drag: &Value, from: &str) -> Vec<u8> {
    let mut p = drag.clone();
    p["from"] = json!(from);
    p.to_string().into_bytes()
}

/// A drag's `{ kind, id, from }`, if it's a Herdr one.
pub(super) fn read(data: &ExternalDragData) -> Option<Value> {
    if data.mime.as_ref() != DRAG_MIME {
        return None;
    }
    let v: Value = serde_json::from_slice(&data.bytes).ok()?;
    let ok = ["kind", "id", "from"].iter().all(|k| v.get(k).is_some_and(Value::is_string));
    ok.then_some(v)
}

/// A drag event for a copy, at `at` inside its block's `bounds`.
fn event(kind: &str, drag: &Value, at: Point<Pixels>, bounds: Bounds<Pixels>) -> Value {
    let mut e = drag.clone();
    e["type"] = json!(kind);
    e["x"] = json!(f32::from(at.x - bounds.origin.x));
    e["y"] = json!(f32::from(at.y - bounds.origin.y));
    e
}

/// The copy whose block is at `at`: an overlay on top, then a float, then the
/// smallest layout block holding it.
pub(super) fn copy_at<'a>(shown: impl IntoIterator<Item = (&'a str, u64)>, bounds: &HashMap<String, Bounds<Pixels>>, at: Point<Pixels>) -> Option<(u64, Bounds<Pixels>)> {
    let rank = |slot: &str| if slot.starts_with("overlay:") { 0 } else if slot.starts_with("float:") { 1 } else { 2 };
    shown
        .into_iter()
        .filter_map(|(slot, copy)| {
            let b = *bounds.get(slot)?;
            b.contains(&at).then(|| (rank(slot), f32::from(b.size.width) * f32::from(b.size.height), copy, b))
        })
        .min_by(|a, b| (a.0, a.1).partial_cmp(&(b.0, b.1)).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, _, copy, b)| (copy, b))
}

impl AppWindow {
    /// The listeners that start and follow drags between windows (a canvas
    /// at the window's root: they see every move and drag event).
    pub(super) fn drag_listeners(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.weak_entity();
        canvas(|_, _, _| (), move |_, _, window, _| {
            let out = this.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    let _ = out.update(cx, |this, _| this.drag_out(e, window));
                }
            });
            let over = this.clone();
            window.on_mouse_event(move |e: &FileDropEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    let _ = over.update(cx, |this, _| this.platform_drag(e, window));
                }
            });
        })
        .absolute()
        .size_0()
        .into_any_element()
    }

    /// An armed drag leaving the window with the button held becomes the
    /// platform's, carrying `{ kind, id, from }`.
    fn drag_out(&mut self, e: &MouseMoveEvent, window: &mut Window) {
        if self.drag_out.is_some() || e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if Bounds::new(Point::default(), window.viewport_size()).contains(&e.position) {
            return;
        }
        let Some((copy, drag)) = mods::armed(&self.name) else { return };
        let data = ExternalDragData { mime: DRAG_MIME.into(), bytes: payload(&drag, &self.name).into() };
        if window.start_platform_drag(ExternalDragPayload::Data(data)) {
            self.drag_out = Some(copy);
        }
    }

    /// The platform's drag events for this window: whose block is under the
    /// pointer gets `over`/`leave`/`drop`; the copy that started it, `end`.
    fn platform_drag(&mut self, e: &FileDropEvent, window: &mut Window) {
        match e {
            FileDropEvent::DataOver { position, data } => {
                let Some(drag) = read(data) else { return };
                let target = self.drag_target(*position);
                if self.drag_over.is_some() && self.drag_over != target.map(|(c, _)| c) {
                    self.drag_leave();
                }
                if let Some((copy, bounds)) = target {
                    self.drag_over = Some(copy);
                    mods::send_drag_event(copy, event("over", &drag, *position, bounds));
                }
            }
            FileDropEvent::DataDrop { position, data } => {
                let Some(drag) = read(data) else { return };
                let target = self.drag_target(*position);
                if self.drag_over.is_some() && self.drag_over != target.map(|(c, _)| c) {
                    self.drag_leave();
                }
                self.drag_over = None;
                if let Some((copy, bounds)) = target {
                    mods::send_drag_event(copy, event("drop", &drag, *position, bounds));
                }
            }
            FileDropEvent::Exited => self.drag_leave(),
            FileDropEvent::DataEnded { dropped } => {
                self.drag_leave();
                mods::disarm(&self.name);
                if let Some(copy) = self.drag_out.take() {
                    mods::send_drag_event(copy, json!({ "type": "end", "dropped": dropped }));
                }
                window.refresh();
            }
            _ => {}
        }
    }

    fn drag_leave(&mut self) {
        if let Some(copy) = self.drag_over.take() {
            mods::send_drag_event(copy, json!({ "type": "leave" }));
        }
    }

    /// The shown copy whose block is at `at`.
    fn drag_target(&self, at: Point<Pixels>) -> Option<(u64, Bounds<Pixels>)> {
        let (_, shown) = self.visible();
        let slots: Vec<(String, u64)> = shown.into_iter().filter_map(|(s, i)| self.copies.get(&s).filter(|c| c.inst == i && c.root.is_some()).map(|c| (s, c.id))).collect();
        copy_at(slots.iter().map(|(s, c)| (s.as_str(), *c)), &self.bounds.borrow(), at)
    }
}

#[cfg(test)]
mod tests {
    use super::{DRAG_MIME, copy_at, event, payload, read};
    use gpui::{Bounds, ExternalDragData, point, px, size};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn a_herdr_drag_carries_kind_id_and_the_window_it_left() {
        let bytes = payload(&json!({ "kind": "tab", "id": "tb1" }), "left");
        let data = ExternalDragData { mime: DRAG_MIME.into(), bytes: bytes.into() };
        assert_eq!(read(&data), Some(json!({ "kind": "tab", "id": "tb1", "from": "left" })));
        // Another type, or not our shape: not a Herdr drag.
        assert_eq!(read(&ExternalDragData { mime: "text/plain".into(), bytes: data.bytes.clone() }), None);
        assert_eq!(read(&ExternalDragData { mime: DRAG_MIME.into(), bytes: br#"{"kind":"tab"}"#.to_vec().into() }), None);
        assert_eq!(read(&ExternalDragData { mime: DRAG_MIME.into(), bytes: b"nope".to_vec().into() }), None);
    }

    #[test]
    fn the_copy_under_the_pointer_gets_block_local_positions() {
        let b = |x: f32, y: f32, w: f32, h: f32| Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        let bounds: HashMap<String, Bounds<gpui::Pixels>> = [
            ("root".to_owned(), b(0., 0., 1000., 800.)),
            ("spaces".to_owned(), b(0., 40., 240., 400.)),
            ("float:board".to_owned(), b(100., 100., 200., 100.)),
            ("overlay:o3".to_owned(), b(150., 120., 100., 50.)),
        ].into();
        let shown = [("root", 1), ("spaces", 2), ("float:board", 3), ("overlay:o3", 4)];
        let at = |x: f32, y: f32| copy_at(shown.iter().copied(), &bounds, point(px(x), px(y))).map(|(c, _)| c);
        // The smallest layout block wins over the one around it.
        assert_eq!(at(20., 60.), Some(2));
        assert_eq!(at(500., 500.), Some(1));
        // Floats sit over the layout, overlays over everything.
        assert_eq!(at(110., 110.), Some(3));
        assert_eq!(at(160., 130.), Some(4));
        // A block nobody shows, or outside the window: none.
        assert_eq!(copy_at([("spaces", 2)], &bounds, point(px(900.), px(700.))), None);
        let e = event("over", &json!({ "kind": "space", "id": "sp1", "from": "main" }), point(px(30.), px(90.)), bounds["spaces"]);
        assert_eq!(e, json!({ "type": "over", "kind": "space", "id": "sp1", "from": "main", "x": 30.0, "y": 50.0 }));
    }
}
