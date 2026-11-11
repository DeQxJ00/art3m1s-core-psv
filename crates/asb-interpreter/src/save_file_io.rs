use crate::lua_engine::EngineContext;
use mlua::Lua;
use std::sync::{Arc, Mutex};

// Route only the game's logical save directory through host storage. Native Lua
// otherwise resolves these relative paths against the application's working directory.
pub fn install(lua: &Lua, ctx: Arc<Mutex<EngineContext>>) -> mlua::Result<()> {
    let scope = ctx.clone();
    let is_save = lua.create_function(move |_, path: String| {
        let ctx = scope.lock().unwrap();
        let prefix = ctx.variables.as_ref().and_then(|v| v.lock().unwrap()
            .get("s.savepath").map(|v| v.as_string())).unwrap_or_else(|| "save".into());
        let path = path.replace('\\', "/");
        let prefix = prefix.replace('\\', "/");
        Ok(path.starts_with(&format!("{}/", prefix.trim_end_matches('/'))))
    })?;
    let reader = ctx.clone();
    let read = lua.create_function(move |lua, path: String| {
        let loader = reader.lock().unwrap().file_reader.clone();
        match loader.ok_or_else(|| "host reader unavailable".to_string())
            .and_then(|load| load(&path).map_err(|e| e.to_string())) {
            Ok(bytes) => Ok((mlua::Value::String(lua.create_string(bytes)?), None)),
            Err(e) => Ok((mlua::Nil, Some(e))),
        }
    })?;
    let write = lua.create_function(move |_, (path, bytes): (String, mlua::String)| {
        match ctx.lock().unwrap().callbacks.file_write(&path, &bytes.as_bytes()) {
            Ok(()) => Ok((Some(true), None)),
            Err(e) => Ok((None, Some(e.to_string()))),
        }
    })?;
    lua.load(include_str!("save_file_io.lua")).call::<()>((is_save, read, write))
}
