//! Several Herdr windows in one process (`window.new`). Each is a whole
//! screen of this home: its own model, view, layout clicks and mods, kept in
//! step with the others through the core's layout documents, as two
//! computers' screens are. In code a Herdr window is a "screen", so it isn't
//! mistaken for a mod's `kind: "window"` overlay (`windows.json`).
//!
//! Which window shows which space or tab is shared.json `screens`:
//! `{ "<name>": { "spaces": [ids], "tabs": [ids], "adhoc": true, "closed": true } }`,
//! ids as Herdr shows them (this computer's bare, others' `<machine>/<id>`).
//! A space in no list, or held by a window that isn't open, shows in main. A
//! tab in no list shows where its space does. Windows listed in settings.json
//! `windows` open at every start; others are made on the fly (`adhoc`) and
//! kept, closed or not, until `window.forget`.

use crate::mods::{self, Request};
use crate::model::Model;
use crate::window::AppWindow;
use gpui::{App, AppContext as _, WindowHandle};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

pub const MAIN: &str = "main";
/// The shared.json key.
pub const KEY: &str = "screens";

/// What a window holds, as shared.json keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Held {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spaces: Vec<String>,
    /// Tabs pulled out of a space shown elsewhere.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<String>,
    /// Made on the fly, not listed in settings.json.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub adhoc: bool,
    /// Closed by itself (not by quitting): an adhoc one stays closed at the next start.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
}

pub type Screens = BTreeMap<String, Held>;

pub fn read() -> Screens {
    mods::shared(KEY).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// Writes the owners; every window takes its share again (`Request::Screens`).
pub fn write(s: &Screens) {
    // Main's entry stays, empty or not: it says main's list was made (`seed_main`).
    let kept: Screens = s.iter().filter(|(n, h)| *n == MAIN || **h != Held::default()).map(|(n, h)| (n.clone(), h.clone())).collect();
    mods::set_shared(KEY.into(), serde_json::to_value(&kept).unwrap_or(Value::Null));
}

/// Whether owners are recorded: once a second window has existed (shared.json
/// has `screens`) or settings.json lists one. A person with one window gets no
/// writes from it.
pub fn recording() -> bool {
    mods::shared(KEY).is_some() || !listed().is_empty() || OPEN.with_borrow(|o| o.len() > 1)
}

/// Main's list, made once when windows start: every space main shows that no
/// list holds yet, so nothing main shows is up for grabs later. Main's entry
/// marks it made. False when there was nothing to do.
pub fn seed_main(held: &mut Screens, model: &Model) -> bool {
    if held.contains_key(MAIN) {
        return false;
    }
    let spaces = model.spaces.iter().filter(|p| !held.values().any(|h| h.spaces.contains(&p.id))).map(|p| p.id.clone()).collect();
    held.insert(MAIN.to_owned(), Held { spaces, ..Default::default() });
    true
}

pub fn valid(name: &str) -> Result<(), String> {
    let ok = !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok { Ok(()) } else { Err(format!("window name {name:?}: 1 to 32 letters, digits, - or _")) }
}

// ---- who shows what --------------------------------------------------------

/// The owners as they stand: which window shows each space and tab now.
#[derive(Clone, Debug, Default)]
pub struct Owners {
    held: Screens,
    open: BTreeSet<String>,
    listed: BTreeSet<String>,
}

impl Owners {
    pub fn now() -> Owners {
        Owners::new(read(), open_names().into_iter().collect(), listed())
    }

    pub fn new(held: Screens, open: BTreeSet<String>, listed: BTreeSet<String>) -> Owners {
        Owners { held, open, listed }
    }

    /// A window whose lists count: main, a listed one, or a kept adhoc one.
    fn counts(&self, name: &str) -> bool {
        name == MAIN || self.listed.contains(name) || self.held.get(name).is_some_and(|h| h.adhoc)
    }

    fn holder(&self, find: impl Fn(&Held) -> bool) -> Option<&str> {
        self.held.iter().find(|(n, h)| self.counts(n) && find(h)).map(|(n, _)| n.as_str())
    }

    /// The window a space shows in: the one holding it while that one is open, else main.
    pub fn space(&self, id: &str) -> &str {
        self.holder(|h| h.spaces.iter().any(|s| s == id)).filter(|n| self.open.contains(*n)).unwrap_or(MAIN)
    }

    /// The window a tab shows in: the one it was pulled out to while open, else its space's.
    pub fn tab(&self, tab: &str, space: &str) -> &str {
        match self.holder(|h| h.tabs.iter().any(|t| t == tab)) {
            Some(n) if self.open.contains(n) => n,
            _ => self.space(space),
        }
    }

    /// The window a pane's or terminal's tab shows in (`None`: in no tab).
    pub fn of_tab_in(&self, model: &Model, node: &str) -> Option<&str> {
        let space = model.space_of(node)?;
        let tab = model.tab_of(node)?;
        Some(self.tab(&tab.id, &space.id))
    }

    /// Whether `me` shows anything of the space: one of its tabs, or the space itself while it has none.
    pub fn shows(&self, model: &Model, space: &str, me: &str) -> bool {
        let Some(p) = model.space(space) else { return false };
        if p.tabs.is_empty() {
            return self.space(space) == me;
        }
        p.tabs.iter().any(|t| self.tab(&t.id, space) == me)
    }

    pub fn spaces_of<'a>(&self, model: &'a Model, me: &str) -> Vec<&'a str> {
        model.spaces.iter().filter(|p| self.shows(model, &p.id, me)).map(|p| p.id.as_str()).collect()
    }

    pub fn is_open(&self, name: &str) -> bool {
        self.open.contains(name)
    }

    pub fn is_listed(&self, name: &str) -> bool {
        self.listed.contains(name)
    }
}

