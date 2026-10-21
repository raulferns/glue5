//! The lobby's jobs: each player's go over to the others by themselves.
//!
//! Every few seconds a player announces the jobs it made or got (an id, a
//! fingerprint of the file, its size) on a port of the lobby's virtual LAN
//! that the engine does not use; a player without one (or with another
//! version) asks the announcer for its pieces, a few at a time, again when
//! a piece does not come. Received whole and matching its fingerprint, the
//! file goes to `jobs/shared/` and into the library: when someone hosts a
//! job, the others have it already, as the scripts expect (they find a
//! job's header and its file at once).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{Job, Kind, MAX_FILE, add, fingerprint, is_content_id, library, parse_job, shared_dir};

use crate::Locked;

/// The port on the virtual LAN (the engine's sockets take 6672 and 50000 up).
pub(crate) const PORT: u16 = 47900;
const BROADCAST: u32 = u32::MAX;
const ANNOUNCE: u8 = b'A';
const GET: u8 = b'G';
const PIECE: u8 = b'P';
/// A piece's size, inside a datagram.
const PIECE_SIZE: usize = 1024;
/// Jobs in one announcement: (22 + 8 + 4) bytes each.
const PER_ANNOUNCEMENT: usize = 32;
const ANNOUNCE_EVERY: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(100);
/// Pieces asked for at a time, and when one is asked for again.
const WINDOW: usize = 16;
const RETRY: Duration = Duration::from_millis(600);
/// A download whose announcer says nothing for this long is dropped.
const GIVE_UP: Duration = Duration::from_secs(30);
/// Jobs downloaded at once, and at once from one player (the others wait
/// for a later announcement).
const DOWNLOADS_AT_ONCE: usize = 8;
const DOWNLOADS_PER_PLAYER: usize = 2;

struct Download {
    hash: [u8; 8],
    from: u32,
    pieces: Vec<Option<Vec<u8>>>,
    asked: Vec<Option<Instant>>,
    heard: Instant,
}

static DOWNLOADS: Mutex<Option<HashMap<String, Download>>> = Mutex::new(None);

pub(super) fn start() {
    let started = std::thread::Builder::new().name("jobs".into()).spawn(|| {
        let mut announced = Instant::now() - ANNOUNCE_EVERY;
        loop {
            std::thread::sleep(TICK);
            if crate::p2p::get().is_none() {
                continue;
            }
            if announced.elapsed() >= ANNOUNCE_EVERY {
                announced = Instant::now();
                announce();
            }
            ask();
        }
    });
    if let Err(e) = started {
        log::warn!("jobs: sharing: {e}");
    }
}

fn send(to: u32, payload: &[u8]) {
    if let Some(p2p) = crate::p2p::get() {
        p2p.send(PORT, to, PORT, payload);
    }
}

/// The jobs the lobby gets from this player: its own and those it got.
fn shareable(job: &Job) -> bool {
    job.kind != Kind::Game && job.size as usize <= MAX_FILE
}

fn announce() {
    // Ids, fingerprints and sizes only (the headers are up to MBs each).
    let jobs: Vec<(String, [u8; 8], u32)> =
        library().as_ref().unwrap().iter().filter(|j| shareable(j)).map(|j| (j.id.clone(), j.hash, j.size)).collect();
    for chunk in jobs.chunks(PER_ANNOUNCEMENT) {
        let mut d = vec![ANNOUNCE];
        for (id, hash, size) in chunk {
            d.extend_from_slice(id.as_bytes());
            d.extend_from_slice(hash);
            d.extend_from_slice(&size.to_be_bytes());
        }
        send(BROADCAST, &d);
    }
}

