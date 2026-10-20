//! Mods: Luau scripts, each in a script thread of the game's own, calling
//! the game's natives.
//!
//! A mod is a folder in the mods folder with a `main.luau` and, optionally,
//! a `mod.json` (`{"autostart": true}`). The application calls [`install`]
//! before the game starts, then [`start`], [`stop`], [`mods`] and [`logs`]
//! from any thread; the mods themselves run on the engine's script update
//! (engine.rs). [`studio`] serves GLUE 5 Studio and the MCP endpoint.

mod checker;
mod engine;
pub mod files;
mod json;
mod menus;
pub mod natives;
mod storage;
pub mod studio;
mod vm;
pub mod world;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModState {
    Stopped,
    Running,
    Error,
}

impl ModState {
    pub fn name(self) -> &'static str {
        match self {
            ModState::Stopped => "stopped",
            ModState::Running => "running",
            ModState::Error => "error",
        }
    }
}

/// A mod, as the application shows it.
#[derive(Clone, Debug)]
pub struct Mod {
    pub name: String,
    pub state: ModState,
    /// Why it stopped, when it failed.
    pub error: Option<String>,
    /// Time it takes per frame (smoothed), in milliseconds.
    pub cpu_ms: f32,
    /// Memory of its VM, in bytes.
    pub memory: usize,
    /// It starts with the game.
    pub autostart: bool,
    /// It waits (the engine counts the time).
    pub waiting: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct LogLine {
    /// Increasing: what a reader has seen is what is below its cursor.
    pub seq: u64,
    pub time: SystemTime,
    pub module: String,
    pub level: LogLevel,
    pub text: String,
}

/// What `eval` gives back: the values as text, or the error.
pub type EvalResult = Result<String, String>;

enum Command {
    Start(String),
    Stop(String),
    Eval(String, Sender<EvalResult>),
}

struct State {
    mods: Vec<Mod>,
    commands: Vec<Command>,
    log: VecDeque<LogLine>,
    next_seq: u64,
    /// Files written, deleted or renamed, with a sequence number.
    changes: VecDeque<(u64, String)>,
    next_change: u64,
    /// The mods' menus, what was done with them (for the mods), what they
    /// notify (for the application).
    menus: Vec<Menu>,
    inputs: Vec<(String, u32, MenuInput)>,
    notices: VecDeque<(String, String)>,
    next_menu_id: u32,
}

struct Shared {
    dir: OnceLock<PathBuf>,
    state: Mutex<State>,
    /// Commands are waiting for the engine.
    pending: AtomicBool,
    /// Changes when a mod's state does (for readers that push it).
    generation: AtomicU64,
}

static SHARED: Shared = Shared {
    dir: OnceLock::new(),
    state: Mutex::new(State {
        mods: Vec::new(),
        commands: Vec::new(),
        log: VecDeque::new(),
        next_seq: 1,
        changes: VecDeque::new(),
        next_change: 1,
        menus: Vec::new(),
        inputs: Vec::new(),
        notices: VecDeque::new(),
        next_menu_id: 1,
    }),
    pending: AtomicBool::new(false),
    generation: AtomicU64::new(0),
};

const LOG_LINES: usize = 2000;

/// The mod whose turn it is (empty between turns), for the failure note.
static RUNNING_MOD: Mutex<String> = Mutex::new(String::new());

/// Mods kept from starting with the game: they ran when it last stopped.
static HELD_BACK: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The game has stopped working: which mods ran, and what a mod was doing
/// then (none: no mod was running). Saved, so that the next start does not
/// start them by itself.
pub fn note_failure() -> Option<String> {
    let dir = dir()?;
    let running: Vec<String> =
        mods().into_iter().filter(|m| m.state == ModState::Running).map(|m| m.name).collect();
    let during = RUNNING_MOD.try_lock().map(|m| m.clone()).unwrap_or_default();
    let native = match natives::RUNNING.load(Ordering::Relaxed) {
        0 => None,
        i => natives::NATIVES.get(i - 1).map(natives::qualified),
    };
    if running.is_empty() && during.is_empty() {
        return None;
    }
    let mut names = running.clone();
    if !during.is_empty() && !names.contains(&during) {
        names.push(during.clone());
    }
    let note = serde_json::json!({ "mods": names, "during": during, "native": native });
    let _ = std::fs::create_dir_all(dir.join(".glue5"));
    let _ = std::fs::write(dir.join(".glue5/failed.json"), note.to_string());
    Some(match (during.is_empty(), native) {
        (false, Some(n)) => format!("Mod {during} was calling {n}."),
        (false, None) => format!("Mod {during} was running."),
        _ => format!("Mods running: {}.", running.join(", ")),
    })
}

/// The mods kept from starting with the game this time, and why.
pub fn held_back() -> Vec<String> {
    HELD_BACK.lock().unwrap().clone()
}

/// The mods folder `dir` (made if missing): its mods listed, their files
/// open to the Studio. Before the game too; they run once [`install`]ed.
pub fn open(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    if SHARED.dir.set(dir.to_path_buf()).is_err() {
        return;
    }
    world::load_saved(dir);
    refresh();
}

/// Mods run with the game about to start (the folder [`open`]ed if it was
/// not): the mods marked autostart start once the game runs scripts. Call
/// before `glue5_runtime::start`.
pub fn install(dir: &Path) {
    open(dir);
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }
    let dir = self::dir().unwrap_or(dir);
    glue5_runtime::add_hooks(engine::hooks);
    // The mods' script threads get stacks of their own.
    glue5_runtime::overlay::more_script_stacks(32);
    log::info!("mods: {} ({})", dir.display(), natives::SUMMARY);
    // The game stopped while these ran: not by themselves this time.
    let failed = dir.join(".glue5/failed.json");
    if let Ok(text) = std::fs::read_to_string(&failed) {
        let _ = std::fs::remove_file(&failed);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        let names: Vec<String> =
            v["mods"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(str::to_string)).collect();
        for n in &names {
            log_line(n, LogLevel::Error, "the game stopped while this mod ran: it does not start by itself this time");
        }
        if !names.is_empty() {
            let text = format!("{} did not start: the game stopped while it ran last time.", names.join(", "));
            SHARED.state.lock().unwrap().notices.push_back(("mods".into(), text));
        }
        *HELD_BACK.lock().unwrap() = names;
    }
    let held = held_back();
    let autostart: Vec<String> =
        mods().into_iter().filter(|m| m.autostart && !held.contains(&m.name)).map(|m| m.name).collect();
    for name in autostart {
        start(&name);
    }
}