/// Keeps a window's selection on what it shows: its active space one it
/// shows, each such space's active tab one of its own. Returns whether it changed.
pub fn keep_view(model: &mut Model, owners: &Owners, me: &str) -> bool {
    // Alone, a window shows everything: its selection stays as it always was
    // (none selected included).
    if owners.open.iter().all(|n| n == me) {
        return false;
    }
    let mut changed = false;
    let shown: Vec<String> = owners.spaces_of(model, me).into_iter().map(str::to_owned).collect();
    for p in model.spaces.iter_mut().filter(|p| shown.contains(&p.id)) {
        let mine: Vec<&str> = p.tabs.iter().filter(|t| owners.tab(&t.id, &p.id) == me).map(|t| t.id.as_str()).collect();
        let active = p.active.as_deref().or(p.tabs.first().map(|t| t.id.as_str()));
        if let Some(first) = mine.first()
            && !active.is_some_and(|a| mine.contains(&a))
        {
            // A settled tab only when nothing else is here.
            let pick = p.tabs.iter().find(|t| mine.contains(&t.id.as_str()) && t.settled.is_none()).map(|t| t.id.clone()).unwrap_or_else(|| first.to_string());
            p.active = Some(pick);
            changed = true;
        }
    }
    let active_ok = model.active.as_ref().is_some_and(|a| shown.contains(a));
    if !active_ok {
        let next = shown.first().cloned();
        changed |= model.active != next;
        model.active = next;
    }
    changed
}

/// Moves a space, a tab or a machine's spaces to window `to` (`window.move`).
/// A space takes its tabs along, except tabs pulled out to another window. A
/// tab moves alone and stays in its space. A machine moves the spaces of it
/// that window `from` shows, once: new ones go where they're started.
pub fn move_to(held: &mut Screens, owners: &Owners, model: &Model, kind: &str, id: &str, to: &str, from: &str) -> Result<(), String> {
    let spaces: Vec<String> = match kind {
        "space" => {
            model.space(id).ok_or_else(|| format!("no space {id:?}"))?;
            vec![id.to_owned()]
        }
        "tab" => {
            let space = model.space_of(id).filter(|p| p.tab(id).is_some()).ok_or_else(|| format!("no tab {id:?}"))?.id.clone();
            for h in held.values_mut() {
                h.tabs.retain(|t| t != id);
            }
            // Back with its space: no entry needed.
            if owners.space(&space) != to {
                held.entry(to.to_owned()).or_default().tabs.push(id.to_owned());
            }
            return Ok(());
        }
        "machine" => {
            let machine = (!id.is_empty()).then_some(id);
            if !model.spaces.iter().any(|p| p.machine.as_deref() == machine) {
                return Err(format!("no spaces on machine {id:?} (\"\": this computer)"));
            }
            // What `from` shows of it moves, once: the spaces it holds whole, and
            // its tabs pulled out of spaces held elsewhere, alone. A third window's stay.
            let of = model.spaces.iter().filter(|p| p.machine.as_deref() == machine);
            let pulled: Vec<(String, String)> = of
                .clone()
                .filter(|p| owners.space(&p.id) != from)
                .flat_map(|p| p.tabs.iter().filter(|t| owners.tab(&t.id, &p.id) == from).map(|t| (t.id.clone(), p.id.clone())))
                .collect();
            for (tab, space) in pulled {
                for h in held.values_mut() {
                    h.tabs.retain(|t| *t != tab);
                }
                if owners.space(&space) != to {
                    held.entry(to.to_owned()).or_default().tabs.push(tab);
                }
            }
            of.filter(|p| owners.space(&p.id) == from).map(|p| p.id.clone()).collect()
        }
        other => return Err(format!("window.move kind is \"space\", \"tab\" or \"machine\", not {other:?}")),
    };
    for space in spaces {
        let tabs: Vec<String> = model.space(&space).map(|p| p.tabs.iter().map(|t| t.id.clone()).collect()).unwrap_or_default();
        // Its tabs pulled out to `to` are home again, and its holder's own
        // entries for them meant nothing; those in a third window stay.
        let holder = owners.space(&space).to_owned();
        for (name, h) in held.iter_mut() {
            h.spaces.retain(|s| *s != space);
            if *name == to || *name == holder {
                h.tabs.retain(|t| !tabs.contains(t));
            }
        }
        // Main's list too: only a space nobody placed is in no list.
        held.entry(to.to_owned()).or_default().spaces.push(space);
    }
    Ok(())
}

/// Where a machine moved into `to` comes from when nobody said: the one other
/// open window that shows any of its spaces or tabs.
pub fn machine_source(owners: &Owners, model: &Model, machine: &str, to: &str, open: &[String]) -> Result<String, String> {
    let m = (!machine.is_empty()).then_some(machine);
    let showing: Vec<&String> = open
        .iter()
        .filter(|w| *w != to && model.spaces.iter().any(|p| p.machine.as_deref() == m && owners.shows(model, &p.id, w)))
        .collect();
    match showing.as_slice() {
        [one] => Ok(one.to_string()),
        [] => Err(format!("no other window shows machine {machine:?}")),
        _ => Err(format!("several windows show machine {machine:?}: say which one with from")),
    }
}

/// A space or tab just made in window `me` shows there (`space.new`, `tab.new`,
/// adoption). Returns whether the owners changed.
pub fn claim(held: &mut Screens, owners: &Owners, model: &Model, space: &str, tab: Option<&str>, me: &str) -> bool {
    match tab {
        // A new tab in a space shown elsewhere is pulled out to here.
        Some(tab) if model.space(space).is_some_and(|p| p.tabs.len() > 1) => {
            if owners.tab(tab, space) == me {
                return false;
            }
            for h in held.values_mut() {
                h.tabs.retain(|t| t != tab);
            }
            if owners.space(space) != me {
                held.entry(me.to_owned()).or_default().tabs.push(tab.to_owned());
            }
            true
        }
        // A new space (or its first tab): in its window's list, main's too.
        _ => {
            if held.get(me).is_some_and(|h| h.spaces.iter().any(|s| s == space)) {
                return false;
            }
            for h in held.values_mut() {
                h.spaces.retain(|s| s != space);
            }
            held.entry(me.to_owned()).or_default().spaces.push(space.to_owned());
            true
        }
    }
}

