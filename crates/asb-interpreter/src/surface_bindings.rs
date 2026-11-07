//! Resolve missing UI bindings against the script's current localized UI path.
use mlua::{Lua, Table};

pub(crate) fn resolve(lua: &Lua, key: &str, exists: impl Fn(&str)->bool) -> String {
    let found = |p: &str| exists(p) || [".png", ".jpg", ".jpeg", ".webp", ".pvr"]
        .iter().any(|ext| exists(&format!("{p}{ext}")));
    let candidate = (|| {
        let game: Table = lua.globals().raw_get("game").ok()?;
        let paths: Table = game.raw_get("path").ok()?;
        let ui: String = paths.raw_get("ui").ok()?;
        let ui = ui.trim_end_matches('/');
        let (parent, language) = ui.rsplit_once('/')?;
        if language.is_empty() || language.bytes().any(|b| !b.is_ascii_alphabetic() && b != b'-' && b != b'_') { return None; }
        let (directory, name) = key.rsplit_once('/')?;
        if directory != parent || name.is_empty() { return None; }
        Some(format!("{ui}/{name}"))
    })();
    match candidate {
        Some(candidate) if !found(key) && found(&candidate) => candidate,
        _ => key.to_owned(),
    }
}

const REGISTRY: &str = "art3m1s.surface_binding_paths";
fn bindings(lua: &Lua) -> mlua::Result<Table> {
    if let Ok(t) = lua.named_registry_value::<Table>(REGISTRY) { return Ok(t); }
    let t = lua.create_table()?;
    lua.set_named_registry_value(REGISTRY,t.clone())?;
    Ok(t)
}
pub(crate) fn bind(lua: &Lua, key: &str, exists: impl Fn(&str)->bool) -> mlua::Result<String> {
    let resolved=resolve(lua,key,exists);
    let map=bindings(lua)?;
    let stack=map.raw_get::<Table>(key).unwrap_or(lua.create_table()?);
    stack.raw_set(stack.raw_len()+1,resolved.clone())?;
    map.raw_set(key,stack)?;
    Ok(resolved)
}
pub(crate) fn lookup(lua: &Lua, key: &str, pop: bool) -> mlua::Result<String> {
    let map=bindings(lua)?;
    if let Ok(stack)=map.raw_get::<Table>(key) {
        let n=stack.raw_len();
        if n>0 {
            let path=stack.raw_get(n)?;
            if pop { stack.raw_set(n,mlua::Value::Nil)?; if n==1 { map.raw_set(key,mlua::Value::Nil)?; } }
            return Ok(path);
        }
    }
    Ok(key.to_owned())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn unbind_uses_original_binding_after_language_change() {
        let lua=Lua::new();
        lua.load("game={path={ui='ui/1280/cn/'}}").exec().unwrap();
        assert_eq!(bind(&lua,"ui/1280/mw",|p|p=="ui/1280/cn/mw.png").unwrap(),"ui/1280/cn/mw");
        lua.load("game.path.ui='ui/1280/en/'").exec().unwrap();
        assert_eq!(lookup(&lua,"ui/1280/mw",false).unwrap(),"ui/1280/cn/mw");
        assert_eq!(lookup(&lua,"ui/1280/mw",true).unwrap(),"ui/1280/cn/mw");
        assert_eq!(lookup(&lua,"ui/1280/mw",false).unwrap(),"ui/1280/mw");
    }
    #[test] fn localized_binding_preserves_existing_and_rejects_unrelated_paths() {
        let lua=Lua::new();
        lua.load("game={path={ui='ui/1280/cn/'}}").exec().unwrap();
        let exists=|p:&str| matches!(p,"ui/1280/cn/mw.png"|"ui/1280/shared.png");
        assert_eq!(resolve(&lua,"ui/1280/mw",exists),"ui/1280/cn/mw");
        assert_eq!(resolve(&lua,"ui/1280/shared",exists),"ui/1280/shared");
        assert_eq!(resolve(&lua,"image/mw",exists),"image/mw");
        assert_eq!(resolve(&lua,"ui/1280/cn/mw",exists),"ui/1280/cn/mw");
        assert_eq!(resolve(&lua,"ui/1280/absent",exists),"ui/1280/absent");
    }
}