/// The mods folder, once open.
pub fn dir() -> Option<&'static Path> {
    SHARED.dir.get().map(PathBuf::as_path)
}

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Whether mods run with the game (it started with them).
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

/// Why a mod cannot be started now, if it cannot.
pub fn cannot_start() -> Option<&'static str> {
    (!installed()).then_some("mods run with the game, and it is not running with them: start it (mods on), or mark the mod to start with the game")
}

/// Whether mod `name` starts with the game.
pub fn set_autostart(name: &str, on: bool) -> Result<(), String> {
    files::set_autostart(name, on)
}

/// Reads the mods folder again (new or removed mods).
pub fn refresh() {
    let Some(dir) = dir() else { return };
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("main.luau").is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                found.push((name, files::autostart(&path)));
            }
        }
    }
    found.sort();
    let mut s = SHARED.state.lock().unwrap();
    let before = s.mods.len();
    s.mods.retain(|m| m.state != ModState::Stopped || found.iter().any(|(n, _)| *n == m.name));
    let mut changed = before != s.mods.len();
    for (name, autostart) in found {
        match s.mods.iter_mut().find(|m| m.name == name) {
            Some(m) => {
                changed |= m.autostart != autostart;
                m.autostart = autostart;
            }
            None => {
                changed = true;
                s.mods.push(Mod {
                    name,
                    state: ModState::Stopped,
                    error: None,
                    cpu_ms: 0.0,
                    memory: 0,
                    autostart,
                    waiting: false,
                })
            }
        }
    }
    s.mods.sort_by(|a, b| a.name.cmp(&b.name));
    if changed {
        SHARED.generation.fetch_add(1, Ordering::Relaxed);
    }
}

/// The mods and their state.
pub fn mods() -> Vec<Mod> {
    SHARED.state.lock().unwrap().mods.clone()
}

/// Changes whenever [`mods`] would give something else (but the time and
/// memory figures).
pub fn generation() -> u64 {
    SHARED.generation.load(Ordering::Relaxed)
}

/// Starts mod `name` (again, if it ran before).
pub fn start(name: &str) {
    command(Command::Start(name.to_string()));
}

/// Stops mod `name`: its thread ends, with what it created.
pub fn stop(name: &str) {
    command(Command::Stop(name.to_string()));
}

