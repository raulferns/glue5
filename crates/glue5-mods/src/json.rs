//! `json.encode` and `json.decode`: Luau values as JSON text and back, for
//! mods (settings, the storage, later the network). A table whose keys are
//! 1..n is an array, an empty one too; other tables are objects, with
//! string keys (numbers are written as strings). `null` reads as `nil`.

use mlua::{Lua, Table, Value};
use serde_json::{Map, Number, Value as Json};

/// Tables nested deeper are refused (a table that holds itself).
const DEPTH: usize = 64;

/// The `json` table.
pub fn install(lua: &Lua) -> mlua::Result<()> {
    let json = lua.create_table()?;
    json.set(
        "encode",
        lua.create_function(|_, (value, pretty): (Value, Option<bool>)| {
            let v = to_json(&value, 0).map_err(mlua::Error::runtime)?;
            let text = if pretty == Some(true) { serde_json::to_string_pretty(&v) } else { serde_json::to_string(&v) };
            text.map_err(|e| mlua::Error::runtime(format!("json.encode: {e}")))
        })?,
    )?;
    json.set(
        "decode",
        lua.create_function(|lua, text: mlua::LuaString| {
            let v: Json = serde_json::from_slice(&text.as_bytes())
                .map_err(|e| mlua::Error::runtime(format!("json.decode: {e}")))?;
            from_json(lua, &v)
        })?,
    )?;
    lua.globals().set("json", json)
}

/// A Luau value as JSON.
pub fn to_json(value: &Value, depth: usize) -> Result<Json, String> {
    Ok(match value {
        Value::Nil => Json::Null,
        Value::Boolean(b) => Json::Bool(*b),
        Value::Integer(i) => Json::from(*i),
        Value::Number(n) => number(*n)?,
        Value::String(s) => Json::String(s.to_str().map_err(|_| "json: a string that is not UTF-8")?.to_string()),
        // Single precision: written with as many digits as it holds.
        Value::Vector(v) => Json::Array(
            [v.x(), v.y(), v.z()].iter().map(|c| number(c.to_string().parse().unwrap_or(0.0))).collect::<Result<_, _>>()?,
        ),
        Value::Table(t) => table(t, depth)?,
        other => return Err(format!("json: a {} is not JSON", other.type_name())),
    })
}

/// Whole numbers without a fraction (`3`, not `3.0`).
fn number(n: f64) -> Result<Json, String> {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        Ok(Json::from(n as i64))
    } else {
        Ok(Json::Number(Number::from_f64(n).ok_or("json: NaN and infinity are not JSON")?))
    }
}

fn table(t: &Table, depth: usize) -> Result<Json, String> {
    if depth >= DEPTH {
        return Err("json: tables nested too deep (does a table hold itself?)".into());
    }
    let len = t.raw_len();
    let mut count = 0;
    let mut pairs = Vec::new();
    for pair in t.pairs::<Value, Value>() {
        let (k, v) = pair.map_err(|e| e.to_string())?;
        count += 1;
        pairs.push((k, v));
    }
    // 1..n and nothing else: an array.
    if count == len {
        let mut out = Vec::with_capacity(len);
        for i in 1..=len {
            out.push(to_json(&t.raw_get::<Value>(i).map_err(|e| e.to_string())?, depth + 1)?);
        }
        return Ok(Json::Array(out));
    }
    let mut out = Map::new();
    for (k, v) in pairs {
        let key = match &k {
            Value::String(s) => s.to_str().map_err(|_| "json: a key that is not UTF-8")?.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            other => return Err(format!("json: a {} key (keys are strings or numbers)", other.type_name())),
        };
        out.insert(key, to_json(&v, depth + 1)?);
    }
    Ok(Json::Object(out))
}

/// JSON as a Luau value (`null` is `nil`).
pub fn from_json(lua: &Lua, v: &Json) -> mlua::Result<Value> {
    Ok(match v {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => Value::String(lua.create_string(s)?),
        Json::Array(items) => {
            let t = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, from_json(lua, item)?)?;
            }
            Value::Table(t)
        }
        Json::Object(map) => {
            let t = lua.create_table_with_capacity(0, map.len())?;
            for (k, item) in map {
                t.raw_set(k.as_str(), from_json(lua, item)?)?;
            }
            Value::Table(t)
        }
    })
}

#[cfg(test)]
mod tests {
    use mlua::Lua;

    #[test]
    fn round_trip() {
        let lua = Lua::new();
        super::install(&lua).unwrap();
        let out: String = lua
            .load(
                r#"
                local t = json.decode('{"a": [1, 2.5, null, "x"], "b": {"c": true}, "d": null, "e": []}')
                assert(t.a[1] == 1 and t.a[2] == 2.5 and t.a[3] == nil and t.a[4] == "x")
                assert(t.b.c == true and t.d == nil and #t.e == 0)
                assert(json.encode({1, 2, 3}) == "[1,2,3]")
                assert(json.encode({}) == "[]")
                assert(json.encode({x = 1}) == '{"x":1}')
                assert(json.encode({[10] = "a"}) == '{"10":"a"}')
                assert(json.encode(vector.create(1.1, -2, 0.5)) == "[1.1,-2,0.5]")
                local ok, err = pcall(json.encode, {f = print})
                assert(not ok and string.find(tostring(err), "function"))
                local loop = {}
                loop.self = loop
                ok, err = pcall(json.encode, loop)
                assert(not ok and string.find(tostring(err), "too deep"))
                ok, err = pcall(json.decode, "{oops")
                assert(not ok and string.find(tostring(err), "json.decode"))
                return json.encode(t.b)
            "#,
            )
            .eval()
            .unwrap();
        assert_eq!(out, r#"{"c":true}"#);
    }
}
