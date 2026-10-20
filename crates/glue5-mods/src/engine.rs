//! Mods in the engine: each runs in a script thread of its own, so its
//! natives run in a real script context (the entities it creates belong to
//! its thread, and the engine cleans them up when the thread ends).
//!
//! The thread's program is ours, built in wasm memory: `loop: WAIT(0)`.
//! Every frame the engine calls WAIT for every script thread; the hook on
//! it resumes the mod's Lua when the thread is a mod's and is about to wait,
//! and writes how long it waits into WAIT's argument. Starting and killing
//! threads happens from the same hook, in whatever thread waits next, so it
//! all stays on the engine's script update.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use glue5_game::{Env, Hooks, orig};
use glue5_runtime::memory::Mem;

use crate::vm::{Jobs, Step, Vm};
use crate::{Command, LogLevel, ModState, SHARED, log_line, natives};

/// The REPL's thread: `eval` runs there, as a mod of its own.
const REPL: &str = "~eval";

pub(crate) fn hooks(h: &mut Hooks) {
    h.script_wait = Some(script_wait);
}

/// The program registry's slots (`scrProgram::GetProgram`): 176 u64.
const PROGRAMS: u64 = 15406680;
const PROGRAM_SLOTS: u64 = 176;
/// The program size we copy from a loaded one; the fields set below are
/// all the engine reads to start and run a thread.
const PROGRAM_SIZE: u64 = 128;
/// scrThread fields.
const THREAD_STATE: u64 = 32;
const STATE_WAITING: u32 = 1;
/// The stack size of the mods' threads, one of gameconfig.xml's
/// (MICRO): a mod keeps its state in Lua.
const STACK: u32 = 128;
/// Opcodes (the b2802+ table).
const OP_ENTER: u8 = 45;
const OP_NATIVE: u8 = 44;
const OP_PUSH_0: u8 = 113;
const OP_J: u8 = 85;

struct Running {
    name: String,
    thread_id: u32,
    thread: u64,
    vm: Vm,
    /// Lua ended (or failed): the thread is killed from the next wait
    /// outside it.
    ending: bool,
    cpu: f32,
    waiting: bool,
}

#[derive(Default)]
struct Engine {
    /// The program text and natives table every mod program shares.
    code: Option<(u64, u64)>,
    /// Program id by mod name.
    programs: HashMap<String, u32>,
    running: Vec<Running>,
    /// The REPL's snippets.
    jobs: Jobs,
    /// The catalog of names was read (once the game has loaded), and when
    /// that was last checked.
    catalog: bool,
    catalog_checked: Option<Instant>,
}

thread_local! {
    static ENGINE: RefCell<Engine> = RefCell::default();
}

unsafe extern "C" fn script_wait(env: *mut Env, info: u64) {
    let killed = ENGINE.with(|engine| {
        // A mod's native that waits would come back here: not for us then.
        let Ok(mut engine) = engine.try_borrow_mut() else { return false };
        unsafe { engine.wait(env, info) }
    });
    if !killed {
        unsafe { orig::script_wait(env, info) }
    }
}

