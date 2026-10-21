//! The game's frames no longer outrun its audio thread into a 250 ms stall.
//!
//! Each frame the main thread starts the game's audio update
//! (`audNorthAudioEngine::StartUpdate`), which runs on a thread of its own
//! and is waited for at the end of the frame. It fills buffer `count & 3`
//! of four for the audio thread (`rage::audEngine`), then counts the frame;
//! the audio thread, about every 10 ms, reads the newest buffer, `(count -
//! 1) & 3`.
//!
//! With the audio thread two frames behind, the next update would fill the
//! buffer it still reads, and `audController::UpdateEntityRef` waits for it
//! to move on, polling, from inside the update. Meanwhile the update's own
//! commands never reach the mixer, and the audio thread cannot finish a
//! frame without them: both stood still until the audio thread gave up on
//! the mixer, 250 ms later, the main thread waiting all along. Frames faster
//! than the audio thread's did it: the pause menu (150-200 fps on a desktop
//! GPU) stuttered several times a second, every menu opened or page turned.
//! The browser build never draws faster than the display.
//!
//! The frame now waits for the audio thread before the update starts, while
//! the audio thread can still finish its frame: a few milliseconds, only
//! when frames are that fast.
//!
//! The main thread's wait for the update at the end of the frame is timed
//! for the stats (`audio::take_stats`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use glue5_game::{Env, orig};

use crate::memory::Mem;

/// The game's audio frames, counted by the update. Addresses in this build.
const FRAME_COUNT: u64 = 12034076;
/// The buffer the audio thread reads: `(count - 1) & 3` as of its last frame.
const READING: u64 = 12114820;
/// How long a frame waits at most: the audio thread runs every 10 ms.
const MAX_WAIT: Duration = Duration::from_millis(30);

/// Hook for `audNorthAudioEngine::StartUpdate()`.
pub(crate) unsafe extern "C" fn start_update(env: *mut Env) {
    let mem = unsafe { Mem::of(env) };
    let two_behind = || {
        let count = mem.atomic_u32(FRAME_COUNT).load(Ordering::Acquire);
        let reading = mem.atomic_u32(READING).load(Ordering::Acquire);
        count.wrapping_sub(reading) & 3 == 3
    };
    let deadline = Instant::now() + MAX_WAIT;
    while two_behind() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_micros(500));
    }
    unsafe { orig::audio_start_update(env) }
}

/// The semaphore the update's thread signals as it finishes the frame's
/// update (a pointer to it), which `FinishUpdate` alone waits on. An
/// address in this build.
const UPDATE_FINISHED: u64 = 15715088;

/// The main thread's waits for the update: how many, their time and the
/// longest (ns).
static WAITS: AtomicU64 = AtomicU64::new(0);
static WAITED: AtomicU64 = AtomicU64::new(0);
static LONGEST: AtomicU64 = AtomicU64::new(0);

/// Hook for `rage::sysIpcWaitSema(rage::sysIpcSemaTag*, unsigned int)`:
/// times the waits for the update.
pub(crate) unsafe extern "C" fn wait_sema(env: *mut Env, sema: u64, count: u32) {
    if sema == 0 || sema != unsafe { Mem::of(env) }.u64(UPDATE_FINISHED) {
        return unsafe { orig::wait_sema(env, sema, count) };
    }
    let t = Instant::now();
    unsafe { orig::wait_sema(env, sema, count) };
    let ns = t.elapsed().as_nanos() as u64;
    WAITS.fetch_add(1, Ordering::Relaxed);
    WAITED.fetch_add(ns, Ordering::Relaxed);
    LONGEST.fetch_max(ns, Ordering::Relaxed);
}

/// The frames' waits for the game's audio update since the last call: how
/// many, how long in all, the longest.
pub(crate) fn take_waits() -> (u64, Duration, Duration) {
    let ns = |a: &AtomicU64| Duration::from_nanos(a.swap(0, Ordering::Relaxed));
    (WAITS.swap(0, Ordering::Relaxed), ns(&WAITED), ns(&LONGEST))
}

/// This module's hooks.
pub(crate) fn hooks(hooks: &mut glue5_game::Hooks) {
    hooks.audio_start_update = Some(start_update);
    hooks.wait_sema = Some(wait_sema);
}