/// Moves `id` to position `index` (1 = first) among `own` (the items a
/// window shows, in order): those items take each other's places, the rest
/// stay put. When `own` is everything, that's the plain move it always was.
pub fn place_among<T>(items: &mut Vec<T>, id_of: impl Fn(&T) -> &str, own: &[String], id: &str, index: usize) {
    let mut order: Vec<String> = own.iter().filter(|o| *o != id).cloned().collect();
    order.insert(index.saturating_sub(1).min(order.len()), id.to_owned());
    let slots: Vec<usize> = items.iter().enumerate().filter(|(_, t)| id_of(t) == id || own.iter().any(|o| o == id_of(t))).map(|(i, _)| i).collect();
    if slots.len() != order.len() {
        return;
    }
    let mut taken: Vec<Option<T>> = std::mem::take(items).into_iter().map(Some).collect();
    let mut moving: HashMap<String, T> = HashMap::new();
    for &i in &slots {
        let t = taken[i].take().expect("each slot once");
        moving.insert(id_of(&t).to_owned(), t);
    }
    for (&i, o) in slots.iter().zip(&order) {
        taken[i] = moving.remove(o);
    }
    *items = taken.into_iter().flatten().collect();
}

/// `screens()` for mods: every window, open or not, with what it holds.
pub fn listing(owners: &Owners, focused: &str) -> Value {
    let mut names: BTreeSet<String> = owners.listed.clone();
    names.insert(MAIN.into());
    names.extend(owners.held.iter().filter(|(_, h)| h.adhoc).map(|(n, _)| n.clone()));
    names.extend(owners.open.iter().cloned());
    let mut out: Vec<Value> = names
        .into_iter()
        .map(|name| {
            let h = owners.held.get(&name).cloned().unwrap_or_default();
            let kind = if name == MAIN || owners.listed.contains(&name) { "listed" } else { "adhoc" };
            json!({
                "name": name, "main": name == MAIN, "kind": kind,
                "open": owners.open.contains(&name), "focused": name == focused,
                "spaces": h.spaces, "tabs": h.tabs,
            })
        })
        .collect();
    // Main first, then by name.
    out.sort_by_key(|w| (w["main"] != true, w["name"].as_str().unwrap_or_default().to_owned()));
    Value::Array(out)
}

// ---- the open windows ------------------------------------------------------

struct Open {
    name: String,
    tx: async_channel::Sender<Request>,
    handle: Option<WindowHandle<AppWindow>>,
}