impl Engine {
    /// Returns whether the active thread was killed (WAIT must not run).
    unsafe fn wait(&mut self, env: *mut Env, info: u64) -> bool {
        let mem = unsafe { Mem::of(env) };
        let active = unsafe { orig::script_active_thread(env) };
        if SHARED.pending.swap(false, Ordering::Acquire) {
            let commands = std::mem::take(&mut SHARED.state.lock().unwrap().commands);
            for command in commands {
                unsafe { self.command(env, command) };
            }
        }
        if !self.catalog && self.catalog_checked.is_none_or(|t| t.elapsed().as_secs() >= 1) {
            self.catalog_checked = Some(Instant::now());
            let ready = unsafe {
                natives::call_int(env, "GET_IS_LOADING_SCREEN_ACTIVE") == Some(0)
                    && natives::call_int(env, "IS_SCREEN_FADED_IN") == Some(1)
                    && natives::call_int(env, "PLAYER_PED_ID").is_some_and(|p| p != 0)
            };
            if ready {
                self.catalog = true;
                crate::world::set_catalog(unsafe { crate::world::read_catalog(env) });
            }
        }
        // Ended mods' threads, from outside them.
        let mut i = 0;
        while i < self.running.len() {
            let r = &self.running[i];
            if r.ending && r.thread != active {
                unsafe { orig::script_kill_thread(env, r.thread_id) };
                crate::menus::remove_mod(&r.name);
                self.running.remove(i);
            } else {
                i += 1;
            }
        }
        let Some(r) = self.running.iter_mut().find(|r| r.thread == active) else { return false };
        // WAIT runs every frame while the thread waits; the mod runs when
        // it is about to start one.
        if r.ending || mem.u32(active + THREAD_STATE) == STATE_WAITING {
            return false;
        }
        let start = Instant::now();
        let now = unsafe { natives::call_int(env, "GET_GAME_TIMER") }.unwrap_or(0) as u32 as u64;
        *crate::RUNNING_MOD.lock().unwrap() = r.name.clone();
        let step = natives::with_env(env, || r.vm.step(now));
        crate::RUNNING_MOD.lock().unwrap().clear();
        r.cpu = r.cpu * 0.9 + start.elapsed().as_secs_f32() * 1000.0 * 0.1;
        let ms = match step {
            Step::Wait(ms) => {
                if r.waiting != (ms > 0) {
                    r.waiting = ms > 0;
                    set_waiting(&r.name, r.waiting);
                }
                ms
            }
            Step::Finished => {
                log_line(&r.name, LogLevel::Info, "finished");
                set_state(&r.name, ModState::Stopped, None);
                r.ending = true;
                0
            }
            Step::Failed(e) => {
                log_line(&r.name, LogLevel::Error, &e);
                set_state(&r.name, ModState::Error, Some(e));
                r.ending = true;
                0
            }
        };
        // WAIT's argument, in the thread's stack.
        mem.set_i32(mem.u64(info + 16), ms.min(i32::MAX as u32) as i32);
        if let Ok(mut s) = SHARED.state.lock()
            && let Some(m) = s.mods.iter_mut().find(|m| m.name == r.name)
        {
            m.cpu_ms = r.cpu;
            m.memory = r.vm.memory();
        }
        false
    }

    unsafe fn command(&mut self, env: *mut Env, command: Command) {
        match command {
            Command::Start(name) => {
                if self.running.iter().any(|r| r.name == name && !r.ending) {
                    return;
                }
                if let Err(e) = unsafe { self.start(env, &name) } {
                    log_line(&name, LogLevel::Error, &e);
                    set_state(&name, ModState::Error, Some(e));
                }
            }
            Command::Eval(code, reply) => {
                if !self.running.iter().any(|r| r.name == REPL && !r.ending)
                    && let Err(e) = unsafe { self.start(env, REPL) }
                {
                    let _ = reply.send(Err(e));
                    return;
                }
                self.jobs.borrow_mut().push_back((code, reply));
            }
            Command::Stop(name) => {
                if let Some(r) = self.running.iter_mut().find(|r| r.name == name) {
                    r.ending = true;
                    log_line(&name, LogLevel::Info, "stopped");
                    set_state(&name, ModState::Stopped, None);
                }
            }
        }
    }

    unsafe fn start(&mut self, env: *mut Env, name: &str) -> Result<(), String> {
        let dir = SHARED.dir.get().ok_or("no mods folder")?.join(name);
        let log_name = if name == REPL { "eval".to_string() } else { name.to_string() };
        let log: Rc<dyn Fn(&str)> = Rc::new(move |text| log_line(&log_name, LogLevel::Info, text));
        let vm = if name == REPL { Vm::repl(self.jobs.clone(), log) } else { Vm::new(name, &dir, log) }
            .map_err(|e| e.to_string())?;
        let program = unsafe { self.program(env, name)? };
        let thread_name = unsafe { wasm_string(env, &format!("glue5:{name}")) };
        let thread_id = unsafe { orig::script_start_program(env, program, 0, 0, STACK as i32 as u32, thread_name) };
        if thread_id == 0 {
            return Err("the game has no free script thread for it".into());
        }
        let thread = unsafe { orig::script_get_thread(env, thread_id) };
        if thread == 0 {
            return Err("its script thread is gone".into());
        }
        self.running.push(Running {
            name: name.to_string(),
            thread_id,
            thread,
            vm,
            ending: false,
            cpu: 0.0,
            waiting: false,
        });
        if name != REPL {
            log_line(name, LogLevel::Info, "started");
            set_state(name, ModState::Running, None);
        }
        Ok(())
    }

