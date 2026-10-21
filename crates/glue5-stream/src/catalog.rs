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
    /// The dates the engine is told for its files (the first `data_files`;
    /// none: their times), see [`game_dates`]. Not in the hash: the copy's
    /// bytes are the same whatever date the engine is told.
    pub dates: Vec<u64>,
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
        let game = game_dates(&game.join("data"));
        let dates = data
            .iter()
            .map(|e| date(&game, e.path.strip_prefix("data/").unwrap_or(&e.path), e.size).unwrap_or(e.mtime))
            .collect();
        data.extend(shaders);
        Ok(Catalog { dates, ..Catalog::new(build.to_string(), data, data_files) })
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
        Catalog { build, entries, data_files, hash, dates: Vec::new(), lookup }
    }

    /// The id of the file at `path` (relative to the game folder, any case).
    pub fn find(&self, path: &str) -> Option<u32> {
        self.lookup.get(&path.to_ascii_lowercase()).copied()
    }

    pub fn to_json(&self) -> Vec<u8> {
        let files: Vec<_> = self.entries.iter().map(|e| (&e.path, e.size, e.mtime)).collect();
        serde_json::to_vec(&serde_json::json!({
            "build": self.build, "data_files": self.data_files, "files": files, "dates": self.dates,
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
        // (A catalog of an earlier version has none.)
        let dates: Vec<u64> =
            v["dates"].as_array().map_or(Vec::new(), |d| d.iter().filter_map(|t| t.as_u64()).collect());
        if !dates.is_empty() && dates.len() != data_files {
            bail!("catalog: {} dates for {data_files} data files", dates.len());
        }
        Ok(Catalog { dates, ..Catalog::new(build, entries, data_files) })
    }

    /// The manifest the engine reads (`wasm_httpfs_manifest_js`), in the
    /// whale5 server's format: `/game/` mounted on `data/`.
    pub fn engine_manifest(&self) -> Vec<u8> {
        let files: Vec<_> = self.entries[..self.data_files]
            .iter()
            .enumerate()
            .map(|(i, e)| {
                (e.path.strip_prefix("data/").unwrap_or(&e.path), e.size, *self.dates.get(i).unwrap_or(&e.mtime))
            })
            .collect();
        serde_json::to_vec(&serde_json::json!({ "mount": "/game/", "version": self.hash, "files": files }))
            .expect("manifest json")
    }
}

/// The game's files as the whale5 server listed them, by lowercase path
/// under `data/`: (size, date in seconds), from its manifest
/// (`data/manifest.json`, `files` of [path, size, date]). The engine's map
/// caches (`common/data/gta5_cache_*.dat`) hold these dates: told others
/// (a copy's), it builds the cache again at every start (seconds) and
/// cannot save it. Empty without a manifest.
pub fn game_dates(data: &Path) -> HashMap<String, (u64, u64)> {
    let Ok(bytes) = fs::read(data.join("manifest.json")) else { return HashMap::new() };
    let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        log::warn!("{}: not a manifest", data.join("manifest.json").display());
        return HashMap::new();
    };
    let files = manifest["files"].as_array().map_or(&[][..], Vec::as_slice);
    files.iter().filter_map(|f| Some((f[0].as_str()?.to_ascii_lowercase(), (f[1].as_u64()?, f[2].as_u64()?)))).collect()
}

/// The date of the file at `rel` (under `data/`) of `size` bytes, if the
/// manifest lists it so: a file changed since is not the one it lists.
pub fn date(dates: &HashMap<String, (u64, u64)>, rel: &str, size: u64) -> Option<u64> {
    dates.get(&rel.to_ascii_lowercase()).filter(|(s, _)| *s == size).map(|&(_, t)| t)
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
fn manifest_dates() {
    let dir = std::env::temp_dir().join(format!("glue5-catalog-dates-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.json"), br#"{"mount":"/game/","files":[["x64/A.rpf",10,1790644442],["b.xml",3,9]]}"#)
        .unwrap();
    let dates = game_dates(&dir);
    assert_eq!(date(&dates, "x64/a.rpf", 10), Some(1790644442));
    // Changed since (another size), or not listed.
    assert_eq!(date(&dates, "b.xml", 4), None);
    assert_eq!(date(&dates, "c.xml", 3), None);
    assert!(game_dates(&dir.join("none")).is_empty());
    let _ = fs::remove_dir_all(dir);
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
    assert_eq!(m["files"][0][2], 7);
    assert_eq!(m["files"].as_array().unwrap().len(), 1);
    // The engine's dates: told it, kept, out of the hash.
    let dated = Catalog { dates: vec![5], ..Catalog::new("x".into(), entries.clone(), 1) };
    let back = Catalog::from_json(&dated.to_json()).unwrap();
    assert_eq!((back.dates, back.hash), (vec![5], c.hash.clone()));
    let m: serde_json::Value = serde_json::from_slice(&dated.engine_manifest()).unwrap();
    assert_eq!(m["files"][0][2], 5);
    assert!(!safe_path("data/../x") && !safe_path("/etc") && !safe_path("c:/x") && safe_path("data/x"));
}
