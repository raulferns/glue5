//! The catalog: every file a client may read, as the server has it.
//!
//! The engine's files (`data/…`, its `/game`) come first: their index is the
//! file id the engine uses (the manifest it gets lists them in this order).
//! The build's translated shaders (`b/<build>/shaders/…`) follow; the
//! client keeps them as plain files for the renderer.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the game folder, `/`-separated (`data/x64/…`).
    pub path: String,
    pub size: u64,
    /// Seconds since the epoch.
    pub mtime: u64,
}

pub struct Catalog {
    pub build: String,
    pub entries: Vec<Entry>,
    /// The first `data_files` entries are the engine's.
    pub data_files: usize,
    /// Changes whenever a path, size or time does.
    pub hash: String,
    lookup: HashMap<String, u32>,
}

/// Files of the whale5 web build's server beside the game's data, not
/// game files.
const NOT_GAME: [&str; 3] = ["manifest.json", "bootset.json", "bootset_low.json"];

impl Catalog {
    /// The game folder `game` (`data/` and `b/<build>/shaders/`).
    pub fn scan(game: &Path, build: &str) -> Result<Catalog> {
        let mut data = Vec::new();
        walk(&game.join("data"), "data", &mut data).with_context(|| format!("listing {}", game.join("data").display()))?;
        data.retain(|e| !NOT_GAME.iter().any(|n| e.path.strip_prefix("data/") == Some(*n)));
        data.sort_by(|a, b| a.path.cmp(&b.path));
        let shader_dir = format!("b/{build}/shaders");
        let mut shaders = Vec::new();
        walk(&game.join(&shader_dir), &shader_dir, &mut shaders)
            .with_context(|| format!("listing {}", game.join(&shader_dir).display()))?;
        // The packs repeat the shaders for the browser; the renderer reads the files.
        shaders.retain(|e| !e.path.ends_with(".bin"));
        shaders.sort_by(|a, b| a.path.cmp(&b.path));
        if !shaders.iter().any(|e| e.path.ends_with("/index.json")) {
            bail!("{}/index.json is missing", game.join(&shader_dir).display());
        }
        let data_files = data.len();
        data.extend(shaders);
        Ok(Catalog::new(build.to_string(), data, data_files))
    }

    pub fn new(build: String, entries: Vec<Entry>, data_files: usize) -> Catalog {
        let mut sha = Sha256::new();
        sha.update(build.as_bytes());
        for e in &entries {
            sha.update(e.path.as_bytes());
            sha.update([0]);
            sha.update(e.size.to_le_bytes());
            sha.update(e.mtime.to_le_bytes());
        }
        let hash = hex(&sha.finalize()[..8]);
        let lookup = entries.iter().enumerate().map(|(i, e)| (e.path.to_ascii_lowercase(), i as u32)).collect();
        Catalog { build, entries, data_files, hash, lookup }
    }

    /// The id of the file at `path` (relative to the game folder, any case).
    pub fn find(&self, path: &str) -> Option<u32> {
        self.lookup.get(&path.to_ascii_lowercase()).copied()
    }

    pub fn to_json(&self) -> Vec<u8> {
        let files: Vec<_> = self.entries.iter().map(|e| (&e.path, e.size, e.mtime)).collect();
        serde_json::to_vec(&serde_json::json!({
            "build": self.build, "data_files": self.data_files, "files": files,
        }))
        .expect("catalog json")
    }

    pub fn from_json(bytes: &[u8]) -> Result<Catalog> {
        let v: serde_json::Value = serde_json::from_slice(bytes).context("parsing the catalog")?;
        let build = v["build"].as_str().context("catalog without build")?.to_string();
        let data_files = v["data_files"].as_u64().context("catalog without data_files")? as usize;
        let mut entries = Vec::new();
        for f in v["files"].as_array().context("catalog without files")? {
            let (Some(path), Some(size), Some(mtime)) = (f[0].as_str(), f[1].as_u64(), f[2].as_u64()) else {
                bail!("bad catalog entry {f}");
            };
            if !safe_path(path) {
                bail!("catalog path {path:?} leaves the game folder");
            }
            entries.push(Entry { path: path.to_string(), size, mtime });
        }
        if data_files > entries.len() {
            bail!("catalog: {data_files} data files of {}", entries.len());
        }
        Ok(Catalog::new(build, entries, data_files))
    }

    /// The manifest the engine reads (`wasm_httpfs_manifest_js`), in the
    /// whale5 server's format: `/game/` mounted on `data/`.
    pub fn engine_manifest(&self) -> Vec<u8> {
        let files: Vec<_> = self.entries[..self.data_files]
            .iter()
            .map(|e| (e.path.strip_prefix("data/").unwrap_or(&e.path), e.size, e.mtime))
            .collect();
        serde_json::to_vec(&serde_json::json!({ "mount": "/game/", "version": self.hash, "files": files }))
            .expect("manifest json")
    }
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<Entry>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = format!("{rel}/{name}");
        let meta = fs::metadata(entry.path())?;
        if meta.is_dir() {
            walk(&entry.path(), &path, out)?;
        } else if meta.is_file() {
            let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            out.push(Entry { path, size: meta.len(), mtime });
        }
    }
    Ok(())
}

/// Relative, `/`-separated, without `.`/`..`/empty parts or a drive.
pub fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains(':')
        && path.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn round_trip() {
    let entries = vec![
        Entry { path: "data/common/a.xml".into(), size: 3, mtime: 7 },
        Entry { path: "b/x/shaders/index.json".into(), size: 2, mtime: 9 },
    ];
    let c = Catalog::new("x".into(), entries.clone(), 1);
    let back = Catalog::from_json(&c.to_json()).unwrap();
    assert_eq!(back.entries, entries);
    assert_eq!(back.hash, c.hash);
    assert_eq!(back.find("DATA/Common/A.xml"), Some(0));
    let m: serde_json::Value = serde_json::from_slice(&c.engine_manifest()).unwrap();
    assert_eq!(m["files"][0][0], "common/a.xml");
    assert_eq!(m["files"].as_array().unwrap().len(), 1);
    assert!(!safe_path("data/../x") && !safe_path("/etc") && !safe_path("c:/x") && safe_path("data/x"));
}