/// Asks for the pieces missing, a window at a time.
fn ask() {
    let now = Instant::now();
    let mut downloads = DOWNLOADS.locked();
    let Some(downloads) = downloads.as_mut() else { return };
    downloads.retain(|id, d| {
        if now.duration_since(d.heard) > GIVE_UP {
            log::info!("jobs: {id} from {}: gave up", std::net::Ipv4Addr::from(d.from));
            return false;
        }
        true
    });
    for (id, d) in downloads.iter_mut() {
        let waiting = d.asked.iter().zip(&d.pieces).filter(|(a, p)| p.is_none() && a.is_some_and(|t| now.duration_since(t) < RETRY)).count();
        let mut room = WINDOW.saturating_sub(waiting);
        for i in 0..d.pieces.len() {
            if room == 0 {
                break;
            }
            if d.pieces[i].is_some() || d.asked[i].is_some_and(|t| now.duration_since(t) < RETRY) {
                continue;
            }
            let mut g = vec![GET];
            g.extend_from_slice(id.as_bytes());
            g.extend_from_slice(&d.hash);
            g.extend_from_slice(&(i as u32).to_be_bytes());
            send(d.from, &g);
            d.asked[i] = Some(now);
            room -= 1;
        }
    }
}

/// A datagram on the jobs' port, from `from` (vnet.rs).
pub(crate) fn receive(from: u32, d: &[u8]) {
    match d.first() {
        Some(&ANNOUNCE) => on_announce(from, &d[1..]),
        Some(&GET) if d.len() >= 1 + 22 + 8 + 4 => {
            let (id, hash) = (&d[1..23], &d[23..31]);
            let index = u32::from_be_bytes(d[31..35].try_into().unwrap()) as usize;
            on_get(from, id, hash, index);
        }
        Some(&PIECE) if d.len() >= 1 + 22 + 8 + 8 => {
            let (id, hash) = (&d[1..23], &d[23..31]);
            let index = u32::from_be_bytes(d[31..35].try_into().unwrap()) as usize;
            let count = u32::from_be_bytes(d[35..39].try_into().unwrap()) as usize;
            on_piece(from, id, hash, index, count, &d[39..]);
        }
        _ => {}
    }
}

fn on_announce(from: u32, d: &[u8]) {
    let have: Vec<(String, Kind, [u8; 8])> =
        library().as_ref().unwrap().iter().map(|j| (j.id.clone(), j.kind, j.hash)).collect();
    let mut downloads = DOWNLOADS.locked();
    let downloads = downloads.get_or_insert_default();
    for e in d.as_chunks::<34>().0 {
        let Ok(id) = std::str::from_utf8(&e[..22]) else { continue };
        let hash: [u8; 8] = e[22..30].try_into().unwrap();
        let size = u32::from_be_bytes(e[30..34].try_into().unwrap()) as usize;
        if !is_content_id(id) || size == 0 || size > MAX_FILE {
            continue;
        }
        // The player's own jobs and the game's stay as they are; another
        // version only of one shared before (a version already here stays).
        if have.iter().any(|(i, kind, h)| i == id && (*kind != Kind::Shared || *h == hash)) {
            continue;
        }
        // A few at a time, from each player and in all: each takes room
        // for its pieces (MBs from announcements alone otherwise).
        let full = downloads.len() >= DOWNLOADS_AT_ONCE
            || downloads.values().filter(|d| d.from == from).count() >= DOWNLOADS_PER_PLAYER;
        match downloads.get_mut(id) {
            Some(d) if d.hash == hash => d.heard = Instant::now(),
            None if full => {}
            _ => {
                let count = size.div_ceil(PIECE_SIZE);
                log::info!("jobs: {id} ({size} bytes) from {}", std::net::Ipv4Addr::from(from));
                downloads.insert(
                    id.to_owned(),
                    Download { hash, from, pieces: vec![None; count], asked: vec![None; count], heard: Instant::now() },
                );
            }
        }
    }
}

