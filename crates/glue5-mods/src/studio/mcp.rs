//! The MCP endpoint (Model Context Protocol, "Streamable HTTP" without
//! streams: each POST is one JSON-RPC message, answered in JSON), so a
//! coding agent can write a mod, check it, run it, read its log and try
//! natives live:
//!
//! `claude mcp add --transport http glue5 http://<address>:47110/mcp --header "Authorization: Bearer <code>"`

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Request, api};
use crate::files;

const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Answers one POST: status, body, extra headers.
pub fn handle(request: &Request) -> (u16, String, Vec<(&'static str, String)>) {
    let Ok(msg) = serde_json::from_slice::<Value>(&request.body) else {
        return (400, error(Value::Null, -32700, "parse error").to_string(), Vec::new());
    };
    // Batches are no longer in the protocol; one message per request.
    let id = msg["id"].clone();
    let method = msg["method"].as_str().unwrap_or("");
    if id.is_null() {
        // A notification (`notifications/initialized`, ...) or a response.
        return (202, String::new(), Vec::new());
    }
    let p = &msg["params"];
    let result = match method {
        "initialize" => {
            let asked = p["protocolVersion"].as_str().unwrap_or("");
            let version = VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {}, "resources": {} },
                "serverInfo": { "name": "glue5", "title": "GLUE 5 mods", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Mods for GTA V running in GLUE 5. Read the resource glue5://guide first. \
                    Write files with write_file (it answers with type errors), run with start_mod or restart_mod \
                    (they answer with the first log lines), try natives live with eval, look them up with \
                    search_natives, and use a mod's menus with menus and menu_input.",
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => Ok(call(p["name"].as_str().unwrap_or(""), &p["arguments"])),
        "resources/list" => Ok(json!({ "resources": [
            { "uri": "glue5://guide", "name": "guide", "title": "How mods work", "mimeType": "text/markdown" },
            { "uri": "glue5://natives.d.luau", "name": "natives", "title": "Every native's type (Luau definitions)", "mimeType": "text/plain" },
            { "uri": "glue5://world.d.luau", "name": "world", "title": "The installed game's vehicle, ped and weapon names (Luau types)", "mimeType": "text/plain" },
        ]})),
        "resources/read" => match p["uri"].as_str() {
            Some(uri @ "glue5://guide") => Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": api::GUIDE }] })),
            Some(uri @ "glue5://natives.d.luau") => {
                Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/plain", "text": crate::natives::DEFINITIONS }] }))
            }
            Some(uri @ "glue5://world.d.luau") => {
                Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/plain", "text": crate::world::definitions() }] }))
            }
            _ => Err((-32002, "resource not found".to_string())),
        },
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    let body = match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => error(id, code, &message),
    };
    (200, body.to_string(), Vec::new())
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tools() -> Value {
    let string = |d: &str| json!({ "type": "string", "description": d });
    let tool = |name: &str, description: &str, props: Value, required: &[&str]| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": { "type": "object", "properties": props, "required": required },
        })
    };
    json!([
        tool("list_mods", "The mods: name, state (running, stopped, error), the error, time per frame, files.", json!({}), &[]),
        tool("create_mod", "Creates a mod folder with a main.luau to start from.", json!({ "name": string("lowercase letters, digits, - and _") }), &["name"]),
        tool("read_file", "Reads a mod's file.", json!({ "path": string("<mod>/<file>, e.g. trainer/main.luau") }), &["path"]),
        tool("write_file", "Writes a mod's file whole, and answers with the type checker's diagnostics for .luau files (none: it checks). A running mod restarts with it when its mod.json says restartOnSave (the default); the answer then has its state and first log lines.", json!({ "path": string("<mod>/<file>"), "content": string("the whole file") }), &["path", "content"]),
        tool("delete_file", "Deletes a mod's file.", json!({ "path": string("<mod>/<file>") }), &["path"]),
        tool("check", "The type checker's diagnostics for a .luau file, as saved.", json!({ "path": string("<mod>/<file>.luau") }), &["path"]),
        tool("start_mod", "Starts a mod, and answers with its state and log after about a second.", json!({ "name": string("the mod") }), &["name"]),
        tool("restart_mod", "Stops a mod if it runs and starts it again (after changing its files), and answers with its state and log after about a second.", json!({ "name": string("the mod") }), &["name"]),
        tool("stop_mod", "Stops a mod; what it created goes with it.", json!({ "name": string("the mod") }), &["name"]),
        tool("logs", "Log lines (prints, starts, errors with file:line) after a cursor; answers with the next cursor.", json!({ "since": { "type": "integer", "description": "cursor from the last call (0: all kept)" }, "mod": string("only this mod's lines") }), &[]),
        tool("eval", "Runs Luau in the game now (in a script thread, natives available) and answers with the values: e.g. entity.getEntityCoords(player.playerPedId(), true).", json!({ "code": string("an expression, or statements") }), &["code"]),
        tool("menus", "The running mods' menus (ui.menu): each item's id, kind and value.", json!({}), &[]),
        tool("menu_input", "Does what a player would to a mod's menu item, and answers with the mod's log after half a second: \
            value true/false sets a toggle (none flips it), a number a slider, 1-based or the option's text a choice; none presses a button.",
            json!({ "mod": string("the mod"), "item": { "type": "integer", "description": "the item's id (from menus)" }, "value": { "description": "see above; leave out for buttons" } }), &["mod", "item"]),
        tool("search_natives", "Finds natives by words of their name or notes; answers with signatures.", json!({ "query": string("e.g. vehicle colour"), "limit": { "type": "integer", "description": "at most this many (20)" } }), &["query"]),
        tool("native_doc", "A native's signature and notes.", json!({ "name": string("namespace.name (vehicle.createVehicle) or the game's name (CREATE_VEHICLE)") }), &["name"]),
    ])
}