thread_local! {
    static OPEN: RefCell<Vec<Open>> = const { RefCell::new(Vec::new()) };
    /// Open windows by when they last had focus, the most recent last.
    static FOCUS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Names in settings.json `windows` (main always counts as listed).
    static LISTED: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    static CORE: RefCell<Option<Arc<crate::hc::Core>>> = const { RefCell::new(None) };
    /// Terminals a window started lately, or is giving a pane: main leaves
    /// them alone until that window's document places them.
    static BORN: RefCell<HashMap<String, std::time::Instant>> = RefCell::new(HashMap::new());
    static HELD: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

/// How long a window's new terminal is its own to place.
const BORN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// A window started a terminal.
pub fn note_born(address: &str) {
    BORN.with_borrow_mut(|b| {
        b.retain(|_, at| at.elapsed() < BORN_GRACE);
        b.insert(address.to_owned(), std::time::Instant::now());
    });
}

/// A window is giving a terminal a pane (`terminal.keep`), until `release`.
pub fn hold(address: &str) {
    HELD.with_borrow_mut(|h| h.insert(address.to_owned()));
}

pub fn release(address: &str) {
    HELD.with_borrow_mut(|h| h.remove(address));
}

/// Whether some window is placing this terminal itself (main doesn't adopt it).
pub fn in_flight(address: &str) -> bool {
    HELD.with_borrow(|h| h.contains(address)) || BORN.with_borrow(|b| b.get(address).is_some_and(|at| at.elapsed() < BORN_GRACE))
}

pub fn open_names() -> Vec<String> {
    OPEN.with_borrow(|o| o.iter().map(|w| w.name.clone()).collect())
}

pub fn is_open(name: &str) -> bool {
    OPEN.with_borrow(|o| o.iter().any(|w| w.name == name))
}

pub fn listed() -> BTreeSet<String> {
    LISTED.with_borrow(Clone::clone)
}

/// settings.json `windows` changed: windows that left it are no longer listed
/// (their spaces show in main); new ones open. Returns the names that are new.
pub fn set_listed(names: BTreeSet<String>) -> Vec<String> {
    let names: BTreeSet<String> = names.into_iter().filter(|n| n != MAIN && valid(n).is_ok()).collect();
    let was = LISTED.replace(names.clone());
    if was == names {
        return Vec::new();
    }
    // One that left the list closes and its spaces join main, unless it was
    // also made on the fly.
    let mut held = read();
    let gone: Vec<String> = was.difference(&names).filter(|n| !held.get(*n).is_some_and(|h| h.adhoc)).cloned().collect();
    let mut handed = false;
    for name in &gone {
        handed |= hand_to_main(&mut held, name);
        if is_open(name) {
            request_to(name, Request::Run { name: "window.close".into(), args: Value::Null });
        }
    }
    if handed {
        write(&held);
    }
    request_all(|| Request::Screens);
    names.difference(&was).cloned().collect()
}

/// The focused window, else the one focused last, else main.
pub fn focused() -> String {
    FOCUS.with_borrow(|f| f.iter().rev().find(|n| is_open(n)).cloned()).unwrap_or_else(|| MAIN.into())
}

/// A window got or lost the keyboard; mods' `screens()` say which has it.
pub fn note_focus(name: &str, active: bool) {
    let before = focused();
    if active {
        FOCUS.with_borrow_mut(|f| {
            f.retain(|n| n != name);
            f.push(name.to_owned());
        });
    }
    if focused() != before {
        // Republished in every window's screens().
        request_all(|| Request::Screens);
    }
}

fn sender(name: &str) -> Option<async_channel::Sender<Request>> {
    OPEN.with_borrow(|o| o.iter().find(|w| w.name == name).map(|w| w.tx.clone()))
}

fn handle(name: &str) -> Option<WindowHandle<AppWindow>> {
    OPEN.with_borrow(|o| o.iter().find(|w| w.name == name).and_then(|w| w.handle))
}

/// Sends to one open window; false when it isn't open.
pub fn request_to(name: &str, r: Request) -> bool {
    sender(name).is_some_and(|tx| tx.try_send(r).is_ok())
}

pub fn request_all(r: impl Fn() -> Request) {
    for tx in OPEN.with_borrow(|o| o.iter().map(|w| w.tx.clone()).collect::<Vec<_>>()) {
        let _ = tx.try_send(r());
    }
}

/// Brings an open window to the front.
pub fn raise(name: &str, cx: &mut App) -> bool {
    let Some(h) = handle(name) else { return false };
    cx.activate(true);
    h.update(cx, |_, window, _| window.activate_window()).is_ok()
}

/// After connecting: main, then every listed window and every kept adhoc one
/// that wasn't closed. Requests from mods and the control socket are routed
/// to their window from here on.
pub fn start(core: Arc<crate::hc::Core>, requests: async_channel::Receiver<Request>, cx: &mut App) {
    CORE.set(Some(core));
    cx.spawn(async move |cx| {
        while let Ok(r) = requests.recv().await {
            cx.update(|cx| route(r, cx));
        }
    })
    .detach();
    let person = crate::settings::load_person().unwrap_or_default();
    LISTED.set(person.windows.keys().filter(|n| *n != MAIN && valid(n).is_ok()).cloned().collect());
    // A window taken out of the list while Herdr was off: its spaces join main, as live.
    let mut held = read();
    let gone: Vec<String> = held.iter().filter(|(n, h)| *n != MAIN && !h.adhoc && !listed().contains(*n)).map(|(n, _)| n.clone()).collect();
    if !gone.is_empty() {
        for name in &gone {
            hand_to_main(&mut held, name);
        }
        write(&held);
    }
    if let Err(e) = open(MAIN, cx) {
        panic!("cannot open the Herdr window: {e}");
    }
    let held = read();
    let mut later: Vec<String> = listed().into_iter().collect();
    let kept: Vec<String> = held.iter().filter(|(n, h)| h.adhoc && !h.closed && !later.contains(n)).map(|(n, _)| n.clone()).collect();
    later.extend(kept);
    // Listed windows open at every start, whatever they were.
    let mut cleared = held.clone();
    for n in &later {
        if let Some(h) = cleared.get_mut(n) {
            h.closed = false;
        }
    }
    if cleared != held {
        write(&cleared);
    }
    for name in later {
        if let Err(e) = open(&name, cx) {
            tracing::warn!("window {name}: {e}");
        }
    }
    cx.activate(true);
}

/// The window's options: its title names it, the Wayland app id stays Herdr's.
fn options(name: &str, cx: &App) -> gpui::WindowOptions {
    use gpui::{TitlebarOptions, WindowDecorations, WindowOptions, point, px};
    let (window_bounds, display_id) = crate::place::initial(name, cx);
    WindowOptions {
        window_bounds: Some(window_bounds),
        display_id,
        // macOS: always see-through, so a layout can draw its own title bar
        // live; the app drags the window (window/titlebar.rs).
        titlebar: Some(TitlebarOptions {
            title: Some(title(name).into()),
            // Windows: hidden for good (it can't change live); the app draws it.
            appears_transparent: cfg!(any(target_os = "macos", windows)),
            traffic_light_position: cfg!(target_os = "macos").then(|| point(px(12.0), px(7.0))),
        }),
        app_owns_titlebar_drag: cfg!(target_os = "macos"),
        // Linux: open with the title bar the shipped layouts draw. Turning the
        // system's off on the first render, while KWin maps the window, shrinks
        // it to 20x20; turning it on later (a "system" layout) is fine.
        window_decorations: Some(WindowDecorations::Client),
        // Herdr Dev and other side-by-side builds get their own dock icon.
        app_id: Some(std::env::var("HERDR_APP_ID").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "herdr-app".into())),
        ..Default::default()
    }
}

/// Main's title stays `Herdr`, as before there were windows; another's is
/// `Herdr · <name>`. Window rules (Hyprland's) place each window by it.
pub fn title(name: &str) -> String {
    if name == MAIN { "Herdr".into() } else { format!("Herdr · {name}") }
}

/// Opens window `name` (`window.new`), or does nothing if it's open. A name
/// that isn't listed is made an adhoc window, kept until forgotten.
pub fn open(name: &str, cx: &mut App) -> Result<(), String> {
    valid(name)?;
    if is_open(name) {
        return Ok(());
    }
    let core = CORE.with_borrow(Clone::clone).ok_or("Herdr isn't connected to its core yet")?;
    // Main has no entry of its own until windows start (`seed_main`).
    if name != MAIN {
        let mut held = read();
        let was = held.clone();
        let entry = held.entry(name.to_owned()).or_default();
        entry.closed = false;
        if !listed().contains(name) {
            entry.adhoc = true;
        }
        if held != was || mods::shared(KEY).is_none() {
            write(&held);
        }
    }
    let (tx, rx) = async_channel::unbounded();
    OPEN.with_borrow_mut(|o| o.push(Open { name: name.to_owned(), tx, handle: None }));
    let owned = name.to_owned();
    let made = cx.open_window(options(name, cx), move |window, cx| cx.new(|cx| AppWindow::new(core, owned, rx, window, cx)));
    match made {
        Ok(h) => {
            OPEN.with_borrow_mut(|o| {
                if let Some(w) = o.iter_mut().find(|w| w.name == name) {
                    w.handle = Some(h);
                }
            });
            // Every window's mods see the new one; spaces it holds leave main.
            request_all(|| Request::Screens);
            Ok(())
        }
        Err(e) => {
            OPEN.with_borrow_mut(|o| o.retain(|w| w.name != name));
            Err(format!("cannot open window {name}: {e:#}"))
        }
    }
}