fn on_get(from: u32, id: &[u8], hash: &[u8], index: usize) {
    let Ok(id) = std::str::from_utf8(id) else { return };
    // Looked up in place: pieces are asked for up to 160 times a second.
    let Some(path) = library()
        .as_ref()
        .unwrap()
        .iter()
        .find(|j| shareable(j) && j.id == id && j.hash == hash)
        .map(|j| j.path.clone())
    else {
        return;
    };
    // Read and checked once, then kept while the pieces are asked for (up
    // to 160 a second per player: reading and hashing the whole file for
    // each held up the network thread).
    static SERVING: Mutex<Option<(String, [u8; 8], Arc<Vec<u8>>)>> = Mutex::new(None);
    let data = {
        let mut serving = SERVING.locked();
        match &*serving {
            Some((i, h, d)) if i == id && h == hash => d.clone(),
            _ => {
                let Ok(d) = std::fs::read(&path) else { return };
                if fingerprint(&d) != hash {
                    return;
                }
                let d = Arc::new(d);
                *serving = Some((id.to_owned(), hash.try_into().unwrap(), d.clone()));
                d
            }
        }
    };
    let count = data.len().div_ceil(PIECE_SIZE);
    let Some(piece) = data.chunks(PIECE_SIZE).nth(index) else { return };
    let mut p = vec![PIECE];
    p.extend_from_slice(id.as_bytes());
    p.extend_from_slice(hash);
    p.extend_from_slice(&(index as u32).to_be_bytes());
    p.extend_from_slice(&(count as u32).to_be_bytes());
    p.extend_from_slice(piece);
    send(from, &p);
}

fn on_piece(from: u32, id: &[u8], hash: &[u8], index: usize, count: usize, piece: &[u8]) {
    let Ok(id) = std::str::from_utf8(id) else { return };
    let mut guard = DOWNLOADS.locked();
    let Some(downloads) = guard.as_mut() else { return };
    let Some(d) = downloads.get_mut(id) else { return };
    if d.hash != hash || d.from != from || count != d.pieces.len() || index >= count {
        return;
    }
    d.heard = Instant::now();
    d.pieces[index] = Some(piece.to_vec());
    if d.pieces.iter().any(Option::is_none) {
        return;
    }
    let d = downloads.remove(id).unwrap();
    drop(guard);
    let data: Vec<u8> = d.pieces.into_iter().flatten().flatten().collect();
    if fingerprint(&data) != d.hash {
        log::warn!("jobs: {id} from {}: not what was announced", std::net::Ipv4Addr::from(from));
        return;
    }
    let path = shared_dir().join(format!("{id}.json"));
    // Kept only if it is a job.
    let Some(job) = parse_job(&data, id, Kind::Shared, "", &path) else { return };
    if let Err(e) = crate::write_file(&path, &data) {
        log::warn!("jobs: saving {}: {e}", path.display());
        return;
    }
    add(job);
    make_room();
}

/// The lobby's jobs kept, at most: anyone in a public lobby can announce
/// some, one after the other.
const MAX_SHARED: usize = 256;
const MAX_SHARED_BYTES: u64 = 64 << 20;

/// Drops the first shared jobs (the oldest added) while there are too many.
fn make_room() {
    let mut dropped = Vec::new();
    {
        let mut library = library();
        let jobs = library.as_mut().unwrap();
        loop {
            let shared = jobs.iter().filter(|j| j.kind == Kind::Shared);
            let (count, bytes) = shared.fold((0, 0), |(c, b), j| (c + 1, b + u64::from(j.size)));
            if count <= MAX_SHARED && bytes <= MAX_SHARED_BYTES {
                break;
            }
            let first = jobs.iter().position(|j| j.kind == Kind::Shared).unwrap();
            dropped.push(jobs.remove(first));
        }
    }
    for job in dropped {
        log::info!("jobs: {} \"{}\" dropped (room for newer ones)", job.id, job.name);
        let _ = std::fs::remove_file(&job.path);
    }
}

// An announcement and a piece fit a datagram the Internet carries whole.
const _: () = assert!(1 + PER_ANNOUNCEMENT * 34 < 1200 && 1 + 22 + 8 + 8 + PIECE_SIZE < 1200);
