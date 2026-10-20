//! What the buttons and keys do: open, close, save, evaluate.

use leptos::prelude::*;
use serde_json::json;

use crate::net::{fetch_text, request};
use crate::state::*;

pub(crate) fn open_file(s: S, path: String) {
    s.screen.set(Screen::Editor);
    s.drawer.set(false);
    if s.tabs.with_untracked(|t| t.iter().any(|t| t.path == path)) {
        s.active.set(Some(path.clone()));
        with_editor(|e| e.open(&path, "", false));
        return;
    }
    let read_only = path == "natives.d.luau";
    let p = path.clone();
    let shown = move |text: String| {
        with_editor(|e| e.open(&p, &text, read_only));
        s.tabs.update(|t| t.push(Tab { path: p.clone(), dirty: false, read_only }));
        s.active.set(Some(p.clone()));
    };
    if read_only {
        // The definitions, as served (not a mod's file).
        let token = TOKEN.with(|t| t.borrow().clone());
        let url = format!("/natives.d.luau?token={token}");
        fetch_text(url, shown);
    } else {
        request("files.read", json!({ "path": path }), move |r| match r {
            Ok(v) => shown(v["text"].as_str().unwrap_or("").to_string()),
            Err(e) => s.notice.set(Some(e)),
        });
    }
}

pub(crate) fn close_tab(s: S, path: &str) {
    let dirty = s.tabs.with_untracked(|t| t.iter().any(|t| t.path == path && t.dirty));
    if dirty
        && !web_sys::window()
            .and_then(|w| w.confirm_with_message(&format!("{path} has unsaved changes. Close it?")).ok())
            .unwrap_or(false)
    {
        return;
    }
    with_editor(|e| e.close(path));
    s.tabs.update(|t| t.retain(|t| t.path != path));
    if s.active.get_untracked().as_deref() == Some(path) {
        let next = s.tabs.with_untracked(|t| t.last().map(|t| t.path.clone()));
        s.active.set(next.clone());
        if let Some(n) = next {
            with_editor(|e| e.open(&n, "", false));
        }
    }
}

pub(crate) fn save(s: S, path: String) {
    let mut text = String::new();
    with_editor(|e| text = e.text(&path));
    let restart = s.restart_on_save.get_untracked();
    let t = text.clone();
    request("files.write", json!({ "path": path, "text": text, "restart": restart }), move |r| match r {
        Ok(_) => with_editor(|e| e.saved(&path, &t)),
        Err(e) => s.notice.set(Some(e)),
    });
}

pub(crate) fn eval(s: S, code: String) {
    if code.trim().is_empty() {
        return;
    }
    push_row(s, Row::Input(code.clone()));
    request("eval", json!({ "code": code }), move |r| match r {
        Ok(v) => push_row(s, Row::Value(v.as_str().unwrap_or("").to_string())),
        Err(e) => push_row(s, Row::Failure(e)),
    });
}
