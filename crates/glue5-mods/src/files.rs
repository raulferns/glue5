//! The mods' files, for the Studio and MCP: paths are `<mod>/<file>`
//! inside the mods folder, never outside it.

use std::path::{Component, Path, PathBuf};

const TEMPLATE: &str = "--!strict\n-- Runs once the player is in the game. A loop needs wait() to let the game go on.\n\nwhile true do\n    local me = player.playerPedId()\n    if pad.isControlJustPressed(0, 51) then -- E\n        print(\"position\", entity.getEntityCoords(me, true))\n    end\n    wait()\nend\n";

/// What a mod's name may be: a folder name that is the same everywhere.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The file at `path` (`<mod>/<file>`), checked to stay in the mods folder.
pub fn resolve(path: &str) -> Result<PathBuf, String> {
    let dir = crate::dir().ok_or("mods are off")?;
    let rel = Path::new(path);
    let mut parts = 0;
    for c in rel.components() {
        match c {
            Component::Normal(p) if !p.to_string_lossy().starts_with('.') => parts += 1,
            _ => return Err(format!("{path}: not a file of a mod")),
        }
    }
    if parts < 2 {
        return Err(format!("{path}: expected <mod>/<file>"));
    }
    let name = rel.components().next().unwrap().as_os_str().to_string_lossy();
    if !valid_name(&name) {
        return Err(format!("{name}: a mod's name is lowercase letters, digits, - and _"));
    }
    Ok(dir.join(rel))
}

/// The files of mod `name`, as `<mod>/<file>` paths, sorted.
pub fn list(name: &str) -> Vec<String> {
    let Some(dir) = crate::dir() else { return Vec::new() };
    let mut out = Vec::new();
    fn walk(root: &Path, at: &Path, out: &mut Vec<String>, depth: usize) {
        let Ok(entries) = std::fs::read_dir(at) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if p.is_dir() && depth < 4 {
                walk(root, &p, out, depth + 1);
            } else if p.is_file()
                && let Ok(rel) = p.strip_prefix(root)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    walk(dir, &dir.join(name), &mut out, 0);
    out.sort();
    out
}

pub fn read(path: &str) -> Result<String, String> {
    let p = resolve(path)?;
    std::fs::read_to_string(&p).map_err(|e| format!("{path}: {e}"))
}

/// Writes `path` whole (a temporary file renamed over it).
pub fn write(path: &str, text: &str) -> Result<(), String> {
    let p = resolve(path)?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{path}: {e}"))?;
    }
    let tmp = p.with_extension("glue5-tmp");
    std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &p)).map_err(|e| format!("{path}: {e}"))?;
    crate::changed(path);
    crate::refresh();
    Ok(())
}

pub fn delete(path: &str) -> Result<(), String> {
    let p = resolve(path)?;
    std::fs::remove_file(&p).map_err(|e| format!("{path}: {e}"))?;
    crate::changed(path);
    crate::refresh();
    Ok(())
}

pub fn rename(from: &str, to: &str) -> Result<(), String> {
    let (a, b) = (resolve(from)?, resolve(to)?);
    if b.exists() {
        return Err(format!("{to} exists"));
    }
    std::fs::rename(&a, &b).map_err(|e| format!("{from}: {e}"))?;
    crate::changed(from);
    crate::changed(to);
    crate::refresh();
    Ok(())
}

/// A new mod with a `main.luau` to start from.
pub fn create_mod(name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("{name}: a mod's name is lowercase letters, digits, - and _"));
    }
    let main = resolve(&format!("{name}/main.luau"))?;
    if main.exists() {
        return Err(format!("{name} exists"));
    }
    write(&format!("{name}/main.luau"), TEMPLATE)?;
    write(&format!("{name}/mod.json"), "{\n  \"autostart\": false\n}\n")
}

/// Whether the mod in `dir` starts with the game (its mod.json).
pub fn autostart(dir: &Path) -> bool {
    std::fs::read(dir.join("mod.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["autostart"].as_bool())
        .unwrap_or(false)
}

/// Sets whether mod `name` starts with the game, keeping the rest of its mod.json.
pub fn set_autostart(name: &str, on: bool) -> Result<(), String> {
    let path = format!("{name}/mod.json");
    let mut v: serde_json::Value = read(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if !v.is_object() {
        v = serde_json::json!({});
    }
    v["autostart"] = on.into();
    write(&path, &(serde_json::to_string_pretty(&v).unwrap() + "\n"))
}
