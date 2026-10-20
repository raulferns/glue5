//! Mods' menus: `ui.menu(...)` in Luau makes one; the application draws it
//! with its own widgets (touch and controllers included) and sends back
//! what the player does (`menu_input`), which the mod hears at its next
//! turn: the item's `value` changes and its callback runs as a task.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::{Menu, MenuInput, MenuItem, MenuKind, SHARED};

/// A mod's items, by id: the Lua object (its `value`) and its callback.
pub type Handlers = Rc<RefCell<HashMap<u32, (Table, Option<Function>)>>>;

fn next_id() -> u32 {
    let mut s = SHARED.state.lock().unwrap();
    let id = s.next_menu_id;
    s.next_menu_id += 1;
    id
}

fn changed() {
    SHARED.generation.fetch_add(1, Ordering::Relaxed);
}

fn with_item(module: &str, item: u32, f: impl FnOnce(&mut MenuItem)) {
    let mut s = SHARED.state.lock().unwrap();
    if let Some(i) = s.menus.iter_mut().filter(|m| m.module == module).flat_map(|m| m.items.iter_mut()).find(|i| i.id == item) {
        f(i);
    }
    drop(s);
    changed();
}

/// Mod `module`'s menus and what was done with them go (it stopped).
pub fn remove_mod(module: &str) {
    let mut s = SHARED.state.lock().unwrap();
    s.menus.retain(|m| m.module != module);
    s.inputs.retain(|(m, ..)| m != module);
    drop(s);
    changed();
}

/// Whether mod `module` has a menu (then it stays, waiting for the player).
pub fn has_menus(module: &str) -> bool {
    SHARED.state.lock().unwrap().menus.iter().any(|m| m.module == module)
}

/// What was done with mod `module`'s menus since the last call.
pub fn take_inputs(module: &str) -> Vec<(u32, MenuInput)> {
    let mut s = SHARED.state.lock().unwrap();
    let mut out = Vec::new();
    s.inputs.retain(|(m, item, input)| {
        if m == module {
            out.push((*item, *input));
            false
        } else {
            true
        }
    });
    out
}

/// The `ui` table: `ui.menu(title)`, `ui.notify(text)`.
pub fn install(lua: &Lua, module: &str, handlers: Handlers) -> mlua::Result<()> {
    let ui = lua.create_table()?;
    let m = module.to_string();
    ui.set(
        "notify",
        lua.create_function(move |_, text: String| {
            let text: String = text.chars().take(200).collect();
            let mut s = SHARED.state.lock().unwrap();
            if s.notices.len() < 16 {
                s.notices.push_back((m.clone(), text));
            }
            Ok(())
        })?,
    )?;
    let m = module.to_string();
    ui.set(
        "menu",
        lua.create_function(move |lua, title: String| {
            let id = next_id();
            SHARED.state.lock().unwrap().menus.push(Menu {
                module: m.clone(),
                id,
                title: title.chars().take(40).collect(),
                items: Vec::new(),
            });
            changed();
            menu_table(lua, &m, id, handlers.clone())
        })?,
    )?;
    lua.globals().set("ui", ui)
}

