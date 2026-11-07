//! Guard a known invalid deferred flash target without changing timed flashes.
pub(crate) fn repair_source(path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    if path.replace('\\', "/").to_ascii_lowercase() != "system/adv/grph.lua" { return None; }
    let source = std::str::from_utf8(bytes).ok()?;
    let start = source.find("function SC_FL(e, p)")?;
    let tail = &source[start..];
    let end = tail.find("\nfunction ").unwrap_or(tail.len());
    let body = &tail[..end];
    const CALL: &str = "pretag_store(\"SC\",no,\"SC_FL\",p)";
    if !body.contains("local v = getImageTag(p.no)") ||
       !body.contains("local time = (tonumber(v.time or 0)) / 2") ||
       !body.contains("if time ~= 0 then") || body.matches(CALL).count() != 1 ||
       body.contains("if no ~= nil then") { return None; }
    let offset = start + body.find(CALL)?;
    let mut fixed = source.to_owned();
    fixed.replace_range(offset..offset+CALL.len(), &format!("if no ~= nil then {CALL} end"));
    Some(fixed.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;
    const SOURCE: &str = r#"
function SC_FL(e, p)
 local v = getImageTag(p.no)
 local time = (tonumber(v.time or 0)) / 2
 if time ~= 0 then
  flashes = flashes + 1; duration = time
 else
  pretag_store("SC",no,"SC_FL",p)
 end
end
"#;
    #[test]
    fn absent_target_does_not_block_following_dialogue() {
        let fixed = repair_source("system/adv/grph.lua", SOURCE.as_bytes()).unwrap_or(SOURCE.as_bytes().to_vec());
        for time in ["nil", "0", "140"] {
            let lua = Lua::new();
            lua.load(format!("flashes=0; stored={{}}; function getImageTag(i) return {{time={time}}} end; function pretag_store(kind,key,name,p) stored[key]=p end")).exec().unwrap();
            lua.load(&fixed).exec().unwrap();
            lua.load("SC_FL({}, {no=1}); continued=true").exec().unwrap();
            assert!(lua.globals().get::<bool>("continued").unwrap());
            assert_eq!(lua.globals().get::<i32>("flashes").unwrap(), if time=="140" {1} else {0});
            if time=="140" { assert_eq!(lua.globals().get::<i32>("duration").unwrap(),70); }
            lua.load("no='valid'; SC_FL({}, {no=2})").exec().unwrap();
            if time!="140" { assert!(lua.load("return stored.valid ~= nil").eval::<bool>().unwrap()); }
        }
    }
    #[test]
    fn unrelated_and_already_fixed_sources_stay_unchanged() {
        assert!(repair_source("other.lua",SOURCE.as_bytes()).is_none());
        assert!(repair_source("system/adv/grph.lua",b"pretag_store(\"SC\",no,\"SC_FL\",p)").is_none());
        let fixed=repair_source("system/adv/grph.lua",SOURCE.as_bytes()).unwrap();
        assert!(repair_source("system/adv/grph.lua",&fixed).is_none());
    }
}
