//! This window's share of the spaces and tabs (screens.rs keeps who shows
//! what) and the `window.*` commands for Herdr windows.

use super::*;
use crate::screens::{self, MAIN, Owners};

/// `view.json` for main (as before there were windows), `view-<name>.json` for another.
pub(super) fn view_file(name: &str) -> PathBuf {
    if name == MAIN { file("view.json") } else { file(&format!("view-{name}.json")) }
}

impl AppWindow {
    pub(super) fn is_main(&self) -> bool {
        self.name == MAIN
    }

    pub(crate) fn screen_name(&self) -> &str {
        &self.name
    }

    /// The window showing a terminal's tab (a sub-agent's: its top agent's);
    /// None when it's in no tab.
    fn window_of<'a>(&self, owners: &'a Owners, addr: &str) -> Option<&'a str> {
        let pane = self.model.pane_of(addr).or_else(|| self.model.pane_of(&self.root_agent(addr)))?;
        owners.of_tab_in(&self.model, &pane)
    }

    pub(crate) fn window_of_terminal(&self, addr: &str, owners: &Owners) -> Option<String> {
        self.window_of(owners, addr).map(str::to_owned)
    }

    /// Whether this window shows a terminal (one in no tab is main's).
    pub(super) fn shows_terminal(&self, owners: &Owners, addr: &str) -> bool {
        match self.window_of(owners, addr) {
            Some(w) => w == self.name,
            None => self.is_main(),
        }
    }

    /// A space or tab this window just made shows here (recorded only once
    /// windows started, and in main's list only once main's list is made).
    pub(super) fn claim(&mut self, space: &str, tab: Option<&str>) {
        let mut held = screens::read();
        if !screens::recording() || (self.is_main() && !held.contains_key(MAIN)) {
            return;
        }
        if screens::claim(&mut held, &Owners::now(), &self.model, space, tab, &self.name) {
            screens::write(&held);
        }
    }

    /// A labelled terminal main just placed (`adopt`): a space or tab it made
    /// shows in the window of the terminal that started it (its parent), else
    /// where its space is (a new space: main).
    pub(super) fn claim_adopted(&mut self, addr: &str, space: &str, new_space: bool, new_tab: bool) {
        if !new_space && !new_tab {
            return;
        }
        let owners = Owners::now();
        let parent = self.live.get(addr).and_then(|r| r.get("parent")).and_then(Value::as_str).filter(|p| !p.is_empty() && *p != addr);
        let Some(to) = parent.and_then(|p| self.window_of(&owners, p)).map(str::to_owned) else { return };
        let tab = self.model.pane_of(addr).and_then(|p| self.model.tab_of(&p)).map(|t| t.id.clone());
        let mut held = screens::read();
        if !screens::recording() || (to == MAIN && !held.contains_key(MAIN)) {
            return;
        }
        let tab = if new_space { None } else { tab.as_deref() };
        if screens::claim(&mut held, &owners, &self.model, space, tab, &to) {
            screens::write(&held);
        }
    }

    /// Who shows what changed: attach what came here, let go of what left.
    pub(super) fn screens_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hidden_fit = Default::default();
        self.sync(window, cx);
        cx.notify();
    }

    /// Main forgets owners of spaces and tabs that were closed. Only ids it
    /// has seen: a window's brand-new space reaches main's model a moment
    /// after its owner is written.
    pub(super) fn prune_screens(&mut self) {
        if !self.is_main() {
            return;
        }
        self.seed_main_list();
        let ids: Vec<String> = self.model.spaces.iter().flat_map(|p| std::iter::once(p.id.clone()).chain(p.tabs.iter().map(|t| t.id.clone()))).collect();
        self.known_ids.extend(ids);
        let gone = |id: &String| self.known_ids.contains(id) && self.model.space(id).is_none() && self.model.tab(id).is_none();
        let mut held = screens::read();
        let before = held.clone();
        for h in held.values_mut() {
            h.spaces.retain(|s| !gone(s));
            h.tabs.retain(|t| !gone(t));
        }
        if held != before {
            screens::write(&held);
        }
    }

    /// Once windows start, main lists every space it shows (`screens::seed_main`),
    /// when it has every machine's spaces it can get.
    fn seed_main_list(&mut self) {
        if !screens::recording() || !self.machines_seen {
            return;
        }
        if self.documents.values().any(|s| s.available && s.document.is_none()) {
            return;
        }
        let mut held = screens::read();
        if screens::seed_main(&mut held, &self.model) {
            screens::write(&held);
        }
    }

    /// Closes this window (`window.close`, `window.forget`). Main's quits Herdr.
    pub(crate) fn close_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drain_documents(window, cx);
        self.close_mods_now(Instant::now() + Duration::from_millis(250), window, cx);
        crate::place::save(&self.name, window, cx);
        self.let_go_terminals(cx);
        mods::forget_window(&self.name);
        let name = self.name.clone();
        window.remove_window();
        cx.defer(move |cx| screens::closed(&name, cx));
    }

    /// Another window closing alone: its terminal streams end here (the
    /// process goes on), and main attaches what it held.
    pub(super) fn let_go_terminals(&mut self, cx: &mut Context<Self>) {
        if self.is_main() {
            return;
        }
        for (_, t) in self.terms.drain() {
            t.view.read(cx).detach();
        }
    }

    /// The window that shows what a focus command is about, when it's another.
    pub(super) fn shown_elsewhere(&self, name: &str, a: &Value) -> Option<String> {
        let owners = Owners::now();
        let theirs = |w: &str| (w != self.name && owners.is_open(w)).then(|| w.to_owned());
        match name {
            "space.select" => {
                let id = Self::arg(a, "id")?;
                if owners.shows(&self.model, id, &self.name) {
                    return None;
                }
                let p = self.model.space(id)?;
                let w = if p.tabs.is_empty() { owners.space(id) } else { owners.tab(&p.active_tab()?.id, id) };
                let w = if owners.shows(&self.model, id, w) { w } else { p.tabs.iter().map(|t| owners.tab(&t.id, id)).next()? };
                theirs(w)
            }
            "tab.select" | "tab.settle" | "tab.unsettle" if Self::arg(a, "id").is_some() => {
                let id = Self::arg(a, "id")?;
                let space = self.model.space_of(id)?;
                theirs(owners.tab(id, &space.id))
            }
            "agent.focus" => theirs(self.window_of(&owners, Self::arg(a, "terminal")?).unwrap_or(MAIN)),
            "pane.focus" => {
                let pane = Self::owned(a, "pane").or_else(|| Self::arg(a, "terminal").and_then(|t| self.model.pane_of(t)))?;
                theirs(owners.of_tab_in(&self.model, &pane)?)
            }
            _ => None,
        }
    }

    /// The spaces this window shows, in order.
    pub(super) fn own_spaces(&self) -> Vec<String> {
        Owners::now().spaces_of(&self.model, &self.name).into_iter().map(str::to_owned).collect()
    }

    /// The tabs of a space this window shows, in order.
    pub(super) fn own_tabs<'a>(&self, space: &'a model::Space) -> Vec<&'a Tab> {
        let owners = Owners::now();
        space.tabs.iter().filter(|t| owners.tab(&t.id, &space.id) == self.name).collect()
    }

    /// `window.new`, `window.close`, `window.forget`, `window.move`, `window.raise`.
    pub(super) fn screen_command(&mut self, name: &str, a: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let target = Self::owned(a, "name");
        match name {
            "window.new" => {
                let Some(n) = target else { return self.fail("window.new needs name") };
                if let Err(e) = screens::valid(&n) {
                    return self.fail(e);
                }
                let was = screens::is_open(&n);
                if let Err(e) = screens::open(&n, cx) {
                    return self.fail(e);
                }
                if a.get("focus").and_then(Value::as_bool) != Some(false) {
                    self.raise(&n, window, cx);
                }
                self.reply = Some(json!({ "window": n, "opened": !was }));
            }
            "window.close" => match target.filter(|n| *n != self.name) {
                Some(n) => {
                    if !screens::request_to(&n, Request::Run { name: "window.close".into(), args: Value::Null }) {
                        self.fail(format!("no open window {n:?}"));
                    }
                }
                None => self.close_now(window, cx),
            },
            "window.forget" => {
                let Some(n) = target else { return self.fail("window.forget needs name") };
                if let Err(e) = screens::forget(&n) {
                    return self.fail(e);
                }
                if n == self.name {
                    self.close_now(window, cx);
                } else {
                    screens::request_to(&n, Request::Run { name: "window.close".into(), args: Value::Null });
                }
            }
            "window.raise" => {
                let Some(n) = target else { return self.fail("window.raise needs name") };
                if !self.raise(&n, window, cx) {
                    self.fail(format!("no open window {n:?}"));
                }
            }
            "window.move" => {
                if let Err(e) = self.move_to_window(a) {
                    self.fail(e);
                }
            }
            _ => {}
        }
    }

    /// The look (theme, text size) is the whole app's: what this window just
    /// changed of it goes to every other open window (main keeps it in its clicks).
    pub(super) fn share_look(&self) {
        let look = serde_json::to_value(&self.chrome.look).unwrap_or(Value::Null);
        let pick = serde_json::to_value(&self.picked_theme).unwrap_or(Value::Null);
        for other in screens::open_names().into_iter().filter(|n| *n != self.name) {
            screens::request_to(&other, Request::Look { look: look.clone(), pick: pick.clone() });
        }
    }

    /// Every other open window shows this window's layout; `fresh`: from no
    /// clicks (after a reset removed them).
    pub(super) fn relayout_others(&self, fresh: bool) {
        for other in screens::open_names().into_iter().filter(|n| *n != self.name) {
            screens::request_to(&other, Request::Layout { name: self.layout_name.clone(), fresh });
        }
    }

    /// Brings a window to the front (this one directly: it's being updated).
    pub(super) fn raise(&self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if name == self.name {
            cx.activate(true);
            window.activate_window();
            return true;
        }
        screens::raise(name, cx)
    }

    fn move_to_window(&mut self, a: &Value) -> Result<(), String> {
        let (Some(kind), Some(id), Some(to)) = (Self::arg(a, "kind"), Self::arg(a, "id"), Self::arg(a, "to")) else {
            return Err("window.move needs kind, id and to".into());
        };
        screens::valid(to)?;
        let owners = Owners::now();
        let mut held = screens::read();
        let known = to == MAIN || owners.is_listed(to) || held.get(to).is_some_and(|h| h.adhoc) || owners.is_open(to);
        if !known {
            return Err(format!("no window {to:?}: `window.new {{name: {to:?}}}` makes one"));
        }
        // A machine dropped on the window it goes to (the copy that got the drop runs this):
        // it comes from the window that shows it, unless `from` says.
        let from = match Self::arg(a, "from") {
            Some(f) => f.to_owned(),
            None if kind == "machine" && to == self.name => screens::machine_source(&owners, &self.model, id, to, &screens::open_names())?,
            None => self.name.clone(),
        };
        screens::move_to(&mut held, &owners, &self.model, kind, id, to, &from)?;
        let index = a.get("index").and_then(Value::as_u64).map(|i| i as usize);
        if let Some(i) = index.filter(|i| *i > 0) {
            let after = Owners::new(held.clone(), screens::open_names().into_iter().collect(), screens::listed());
            match kind {
                "space" => self.place_space(&after, id, to, i),
                "tab" => self.place_tab(&after, id, to, i),
                _ => {}
            }
        }
        screens::write(&held);
        Ok(())
    }

    /// Puts a moved space at position `index` (1 = first) among the spaces
    /// window `to` shows of its machine; the order is everyone's.
    fn place_space(&mut self, owners: &Owners, id: &str, to: &str, index: usize) {
        let Some(from) = self.model.spaces.iter().position(|p| p.id == id) else { return };
        let machine = self.model.spaces[from].machine.clone();
        let theirs: Vec<String> = self.model.spaces.iter().filter(|p| p.id != id && p.machine == machine && owners.shows(&self.model, &p.id, to)).map(|p| p.id.clone()).collect();
        let space = self.model.spaces.remove(from);
        let at = match theirs.get(index - 1) {
            Some(before) => self.model.spaces.iter().position(|p| p.id == *before),
            None => theirs.last().and_then(|last| self.model.spaces.iter().position(|p| p.id == *last)).map(|i| i + 1),
        };
        let at = at.unwrap_or(from).min(self.model.spaces.len());
        self.model.spaces.insert(at, space);
        self.model.order_spaces();
    }

    /// Puts a moved tab at position `index` among its space's tabs window `to` shows.
    fn place_tab(&mut self, owners: &Owners, id: &str, to: &str, index: usize) {
        let Some(space) = self.model.space_of(id).map(|p| p.id.clone()) else { return };
        let Some(p) = self.model.space_mut(&space) else { return };
        let Some(from) = p.tabs.iter().position(|t| t.id == id) else { return };
        let theirs: Vec<String> = p.tabs.iter().filter(|t| t.id != id && owners.tab(&t.id, &space) == to).map(|t| t.id.clone()).collect();
        let tab = p.tabs.remove(from);
        let at = match theirs.get(index - 1) {
            Some(before) => p.tabs.iter().position(|t| t.id == *before),
            None => theirs.last().and_then(|last| p.tabs.iter().position(|t| t.id == *last)).map(|i| i + 1),
        };
        let at = at.unwrap_or(from).min(p.tabs.len());
        p.tabs.insert(at, tab);
    }
}
