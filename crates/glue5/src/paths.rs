//! Where things are, per platform.
//!
//! - The user directory (settings, saves, caches) is SDL's per-user
//!   application directory: `~/Library/Application Support/glue5` on macOS,
//!   `~/.local/share/glue5` on Linux, `%APPDATA%\glue5` on Windows, the app's
//!   sandbox on iOS and Android.
//! - The game (`b/<build>/` and `data/`, ~21 GB) is never part of the app: it
//!   is wherever the user put it. Mobile defaults are user-visible places
//!   (iOS: the app's Documents, through Finder or Files; Android: the app's
//!   external files directory). The Apple TV has none: it keeps only caches
//!   the system may empty, and the game always streams from a server.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Phones, tablets and the Apple TV: apps the system runs full screen and
/// starts without arguments, which cannot quit or start themselves again.
pub const MOBILE: bool = cfg!(any(target_os = "ios", target_os = "android", target_os = "tvos"));

/// A touch screen is the main input (phones and tablets): the touch
/// controls and their settings button.
pub const TOUCH: bool = cfg!(any(target_os = "ios", target_os = "android"));

/// The Apple TV: no game folder (what it keeps, the system may delete); the
/// game streams from a server.
pub const TV: bool = cfg!(target_os = "tvos");

pub fn user_dir() -> Result<PathBuf> {
    sdl3::filesystem::get_pref_path("", "glue5").map_err(|e| anyhow::anyhow!("no user directory: {e}"))
}

/// Where the game is looked for when nothing says otherwise.
pub fn default_game_dirs() -> Vec<PathBuf> {
    if TV {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    if cfg!(target_os = "ios") {
        if let Ok(docs) = sdl3::filesystem::get_user_folder(sdl3::filesystem::Folder::DOCUMENTS) {
            dirs.push(docs.join("game"));
        }
    } else if cfg!(target_os = "android") {
        #[cfg(target_os = "android")]
        if let Some(ext) = android_external_dir() {
            dirs.push(ext.join("game"));
        }
    } else {
        // A distributed Mac app can sit beside an external game directory.
        // SDL's base path is inside the bundle, not beside the .app.
        #[cfg(target_os = "macos")]
        if let Ok(exe) = std::env::current_exe()
            && let Some(bundle) = exe.parent().and_then(Path::parent).and_then(Path::parent)
            && bundle.extension().is_some_and(|ext| ext == "app")
            && let Some(parent) = bundle.parent()
        {
            dirs.push(parent.join("game"));
        }
        // Likewise beside an AppImage: the executable runs from where it is
        // mounted, a temporary directory; its runtime says where it is.
        #[cfg(target_os = "linux")]
        if let Some(appimage) = std::env::var_os("APPIMAGE").map(PathBuf::from)
            && let Some(parent) = appimage.parent()
        {
            dirs.push(parent.join("game"));
        }
        // Next to the executable, the repository's ./game (target/<profile>/glue5),
        // and the working directory.
        if let Ok(base) = sdl3::filesystem::get_base_path() {
            dirs.push(base.join("game"));
            dirs.push(base.join("../../game"));
        }
        dirs.push(PathBuf::from("game"));
    }
    dirs
}

#[cfg(target_os = "android")]
fn android_external_dir() -> Option<PathBuf> {
    let p = unsafe { sdl3::sys::system::SDL_GetAndroidExternalStoragePath() };
    (!p.is_null()).then(|| PathBuf::from(unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()))
}

/// The app's cache folder: what can be fetched again (the stream's copy of
/// the game, the packs this computer shares). iOS and tvOS may empty it
/// while the app is not running; the stream then fetches again.
pub fn cache_dir(user_dir: &Path) -> PathBuf {
    // SDL's user directory there is Library/Application Support/glue5 (on
    // tvOS Library/Caches/glue5, which is this one).
    #[cfg(any(target_os = "ios", target_os = "tvos"))]
    if let Some(library) = user_dir.parent().and_then(Path::parent) {
        return library.join("Caches").join("glue5");
    }
    #[cfg(target_os = "android")]
    if let Some(ext) = android_external_dir() {
        return ext.join("cache");
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Library/Caches/glue5");
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("glue5").join("cache");
    }
    #[cfg(target_os = "linux")]
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    {
        return cache.join("glue5");
    }
    user_dir.join("cache")
}

/// The stream's copy of the game (a game folder's layout, partial).
pub fn stream_copy(user_dir: &Path) -> PathBuf {
    cache_dir(user_dir).join("stream").join(glue5_runtime::GAME_BUILD)
}

/// Where the whole game goes when it is downloaded from a server: where a
/// copied game would be (`Documents/game`, the external files' `game`, the
/// user directory's `game` on desktop). None on tvOS, which keeps nothing.
pub fn download_dir(user_dir: &Path) -> Option<PathBuf> {
    if !can_download_all() {
        None
    } else if MOBILE {
        default_game_dirs().into_iter().next()
    } else {
        Some(user_dir.join("game"))
    }
}

/// The device keeps files for good (tvOS keeps only caches).
pub fn can_download_all() -> bool {
    !TV
}

/// Where the game's files go, for the start page when they are missing.
pub fn where_the_game_goes() -> &'static str {
    if cfg!(target_os = "ios") {
        "Copy the game folder into the app's files, named game: Finder (iPhone › Files › Glue 5) \
         or the Files app (On My iPhone › Glue 5). This page looks again when the app comes back."
    } else if cfg!(target_os = "android") {
        "Copy the game folder, named game, into Android/data/<app>/files/ on the device."
    } else {
        "Choose the game folder, or start glue5 with its path."
    }
}

/// How the game streams instead, for the start page.
pub fn or_stream() -> &'static str {
    if MOBILE {
        "Or play it from a device that has it, over the network."
    } else {
        "Or play it from another device that has it, over the network."
    }
}

/// What the game folder holds.
pub fn game_layout() -> String {
    format!("game/\n  b/{}/shaders/\n  data/", glue5_runtime::GAME_BUILD)
}

pub fn canonical(dir: &Path) -> Result<PathBuf> {
    dir.canonicalize().with_context(|| format!("{}", dir.display()))
}
