//! Compatibility for a queued shake whose image was deleted earlier in the batch.
pub(crate) fn repair_source(path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    if path.replace('\\', "/").to_ascii_lowercase() != "system/adv/grph.lua" { return None; }
    let source = std::str::from_utf8(bytes).ok()?;
    let start = source.find("function CG_SK(e, p)")?;
    let tail = &source[start..];
    let end = tail.find("\nfunction ").unwrap_or(tail.len());
    let body = &tail[..end];
    const TARGET: &str = "local id = getCGID(v.id)..\".0\"";
    if !body.contains("getImageTag(p.no)") || !body.contains("quake(id, v)") || body.matches(TARGET).count()!=1 { return None; }
    let offset = start + body.find(TARGET)?;
    let mut fixed = source.to_owned();
    fixed.replace_range(offset..offset+TARGET.len(), "local id = getCGID(v.id)\n\tif id == nil then return end\n\tid = id..\".0\"");
    Some(fixed.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;
    const SOURCE: &str = r#"
function CG_SK(e, p)
 local v  = getImageTag(p.no)
 local id = getCGID(v.id)..".0"
 quake(id, v)
end
"#;
    #[test]
    fn deleted_target_is_noop_but_existing_target_still_shakes() {
        let fixed = repair_source("system/adv/grph.lua", SOURCE.as_bytes()).unwrap_or(SOURCE.as_bytes().to_vec());
        let lua = Lua::new();
        lua.load(r#"
            ids = {[6]='scene.5'}; shakes = 0
            function getImageTag(no) return {id=5, time=3500, h=35} end
            function getCGID(id) return ids[1+id] end
            function quake(id,v) assert(id=='scene.5.0'); assert(v.h==35); shakes=shakes+1 end
        "#).exec().unwrap();
        lua.load(&fixed).exec().unwrap();
        lua.load("CG_SK({}, {no=1}); ids[6]=nil; CG_SK({}, {no=2}); continued=true").exec().unwrap();
        assert_eq!(lua.globals().get::<i32>("shakes").unwrap(),1);
        assert!(lua.globals().get::<bool>("continued").unwrap());
    }
    #[test]
    fn repair_is_scoped_and_idempotent() {
        assert!(repair_source("other.lua", SOURCE.as_bytes()).is_none());
        let fixed=repair_source("system/adv/grph.lua",SOURCE.as_bytes()).unwrap();
        assert!(repair_source("system/adv/grph.lua",&fixed).is_none());
    }
}
