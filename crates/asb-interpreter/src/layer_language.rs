//! Compatibility for optional layer filenames during early startup.
pub(crate) fn repair_source(path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    if path.replace('\\', "/").to_ascii_lowercase() != "system/adv/grph.lua" { return None; }
    let source=std::str::from_utf8(bytes).ok()?;
    let start=source.find("function lyc2(param)")?;
    let tail=&source[start..];
    let end=tail.find("\nfunction ").unwrap_or(tail.len());
    let body=&tail[..end];
    const CALL: &str = "param.file = param.file:gsub(\"%*%*\", \"_\"..sys.conf.lang)";
    // Only the known unguarded statement in the layer helper is repaired.
    // Pure-color layers have no file; the loading image precedes saved settings.
    let matches: Vec<_>=body.match_indices(CALL).filter(|(at,_)| {
        body[..*at].rsplit('\n').next().unwrap_or("").trim().is_empty()
    }).collect();
    let [(at,_)]=matches.as_slice() else { return None; };
    if !body.contains("tag(param)") || !body.contains("local p = tcopy(param)") { return None; }
    let mut fixed=source.to_owned();
    fixed.replace_range(start+at..start+at+CALL.len(), &format!(
        "if param.file and param.file:find('**', 1, true) and sys and sys.conf and sys.conf.lang then {CALL} end"
    ));
    Some(fixed.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;
    const SOURCE: &str = r#"
function lyc2(param)
    if param.add then param.id = param.id..param.add end
    param[1] = "lyc"
    param.file = param.file:gsub("%*%*", "_"..sys.conf.lang)
    tag(param)
    local p = tcopy(param)
    p[1] = "lyprop"
    p.left = p.left or p.x
    tag(p)
end
"#;
    #[test]
    fn startup_solid_and_plain_image_do_not_require_language() {
        let lua = Lua::new();
        lua.load("calls={}; function tag(p) table.insert(calls,p) end; function tcopy(p) local r={} for k,v in pairs(p) do r[k]=v end return r end").exec().unwrap();
        let fixed=repair_source("system/adv/grph.lua",SOURCE.as_bytes()).unwrap_or_else(||SOURCE.as_bytes().to_vec());
        lua.load(&fixed).exec().unwrap();
        lua.load(r#"
            lyc2{id='solid',width=960,height=544,color='ff000000'}
            lyc2{id='loading',file='ui/960/loading',x=640}
            assert(#calls==4 and calls[1].file==nil and calls[1].color=='ff000000')
            assert(calls[3].file=='ui/960/loading' and calls[4].left==640)
            sys={conf={lang='en'}}
            lyc2{id='localized',file='ui/button**'}
            assert(calls[5].file=='ui/button_en')
        "#).exec().unwrap();
    }
    #[test]
    fn unrelated_and_already_guarded_sources_are_untouched() {
        assert!(repair_source("other.lua",SOURCE.as_bytes()).is_none());
        assert!(repair_source("system/adv/grph.lua",b"function unrelated(param)\nparam.file = param.file:gsub(\"%*%*\", \"_\"..sys.conf.lang)\nend").is_none());
        let fixed=repair_source("system/adv/grph.lua",SOURCE.as_bytes()).unwrap();
        assert!(repair_source("system/adv/grph.lua",&fixed).is_none());
    }
}