    /// The program of mod `name`'s thread (its name is the thread's), made
    /// once: a copy of a loaded program with our code, natives and name.
    unsafe fn program(&mut self, env: *mut Env, name: &str) -> Result<u32, String> {
        if let Some(&id) = self.programs.get(name) {
            return Ok(id);
        }
        let mem = unsafe { Mem::of(env) };
        let template =
            (0..PROGRAM_SLOTS).map(|i| mem.u64(PROGRAMS + 8 * i)).find(|&p| p != 0).ok_or("no script program is loaded yet")?;
        let (pages, natives) = match self.code {
            Some(code) => code,
            None => {
                let wait = natives::reserved_handler("WAIT").ok_or("no WAIT native")?;
                // ENTER 0 params, 2 locals, no name; loop: PUSH_0; NATIVE
                // 1 argument, no result, natives[0]; J loop.
                let code: [u8; 13] = [OP_ENTER, 0, 2, 0, 0, OP_PUSH_0, OP_NATIVE, 1 << 2, 0, 0, OP_J, 0xF8, 0xFF];
                let text = unsafe { malloc(env, 64)? };
                mem.bytes_mut(text, 64).fill(0);
                mem.bytes_mut(text, code.len()).copy_from_slice(&code);
                let pages = unsafe { malloc(env, 8)? };
                mem.set_u64(pages, text);
                let natives = unsafe { malloc(env, 8)? };
                mem.set_u64(natives, wait);
                self.code = Some((pages, natives));
                (pages, natives)
            }
        };
        let id = natives::joaat(&format!("glue5:{name}"));
        let program = unsafe { malloc(env, PROGRAM_SIZE)? };
        mem.bytes_mut(program, PROGRAM_SIZE as usize).copy_from_slice(mem.bytes(template, PROGRAM_SIZE as usize));
        mem.set_u64(program + 16, pages); // code pages
        mem.set_u32(program + 28, 13); // code size
        mem.set_u32(program + 36, 0); // statics
        mem.set_u64(program + 48, 0);
        mem.set_u32(program + 44, 1); // natives
        mem.set_u64(program + 64, natives);
        mem.set_u32(program + 72, 0);
        mem.set_u32(program + 88, id);
        mem.set_u32(program + 92, 1 << 24); // references: never released
        mem.set_u64(program + 96, unsafe { wasm_string(env, &format!("glue5:{name}")) });
        mem.set_u64(program + 104, 0); // strings
        mem.set_u32(program + 112, 0);
        unsafe { orig::script_insert_program(env, program) };
        if unsafe { orig::script_get_program(env, id) } != program {
            return Err("the game's program registry is full".into());
        }
        self.programs.insert(name.to_string(), id);
        Ok(id)
    }
}

unsafe fn malloc(env: *mut Env, size: u64) -> Result<u64, String> {
    match unsafe { orig::builtin_malloc(env, size) } {
        0 => Err("out of game memory".into()),
        at => Ok(at),
    }
}

/// A NUL-terminated copy of `s` in the game's heap (kept: names of
/// programs and threads).
unsafe fn wasm_string(env: *mut Env, s: &str) -> u64 {
    let Ok(at) = (unsafe { malloc(env, s.len() as u64 + 1) }) else { return 0 };
    unsafe { Mem::of(env) }.write_str(at, s, s.len() + 1);
    at
}

fn set_state(name: &str, state: ModState, error: Option<String>) {
    if let Ok(mut s) = SHARED.state.lock()
        && let Some(m) = s.mods.iter_mut().find(|m| m.name == name)
    {
        m.state = state;
        m.error = error;
        m.waiting = false;
        if state != ModState::Running {
            m.cpu_ms = 0.0;
        }
        SHARED.generation.fetch_add(1, Ordering::Relaxed);
    }
}

fn set_waiting(name: &str, waiting: bool) {
    if let Ok(mut s) = SHARED.state.lock()
        && let Some(m) = s.mods.iter_mut().find(|m| m.name == name)
    {
        m.waiting = waiting;
        SHARED.generation.fetch_add(1, Ordering::Relaxed);
    }
}