/// A window closed by itself (`window.close`, its close button). Main's
/// close quits Herdr; another's spaces show in main until it opens again.
pub fn closed(name: &str, cx: &mut App) {
    OPEN.with_borrow_mut(|o| o.retain(|w| w.name != name));
    FOCUS.with_borrow_mut(|f| f.retain(|n| n != name));
    if name == MAIN {
        // macOS: an app lives on without its windows (the Dock, Cmd+Q), as before windows.
        if !cfg!(target_os = "macos") {
            cx.quit();
        }
        return;
    }
    // A forgotten window has no entry left to mark.
    let mut held = read();
    if let Some(h) = held.get_mut(name) {
        h.closed = true;
        write(&held);
    }
    request_all(|| Request::Screens);
}

/// Forgets an adhoc window (`window.forget`): its spaces and tabs join main.
/// The caller closes it if it's open.
pub fn forget(name: &str) -> Result<(), String> {
    if name == MAIN {
        return Err("main can't be forgotten".into());
    }
    let mut held = read();
    if listed().contains(name) {
        return Err(format!("window {name} is listed in settings.json windows; take it out there"));
    }
    if !hand_to_main(&mut held, name) && !is_open(name) {
        return Err(format!("no window {name}"));
    }
    write(&held);
    Ok(())
}

/// A forgotten (or no longer listed) window's spaces go to main's list:
/// placed, not up for grabs. Its pulled-out tabs go home with their spaces,
/// as when it closes. Before main's list is made, `seed_main` takes them.
/// False when it had no entry.
fn hand_to_main(held: &mut Screens, name: &str) -> bool {
    let Some(gone) = held.remove(name) else { return false };
    if let Some(main) = held.get_mut(MAIN) {
        for s in gone.spaces {
            if !main.spaces.contains(&s) {
                main.spaces.push(s);
            }
        }
    }
    true
}

/// Where a request goes. A mod's goes to its own window; `herdr-app do` to
/// the window it names (`window`), else the one showing the caller's
/// terminal (`terminal`, from `HC_TERMINAL_ID`), else the focused one.
/// `status`, `ls` and `shot` mean main unless they name one.
fn route(r: Request, cx: &mut App) {
    match r {
        Request::To { window, request } => {
            if !request_to(&window, *request) {
                tracing::debug!("request for closed window {window}");
            }
        }
        Request::Refresh => request_all(|| Request::Refresh),
        Request::Screens => request_all(|| Request::Screens),
        Request::Control { cmd, reply } => {
            let target = match control_target(&cmd, cx) {
                Ok(t) => t,
                Err(e) => {
                    let _ = reply.send(json!({ "error": e }));
                    return;
                }
            };
            let Some(tx) = sender(&target) else {
                let _ = reply.send(json!({ "error": format!("no open window {target:?}") }));
                return;
            };
            if let Err(e) = tx.try_send(Request::Control { cmd, reply })
                && let Request::Control { reply, .. } = e.into_inner()
            {
                let _ = reply.send(json!({ "error": format!("window {target} is closing") }));
            }
        }
        // From the app (macOS menu): the focused window.
        other => {
            let _ = request_to(&focused(), other);
        }
    }
}

fn control_target(cmd: &Value, cx: &App) -> Result<String, String> {
    target(cmd, &open_names(), &focused(), |t| window_of_terminal(t, cx))
}

/// Which open window a control request is for: the one it names; for a
/// second launch the one used last; `status`, `ls` and `shot` main; `do` the
/// one showing the caller's terminal, else the one used last.
fn target(cmd: &Value, open: &[String], focused: &str, of_terminal: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    if let Some(w) = cmd.get("window").and_then(Value::as_str).filter(|w| !w.is_empty()) {
        return if open.iter().any(|o| o == w) { Ok(w.to_owned()) } else { Err(format!("no open window {w:?} (`herdr-app do window.new --name {w}` opens it)")) };
    }
    if cmd.get("raise").is_some() {
        return Ok(focused.to_owned());
    }
    if cmd.get("do").is_none() {
        return Ok(MAIN.into());
    }
    if let Some(t) = cmd.get("terminal").and_then(Value::as_str).filter(|t| !t.is_empty())
        && let Some(w) = of_terminal(t).filter(|w| open.contains(w))
    {
        return Ok(w);
    }
    Ok(focused.to_owned())
}

/// The window showing a terminal's tab (a sub-agent's: its top agent's), by main's model.
pub fn window_of_terminal(address: &str, cx: &App) -> Option<String> {
    let main = handle(MAIN)?;
    let this = main.read(cx).ok()?;
    this.window_of_terminal(address, &Owners::now())
}

/// Test windows that are open without a real window behind them.
#[cfg(test)]
pub fn note_open(name: &str) {
    let (tx, _) = async_channel::unbounded();
    note_open_with(name, tx);
}