/// Stops mod `name` if it runs, then starts it.
pub fn restart(name: &str) {
    stop(name);
    start(name);
}

/// Runs Luau `code` in the game (in a script thread of its own, as a mod),
/// as an expression if it is one. `reply` gets the values as text.
pub fn eval(code: &str, reply: Sender<EvalResult>) {
    if !installed() {
        let _ = reply.send(Err("the game is not running with mods: they run from the next start".into()));
        return;
    }
    command(Command::Eval(code.to_string(), reply));
}

fn command(c: Command) {
    if !installed() {
        return;
    }
    SHARED.state.lock().unwrap().commands.push(c);
    SHARED.pending.store(true, Ordering::Release);
}

/// The log lines after `seq` (0: all kept), and the cursor to pass next.
pub fn logs(seq: u64) -> (Vec<LogLine>, u64) {
    let s = SHARED.state.lock().unwrap();
    let lines: Vec<LogLine> = s.log.iter().filter(|l| l.seq > seq).cloned().collect();
    (lines, s.next_seq - 1)
}

/// The files changed after `seq` (by any client), and the cursor to pass next.
pub fn changes(seq: u64) -> (Vec<String>, u64) {
    let s = SHARED.state.lock().unwrap();
    let paths = s.changes.iter().filter(|(n, _)| *n > seq).map(|(_, p)| p.clone()).collect();
    (paths, s.next_change - 1)
}

pub(crate) fn changed(path: &str) {
    let mut s = SHARED.state.lock().unwrap();
    if s.changes.len() == 256 {
        s.changes.pop_front();
    }
    let n = s.next_change;
    s.next_change += 1;
    s.changes.push_back((n, path.to_string()));
}

/// A mod's menu: what the application shows for it (`ui.menu` in Luau).
#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub module: String,
    pub id: u32,
    pub title: String,
    pub items: Vec<MenuItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub id: u32,
    pub label: String,
    /// A line under the label.
    pub note: Option<String>,
    pub kind: MenuKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MenuKind {
    Toggle(bool),
    Slider { value: f64, min: f64, max: f64, step: f64 },
    Choice { options: Vec<String>, index: usize },
    Button,
    Label,
}

/// What a player did with a menu's item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MenuInput {
    Toggle(bool),
    Value(f64),
    Choose(usize),
    Press,
}

/// The mods' menus.
pub fn menus() -> Vec<Menu> {
    SHARED.state.lock().unwrap().menus.clone()
}

/// Whether a mod has a menu.
pub fn has_menus() -> bool {
    !SHARED.state.lock().unwrap().menus.is_empty()
}

/// The player changed or pressed an item: the menu shows it at once, the
/// mod hears of it at its next turn.
pub fn menu_input(module: &str, item: u32, input: MenuInput) {
    let mut s = SHARED.state.lock().unwrap();
    for m in s.menus.iter_mut().filter(|m| m.module == module) {
        for i in m.items.iter_mut().filter(|i| i.id == item) {
            match (&mut i.kind, input) {
                (MenuKind::Toggle(on), MenuInput::Toggle(v)) => *on = v,
                (MenuKind::Slider { value, min, max, .. }, MenuInput::Value(v)) => *value = v.clamp(*min, *max),
                (MenuKind::Choice { options, index }, MenuInput::Choose(v)) if v < options.len() => *index = v,
                _ => {}
            }
        }
    }
    s.inputs.push((module.to_string(), item, input));
    drop(s);
    SHARED.pending.store(true, Ordering::Release);
    SHARED.generation.fetch_add(1, Ordering::Relaxed);
}

/// What the mods notified since the last call: (mod, text).
pub fn take_notices() -> Vec<(String, String)> {
    SHARED.state.lock().unwrap().notices.drain(..).collect()
}

/// Whether a native trapped: the game may now misbehave.
pub fn tainted() -> bool {
    natives::TAINTED.load(Ordering::Relaxed)
}

pub(crate) fn log_line(module: &str, level: LogLevel, text: &str) {
    match level {
        LogLevel::Info => log::info!("mod {module}: {text}"),
        LogLevel::Error => log::error!("mod {module}: {text}"),
    }
    let mut s = SHARED.state.lock().unwrap();
    if s.log.len() == LOG_LINES {
        s.log.pop_front();
    }
    let seq = s.next_seq;
    s.next_seq += 1;
    s.log.push_back(LogLine { seq, time: SystemTime::now(), module: module.to_string(), level, text: text.to_string() });
}