/// A tool's result: text content, or an error the agent sees.
fn call(name: &str, a: &Value) -> Value {
    let s = |k: &str| a[k].as_str().ok_or_else(|| format!("missing argument {k}"));
    let result: Result<Value, String> = (|| {
        Ok(match name {
            "list_mods" => api::mods(),
            "create_mod" => {
                files::create_mod(s("name")?)?;
                json!({ "created": files::list(s("name")?) })
            }
            "read_file" => Value::String(files::read(s("path")?)?),
            "write_file" => {
                let path = s("path")?;
                let (_, cursor) = crate::logs(u64::MAX);
                files::write(path, s("content")?)?;
                let mut out = json!({ "written": path });
                if path.ends_with(".luau") || path.ends_with(".json") {
                    out["diagnostics"] = api::diagnostics(path, None);
                    super::checker_used_by_agent();
                }
                // A running mod restarts with it, as from the Studio.
                if crate::saved(path) {
                    out["restarted"] = settle(path.split('/').next().unwrap_or(""), cursor);
                }
                out
            }
            "delete_file" => {
                files::delete(s("path")?)?;
                json!({ "deleted": s("path")? })
            }
            "check" => {
                super::checker_used_by_agent();
                api::diagnostics(s("path")?, None)
            }
            "start_mod" | "restart_mod" => {
                let mod_name = s("name")?;
                if !crate::mods().iter().any(|m| m.name == mod_name) {
                    return Err(format!("no mod {mod_name}"));
                }
                if let Some(why) = crate::cannot_start() {
                    return Err(why.into());
                }
                let (_, cursor) = crate::logs(u64::MAX);
                if name == "start_mod" {
                    crate::start(mod_name);
                } else {
                    crate::restart(mod_name);
                }
                settle(mod_name, cursor)
            }
            "stop_mod" => {
                crate::stop(s("name")?);
                json!({ "stopping": s("name")? })
            }
            "logs" => {
                let (lines, next) = api::logs(a["since"].as_u64().unwrap_or(0), a["mod"].as_str());
                json!({ "lines": lines, "next": next })
            }
            "eval" => match api::eval(s("code")?, Duration::from_secs(10)) {
                Ok(v) => Value::String(v),
                Err(e) => return Err(e),
            },
            "menus" => api::menus(),
            "menu_input" => {
                let module = s("mod")?;
                let item = a["item"].as_u64().ok_or("missing argument item")? as u32;
                let (_, cursor) = crate::logs(u64::MAX);
                api::menu_input(module, item, &a["value"])?;
                std::thread::sleep(Duration::from_millis(500));
                let (lines, next) = api::logs(cursor, Some(module));
                json!({ "log": lines.iter().map(line).collect::<Vec<_>>(), "next": next })
            }
            "search_natives" => Value::Array(api::search(s("query")?, a["limit"].as_u64().unwrap_or(20).min(100) as usize)),
            "native_doc" => api::find(s("name")?).map(|n| api::describe(n, true)).ok_or("no such native")?,
            _ => return Err(format!("unknown tool {name}")),
        })
    })();
    match result {
        Ok(Value::String(text)) => json!({ "content": [{ "type": "text", "text": text }] }),
        Ok(v) => json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap() }] }),
        Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
    }
}

/// A mod's state and log about a second after it was started (sooner if
/// it fails or ends).
fn settle(name: &str, cursor: u64) -> Value {
    let started = Instant::now();
    let mut seen_running = false;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let m = crate::mods().into_iter().find(|m| m.name == name);
        let state = m.as_ref().map_or("gone", |m| m.state.name());
        seen_running |= state == "running";
        let done = (seen_running && state != "running") || state == "error";
        if done || started.elapsed() > Duration::from_millis(1200) {
            let (lines, next) = api::logs(cursor, Some(name));
            return json!({
                "state": state,
                "error": m.and_then(|m| m.error),
                "log": lines.iter().map(line).collect::<Vec<_>>(),
                "next": next,
                "note": if state == "running" && lines.is_empty() { "running; a mod starts once the player is in the game" } else { "" },
            });
        }
    }
}

fn line(l: &Value) -> String {
    format!("[{}] {}", l["level"].as_str().unwrap_or(""), l["text"].as_str().unwrap_or(""))
}