/// A menu's Lua object: `:toggle`, `:slider`, `:choice`, `:button`,
/// `:label`, `:remove`.
fn menu_table(lua: &Lua, module: &str, menu: u32, handlers: Handlers) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("id", menu)?;
    // Adds an item to the menu; its object, with `value` and `:set`.
    let add = {
        let module = module.to_string();
        let handlers = handlers.clone();
        Rc::new(
            move |lua: &Lua, label: String, note: Option<String>, kind: MenuKind, callback: Option<Function>| -> mlua::Result<Table> {
                let id = next_id();
                let item = MenuItem { id, label: label.chars().take(60).collect(), note, kind: kind.clone() };
                {
                    let mut s = SHARED.state.lock().unwrap();
                    match s.menus.iter_mut().find(|x| x.id == menu) {
                        Some(m) if m.items.len() < 64 => m.items.push(item),
                        Some(_) => return Err(mlua::Error::runtime("a menu holds 64 items at most")),
                        None => return Err(mlua::Error::runtime("the menu was removed")),
                    }
                }
                changed();
                let obj = lua.create_table()?;
                obj.set("id", id)?;
                obj.set("value", value_of(lua, &kind)?)?;
                let module = module.clone();
                obj.set(
                    "set",
                    lua.create_function(move |lua, (this, v): (Table, Value)| {
                        let mut new = Value::Nil;
                        with_item(&module, id, |i| match (&mut i.kind, &v) {
                            (MenuKind::Toggle(on), Value::Boolean(b)) => {
                                *on = *b;
                                new = Value::Boolean(*b);
                            }
                            (MenuKind::Slider { value, min, max, .. }, Value::Integer(_) | Value::Number(_)) => {
                                *value = number(&v).unwrap_or(*value).clamp(*min, *max);
                                new = Value::Number(*value);
                            }
                            (MenuKind::Choice { options, index }, Value::Integer(n)) if (1..=options.len() as i64).contains(n) => {
                                *index = (*n - 1) as usize;
                                new = Value::Integer(*n);
                            }
                            (_, Value::String(s)) => {
                                i.label = s.to_string_lossy().chars().take(60).collect();
                            }
                            _ => {}
                        });
                        if !new.is_nil() {
                            this.set("value", new)?;
                        }
                        let _ = lua;
                        Ok(())
                    })?,
                )?;
                handlers.borrow_mut().insert(id, (obj.clone(), callback));
                Ok(obj)
            },
        )
    };
    let a = add.clone();
    t.set(
        "toggle",
        lua.create_function(move |lua, (_, label, on, a3, a4): (Table, String, Option<bool>, Value, Value)| {
            let (note, cb) = note_and_callback(a3, a4);
            a(lua, label, note, MenuKind::Toggle(on.unwrap_or(false)), cb)
        })?,
    )?;
    let a = add.clone();
    t.set(
        "slider",
        lua.create_function(
            move |lua, (_, label, value, min, max, step, cb): (Table, String, f64, f64, f64, Option<f64>, Option<Function>)| {
                // NaN fails too.
                if min.partial_cmp(&max) != Some(std::cmp::Ordering::Less) {
                    return Err(mlua::Error::runtime("slider: min must be below max"));
                }
                let step = step.filter(|s| *s > 0.0).unwrap_or((max - min) / 100.0);
                a(lua, label, None, MenuKind::Slider { value: value.clamp(min, max), min, max, step }, cb)
            },
        )?,
    )?;
    let a = add.clone();
    t.set(
        "choice",
        lua.create_function(move |lua, (_, label, options, index, cb): (Table, String, Vec<String>, Option<usize>, Option<Function>)| {
            if options.is_empty() {
                return Err(mlua::Error::runtime("choice: give it options"));
            }
            let index = index.unwrap_or(1).clamp(1, options.len()) - 1;
            let options = options.into_iter().take(32).map(|o| o.chars().take(30).collect()).collect();
            a(lua, label, None, MenuKind::Choice { options, index }, cb)
        })?,
    )?;
    let a = add.clone();
    t.set(
        "button",
        lua.create_function(move |lua, (_, label, a3, a4): (Table, String, Value, Value)| {
            let (note, cb) = note_and_callback(a3, a4);
            a(lua, label, note, MenuKind::Button, cb)
        })?,
    )?;
    let a = add.clone();
    t.set(
        "label",
        lua.create_function(move |lua, (_, text): (Table, String)| a(lua, text, None, MenuKind::Label, None))?,
    )?;
    t.set(
        "remove",
        lua.create_function(move |_, _: Table| {
            SHARED.state.lock().unwrap().menus.retain(|m| m.id != menu);
            changed();
            Ok(())
        })?,
    )?;
    Ok(t)
}

/// `(label, [note], [callback])`, either may be left out.
fn note_and_callback(a: Value, b: Value) -> (Option<String>, Option<Function>) {
    match (a, b) {
        (Value::String(s), Value::Function(f)) => (Some(s.to_string_lossy()), Some(f)),
        (Value::String(s), _) => (Some(s.to_string_lossy()), None),
        (Value::Function(f), _) => (None, Some(f)),
        (_, Value::Function(f)) => (None, Some(f)),
        _ => (None, None),
    }
}

fn number(v: &Value) -> Option<f64> {
    match *v {
        Value::Integer(i) => Some(i as f64),
        Value::Number(n) => Some(n),
        _ => None,
    }
}

fn value_of(lua: &Lua, kind: &MenuKind) -> mlua::Result<Value> {
    Ok(match kind {
        MenuKind::Toggle(on) => Value::Boolean(*on),
        MenuKind::Slider { value, .. } => Value::Number(*value),
        MenuKind::Choice { index, .. } => Value::Integer(*index as i64 + 1),
        MenuKind::Button | MenuKind::Label => {
            let _ = lua;
            Value::Nil
        }
    })
}

/// Applies an input to the item's object; what its callback gets.
pub fn apply(lua: &Lua, handlers: &Handlers, item: u32, input: MenuInput) -> mlua::Result<Option<(Function, MultiValue)>> {
    let Some((obj, cb)) = handlers.borrow().get(&item).cloned() else { return Ok(None) };
    let args = match input {
        MenuInput::Toggle(on) => {
            obj.set("value", on)?;
            MultiValue::from_vec(vec![Value::Boolean(on)])
        }
        MenuInput::Value(v) => {
            obj.set("value", v)?;
            MultiValue::from_vec(vec![Value::Number(v)])
        }
        MenuInput::Choose(i) => {
            obj.set("value", i as i64 + 1)?;
            let label = SHARED
                .state
                .lock()
                .unwrap()
                .menus
                .iter()
                .flat_map(|m| m.items.iter())
                .find(|x| x.id == item)
                .and_then(|x| match &x.kind {
                    MenuKind::Choice { options, .. } => options.get(i).cloned(),
                    _ => None,
                })
                .unwrap_or_default();
            MultiValue::from_vec(vec![Value::Integer(i as i64 + 1), Value::String(lua.create_string(label)?)])
        }
        MenuInput::Press => MultiValue::new(),
    };
    Ok(cb.map(|f| (f, args)))
}