/// A test window whose requests go to `tx` (its `AppWindow`'s receiver).
#[cfg(test)]
pub fn note_open_with(name: &str, tx: async_channel::Sender<Request>) {
    OPEN.with_borrow_mut(|o| {
        o.retain(|w| w.name != name);
        o.push(Open { name: name.to_owned(), tx, handle: None });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Model, Tab};

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// alpha (two tabs) and beta (one) here, gamma on box.
    fn model() -> Model {
        let mut m = Model::default();
        m.add_space("alpha", "alpha", "/tmp", Tab::new("a1", "", "pa1", "t1"));
        m.add_tab("alpha", Tab::new("a2", "", "pa2", "t2"));
        m.add_space("beta", "beta", "/tmp", Tab::new("b1", "", "pb1", "t3"));
        m.add_space("box/gamma", "gamma", "/tmp", Tab::new("box/g1", "", "box/pg1", "box/t4"));
        m.space_mut("box/gamma").unwrap().machine = Some("box".into());
        m
    }

    fn held(json: Value) -> Screens {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn spaces_and_tabs_show_in_the_open_window_holding_them_else_main() {
        let h = held(json!({
            "left": { "spaces": ["beta"], "tabs": ["a2"], "adhoc": true },
            // Listed once, not any more: what it held shows in main.
            "gone": { "spaces": ["box/gamma"] },
        }));
        let o = Owners::new(h.clone(), set(&["main", "left", "gone"]), set(&[]));
        assert_eq!(o.space("alpha"), "main");
        assert_eq!(o.space("beta"), "left");
        assert_eq!(o.space("box/gamma"), "main");
        // A pulled-out tab shows where it was pulled to; the rest go with their space.
        assert_eq!(o.tab("a1", "alpha"), "main");
        assert_eq!(o.tab("a2", "alpha"), "left");
        assert_eq!(o.tab("b1", "beta"), "left");
        let m = model();
        assert!(o.shows(&m, "alpha", "main") && o.shows(&m, "alpha", "left"));
        assert_eq!(o.spaces_of(&m, "left"), vec!["alpha", "beta"]);
        assert_eq!(o.spaces_of(&m, "main"), vec!["alpha", "box/gamma"]);
        assert_eq!(o.of_tab_in(&m, "pa2"), Some("left"));
        assert_eq!(o.of_tab_in(&m, "t-nowhere"), None);
        // Left closed: everything it holds shows in main, and it keeps its lists.
        let closed = Owners::new(h, set(&["main"]), set(&[]));
        assert_eq!((closed.space("beta"), closed.tab("a2", "alpha")), ("main", "main"));
        assert_eq!(closed.spaces_of(&m, "left"), Vec::<&str>::new());
        // A listed window counts without an adhoc mark.
        let listed = Owners::new(held(json!({ "gone": { "spaces": ["box/gamma"] } })), set(&["main", "gone"]), set(&["gone"]));
        assert_eq!(listed.space("box/gamma"), "gone");
    }

    #[test]
    fn a_window_never_selects_what_another_shows() {
        let o = Owners::new(held(json!({ "left": { "spaces": ["beta"], "tabs": ["a2"], "adhoc": true } })), set(&["main", "left"]), set(&[]));
        // Main was on beta, alpha's second tab selected: both are left's now.
        let mut main = model();
        main.active = Some("beta".into());
        main.space_mut("alpha").unwrap().active = Some("a2".into());
        assert!(keep_view(&mut main, &o, "main"));
        assert_eq!(main.active.as_deref(), Some("alpha"));
        assert_eq!(main.space("alpha").unwrap().active.as_deref(), Some("a1"));
        assert!(!keep_view(&mut main, &o, "main"));
        // Left shows alpha only through its pulled-out tab.
        let mut left = model();
        left.active = Some("box/gamma".into());
        left.space_mut("alpha").unwrap().active = Some("a1".into());
        assert!(keep_view(&mut left, &o, "left"));
        assert_eq!(left.active.as_deref(), Some("alpha"));
        assert_eq!(left.space("alpha").unwrap().active.as_deref(), Some("a2"));
        // A window showing nothing selects nothing.
        let mut right = model();
        assert!(keep_view(&mut right, &o, "right"));
        assert_eq!(right.active, None);
        // Main alone keeps its selection as before windows, none selected included.
        let alone = Owners::new(held(json!({ "left": { "spaces": ["beta"], "adhoc": true } })), set(&["main"]), set(&[]));
        let mut main = model();
        main.active = None;
        assert!(!keep_view(&mut main, &alone, "main"));
        assert_eq!(main.active, None);
    }

    #[test]
    fn moves_take_a_space_with_its_tabs_but_leave_tabs_pulled_to_a_third_window() {
        let m = model();
        let open = set(&["main", "left", "right"]);
        let mut h = held(json!({ "left": { "tabs": ["a2"], "adhoc": true }, "right": { "adhoc": true } }));
        // alpha to right: its tab in left stays there.
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        move_to(&mut h, &o, &m, "space", "alpha", "right", "main").unwrap();
        assert_eq!(h["right"].spaces, vec!["alpha"]);
        assert_eq!(h["left"].tabs, vec!["a2"]);
        // alpha to left: the tab is home with its space again.
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        move_to(&mut h, &o, &m, "space", "alpha", "left", "right").unwrap();
        assert_eq!((h["left"].spaces.clone(), h["left"].tabs.clone(), h["right"].spaces.clone()), (vec!["alpha".to_owned()], vec![], vec![]));
        // A tab alone, out and back: the entry goes when it's with its space.
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        move_to(&mut h, &o, &m, "tab", "a1", "main", "left").unwrap();
        assert_eq!(h["main"].tabs, vec!["a1"]);
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        move_to(&mut h, &o, &m, "tab", "a1", "left", "main").unwrap();
        assert!(h["main"].tabs.is_empty());
        // Back to main: main's list holds it, no other.
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        move_to(&mut h, &o, &m, "space", "alpha", "main", "left").unwrap();
        assert_eq!(h["main"].spaces, vec!["alpha"]);
        assert!(h.iter().all(|(n, w)| n == "main" || !w.spaces.contains(&"alpha".to_owned())));
        // A machine: the spaces of it the window running the move shows, once.
        let mut h = held(json!({ "left": { "spaces": ["beta"], "adhoc": true } }));
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        move_to(&mut h, &o, &m, "machine", "", "right", "main").unwrap();
        assert_eq!(h["right"].spaces, vec!["alpha"]);
        assert_eq!(h["left"].spaces, vec!["beta"]);
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        move_to(&mut h, &o, &m, "machine", "box", "left", "main").unwrap();
        assert_eq!(h["left"].spaces, vec!["beta", "box/gamma"]);
        let o = Owners::new(h.clone(), open, set(&[]));
        assert!(move_to(&mut h, &o, &m, "space", "nope", "left", "main").unwrap_err().contains("no space"));
        assert!(move_to(&mut h, &o, &m, "machine", "vps", "left", "main").unwrap_err().contains("no spaces on machine"));
        assert!(move_to(&mut h, &o, &m, "pane", "pa1", "left", "main").unwrap_err().contains("kind"));
    }

    #[test]
    fn a_machine_moves_what_its_window_shows_of_it_and_leaves_a_third_windows_spaces() {
        // box: sp1 (two tabs) held by right, its t2 pulled out to main; sp2 main's.
        let mut m = Model::default();
        m.add_space("box/sp1", "one", "/tmp", Tab::new("box/t1", "", "box/p1", "box/a1"));
        m.add_tab("box/sp1", Tab::new("box/t2", "", "box/p2", "box/a2"));
        m.add_space("box/sp2", "two", "/tmp", Tab::new("box/t3", "", "box/p3", "box/a3"));
        for p in m.spaces.iter_mut() {
            p.machine = Some("box".into());
        }
        let open = set(&["main", "left", "right"]);
        let mut h = held(json!({
            "main": { "spaces": ["box/sp2"], "tabs": ["box/t2"] },
            "right": { "spaces": ["box/sp1"], "adhoc": true },
            "left": { "adhoc": true },
        }));
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        move_to(&mut h, &o, &m, "machine", "box", "left", "main").unwrap();
        // sp2 moves whole, t2 moves alone and stays in sp1, sp1 stays right's.
        assert_eq!(h["left"].spaces, vec!["box/sp2"]);
        assert_eq!(h["left"].tabs, vec!["box/t2"]);
        assert_eq!(h["right"].spaces, vec!["box/sp1"]);
        assert!(h["main"].spaces.is_empty() && h["main"].tabs.is_empty());
        let after = Owners::new(h.clone(), open.clone(), set(&[]));
        assert_eq!((after.tab("box/t2", "box/sp1"), after.tab("box/t1", "box/sp1")), ("left", "right"));
        // Back to right, where t2's space is: home with it, no entry left.
        move_to(&mut h, &after, &m, "machine", "box", "right", "left").unwrap();
        assert_eq!(h["right"].spaces, vec!["box/sp1", "box/sp2"]);
        assert!(h["left"].tabs.is_empty() && h["left"].spaces.is_empty());
    }

    #[test]
    fn a_machine_dropped_on_its_window_comes_from_the_one_other_window_showing_it() {
        let m = model();
        let open = vec!["main".to_owned(), "left".to_owned(), "right".to_owned()];
        // left shows beta (this computer); box's gamma is main's.
        let o = Owners::new(held(json!({ "left": { "spaces": ["beta"], "adhoc": true }, "main": { "spaces": ["alpha", "box/gamma"] } })), open.iter().cloned().collect(), set(&[]));
        assert_eq!(machine_source(&o, &m, "", "main", &open), Ok("left".into()));
        assert!(machine_source(&o, &m, "box", "main", &open).unwrap_err().contains("no other window"));
        assert_eq!(machine_source(&o, &m, "box", "left", &open), Ok("main".into()));
        // Two others show this computer: ask.
        let o = Owners::new(held(json!({ "left": { "spaces": ["beta"], "adhoc": true }, "right": { "tabs": ["a2"], "adhoc": true } })), open.iter().cloned().collect(), set(&[]));
        assert!(machine_source(&o, &m, "", "left", &open).unwrap_err().contains("from"));
    }

    #[test]
    fn what_a_window_makes_shows_there() {
        let mut m = model();
        let open = set(&["main", "left"]);
        let mut h = held(json!({ "left": { "adhoc": true } }));
        // A new space made from left.
        m.add_space("delta", "", "/tmp", Tab::new("d1", "", "pd1", "t5"));
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        assert!(claim(&mut h, &o, &m, "delta", None, "left"));
        assert_eq!(h["left"].spaces, vec!["delta"]);
        // Made from main: in main's list (only one nobody placed is in none), once.
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        assert!(claim(&mut h, &o, &m, "beta", None, "main"));
        assert_eq!(h["main"].spaces, vec!["beta"]);
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        assert!(!claim(&mut h, &o, &m, "beta", None, "main"));
        // A new tab from left in main's alpha is pulled out to left.
        m.add_tab("alpha", Tab::new("a3", "", "pa3", "t6"));
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        assert!(claim(&mut h, &o, &m, "alpha", Some("a3"), "left"));
        assert_eq!(h["left"].tabs, vec!["a3"]);
        // From main into left's delta: pulled out to main.
        m.add_tab("delta", Tab::new("d2", "", "pd2", "t7"));
        let o = Owners::new(h.clone(), open.clone(), set(&[]));
        assert!(claim(&mut h, &o, &m, "delta", Some("d2"), "main"));
        assert_eq!(h["main"].tabs, vec!["d2"]);
        // A tab made where its space shows: nothing.
        m.add_tab("delta", Tab::new("d3", "", "pd3", "t8"));
        let o = Owners::new(h.clone(), open, set(&[]));
        assert!(!claim(&mut h, &o, &m, "delta", Some("d3"), "left"));
    }

    #[test]
    fn screens_lists_every_window_main_first() {
        let h = held(json!({
            "zed": { "spaces": ["beta"], "adhoc": true, "closed": true },
            "left": { "tabs": ["a2"] },
            "stale": { "spaces": ["alpha"] },
        }));
        let o = Owners::new(h, set(&["main", "left"]), set(&["left"]));
        let l = listing(&o, "left");
        let names: Vec<&str> = l.as_array().unwrap().iter().map(|w| w["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["main", "left", "zed"]);
        assert_eq!(l[0], json!({ "name": "main", "main": true, "kind": "listed", "open": true, "focused": false, "spaces": [], "tabs": [] }));
        assert_eq!(l[1], json!({ "name": "left", "main": false, "kind": "listed", "open": true, "focused": true, "spaces": [], "tabs": ["a2"] }));
        assert_eq!(l[2], json!({ "name": "zed", "main": false, "kind": "adhoc", "open": false, "focused": false, "spaces": ["beta"], "tabs": [] }));
    }

    #[test]
    fn control_requests_go_to_the_named_window_then_the_callers_then_the_focused() {
        let open = vec!["main".to_owned(), "left".to_owned()];
        let of = |t: &str| match t {
            "t1" => Some("left".to_owned()),
            "t9" => Some("closed".to_owned()),
            _ => None,
        };
        let go = |cmd: Value| target(&cmd, &open, "left", of);
        assert_eq!(go(json!({ "do": "space.new", "window": "main", "terminal": "t1" })), Ok("main".into()));
        assert!(go(json!({ "do": "space.new", "window": "right" })).unwrap_err().contains("no open window"));
        assert_eq!(go(json!({ "do": "space.new", "terminal": "t1" })), Ok("left".into()));
        // A terminal in no tab, or in a window that isn't open: the focused one.
        assert_eq!(target(&json!({ "do": "space.new", "terminal": "t5" }), &open, "main", of), Ok("main".into()));
        assert_eq!(target(&json!({ "do": "space.new", "terminal": "t9" }), &open, "main", of), Ok("main".into()));
        // Asking what's on screen means main, unless named.
        for verb in ["status", "ls", "shot"] {
            assert_eq!(go(json!({ verb: true, "terminal": "t1" })), Ok("main".into()));
            assert_eq!(go(json!({ verb: true, "window": "left" })), Ok("left".into()));
        }
        // A second launch brings up the window used last.
        assert_eq!(go(json!({ "raise": true })), Ok("left".into()));
    }

    #[test]
    fn a_forgotten_windows_spaces_go_to_main_and_its_tabs_home() {
        let mut h = held(json!({
            "zed": { "spaces": ["beta", "alpha"], "tabs": ["box/g1"], "adhoc": true, "closed": true },
            "main": { "spaces": ["alpha"] },
        }));
        assert!(hand_to_main(&mut h, "zed"));
        assert_eq!(serde_json::to_value(&h).unwrap(), json!({ "main": { "spaces": ["alpha", "beta"] } }));
        assert!(!hand_to_main(&mut h, "zed"));
        // Before main's list is made, the seed takes them.
        let mut h = held(json!({ "zed": { "spaces": ["beta"], "adhoc": true } }));
        assert!(hand_to_main(&mut h, "zed"));
        assert!(h.is_empty());
    }

    #[test]
    fn main_lists_what_it_shows_once_when_windows_start() {
        let m = model();
        let mut h = held(json!({ "left": { "spaces": ["beta"], "adhoc": true } }));
        assert!(seed_main(&mut h, &m));
        assert_eq!(h["main"].spaces, vec!["alpha", "box/gamma"]);
        // Once: later, a space nobody placed stays out of every list.
        assert!(!seed_main(&mut h, &m));
        // An empty list still says it was made.
        let mut h = Screens::new();
        assert!(seed_main(&mut h, &Model::default()));
        assert_eq!(serde_json::to_value(&h).unwrap(), json!({ "main": {} }));
    }

    #[test]
    fn a_terminal_a_window_is_placing_is_not_up_for_adoption() {
        assert!(!in_flight("t-r3"));
        note_born("t-r3");
        assert!(in_flight("t-r3"));
        hold("t-r3-keep");
        assert!(in_flight("t-r3-keep"));
        release("t-r3-keep");
        assert!(!in_flight("t-r3-keep"));
    }

    #[test]
    fn a_move_counts_what_its_window_shows_and_is_the_plain_move_alone() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let go = |items: &[&str], own: &[&str], id: &str, index: usize| {
            let mut v = names(items);
            place_among(&mut v, |s| s, &names(own), id, index);
            v
        };
        // Alone (everything's its own): the plain move.
        let all = ["a", "b", "c", "d"];
        assert_eq!(go(&all, &all, "d", 1), names(&["d", "a", "b", "c"]));
        assert_eq!(go(&all, &all, "a", 3), names(&["b", "c", "a", "d"]));
        assert_eq!(go(&all, &all, "a", 99), names(&["b", "c", "d", "a"]));
        // A window showing b and d: d first among them, a and c stay where they are.
        assert_eq!(go(&all, &["b", "d"], "d", 1), names(&["a", "d", "c", "b"]));
        assert_eq!(go(&all, &["b", "d"], "b", 2), names(&["a", "d", "c", "b"]));
        // An item it doesn't show yet moves in among them.
        assert_eq!(go(&all, &["b", "d"], "a", 2), names(&["b", "a", "c", "d"]));
    }

    #[test]
    fn main_keeps_the_stock_title_and_others_are_named() {
        assert_eq!(title("main"), "Herdr");
        assert_eq!(title("left"), "Herdr · left");
    }

    #[test]
    fn window_names_are_short_and_plain() {
        assert!(valid("left").is_ok() && valid("dp-7_portrait").is_ok());
        for bad in ["", "a b", "x/y", "Herdr · left", &"x".repeat(33)] {
            assert!(valid(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_shared_file_keeps_only_what_a_window_holds() {
        let h = held(json!({ "left": { "spaces": ["beta"], "adhoc": true, "closed": false } }));
        assert_eq!(serde_json::to_value(&h).unwrap(), json!({ "left": { "spaces": ["beta"], "adhoc": true } }));
        // An entry from a newer build keeps working.
        let h: Screens = serde_json::from_value(json!({ "left": { "tabs": ["a2"], "monitor": "DP-7" } })).unwrap();
        assert_eq!(h["left"].tabs, vec!["a2"]);
    }
}
