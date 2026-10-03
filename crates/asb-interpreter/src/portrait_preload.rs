//! Recognized message-portrait cache compatibility. Resolve filenames during
//! the game's cache pass; rendering/timeline queries never execute the helper.
use mlua::{Function, Lua, Table, Value};

const MAP: &str = "art3m1s.portrait-preloads";
const EPOCH: &str = "art3m1s.portrait-preload-epoch";
const OLD: &str = r#"local px = path:gsub(":fg/", ":fa/"):gsub("/[lsm]/", "/")
setImageStack(px..file..ext)

for i, v in ipairs(tbl) do
if p[v] then
if v == "face" then setImageStack(path..p[v]..ext)
else setImageStack(px ..p[v]..ext) end
end
end"#;
const NEW: &str = r#"if not e:preloadPortraitLayers(p) then
    local px = path:gsub(":fg/", ":fa/"):gsub("/[lsm]/", "/")
    setImageStack(px..file..ext)
    for v in pairs(tbl) do
        if v ~= "file" and p[v] then
            if v == "face" then setImageStack(path..p[v]..ext)
            else setImageStack(px..p[v]..ext) end
        end
    end
end
"#;
fn compact(s: &str) -> String { s.chars().filter(|c| !c.is_whitespace()).collect() }

pub(crate) fn repair_source(path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    if !path.replace('\\', "/").ends_with("/image/cache.lua") { return None; }
    let source = std::str::from_utf8(bytes).ok()?;
    if !source.lines().any(|l| compact(l) == "functionstackImageCache(p)") { return None; }
    let lines: Vec<_> = source.split_inclusive('\n').collect();
    let expected: Vec<_> = OLD.lines().map(compact).collect();
    let matches: Vec<_> = lines.windows(expected.len()).enumerate()
        .filter(|(_, lines)| lines.iter().map(|l| compact(l)).eq(expected.iter().cloned()))
        .map(|(i, _)| i).collect();
    let [at] = matches.as_slice() else { return None; };
    // Preserve the original mode/cache-size guards. Unknown implementations
    // and bytecode keep the normal include path.
    if *at == 0 || compact(lines[at - 1]) != "ifsz~=\"small\"and(m==1orm==3)then" { return None; }
    let mut out = lines[..*at].concat();
    out.push_str(NEW);
    out.push_str(&lines[at + expected.len()..].concat());
    Some(out.into_bytes())
}
fn text(t: &Table, key: &str) -> Option<String> {
    let Value::String(s) = t.raw_get::<Value>(key).ok()? else { return None; };
    let s = s.to_str().ok()?.to_string();
    (!s.is_empty() && s.len() <= 1024 && !s.contains('\0')).then_some(s)
}
pub(crate) fn epoch(lua: &Lua) -> u64 { lua.named_registry_value(EPOCH).unwrap_or(0) }
pub(crate) fn paths(lua: &Lua, tag: &Table) -> Vec<String> {
    lua.named_registry_value::<Table>(MAP).ok()
        .and_then(|m| m.raw_get::<Table>(tag.clone()).ok())
        .map(|t| t.sequence_values::<String>().take(32).filter_map(Result::ok).collect())
        .unwrap_or_default()
}
fn record(lua: &Lua, tag: &Table, paths: &[String]) -> mlua::Result<()> {
    if self::paths(lua, tag) == paths { return Ok(()); }
    let map = match lua.named_registry_value::<Table>(MAP) {
        Ok(t) => t,
        Err(_) => {
            let t = lua.create_table()?;
            let mt = lua.create_table()?;
            mt.raw_set("__mode", "k")?;
            t.set_metatable(Some(mt)); // Do not retain completed chapter tags.
            lua.set_named_registry_value(MAP, t.clone())?;
            t
        }
    };
    map.raw_set(tag.clone(), lua.create_sequence_from(paths.iter().cloned())?)?;
    lua.set_named_registry_value(EPOCH, epoch(lua).wrapping_add(1))
}
pub(crate) fn preload(lua: &Lua, tag: &Table) -> mlua::Result<bool> {
    let g = lua.globals();
    let Ok(resolve) = g.raw_get::<Function>("getMWFaceFile") else { return Ok(false); };
    let Ok(bind) = g.raw_get::<Function>("setImageStack") else { return Ok(false); };
    let Some(ext) = g.raw_get::<Table>("game").ok().and_then(|t| text(&t, "fgext")) else { return Ok(false); };
    let copy = lua.create_table()?;
    for (n, entry) in tag.clone().pairs::<Value, Value>().enumerate() {
        if n >= 128 { return Ok(false); }
        let (key, value) = entry?;
        copy.raw_set(key, value)?;
    }
    let Ok(parts) = resolve.call::<Table>((copy, true)) else { return Ok(false); };
    let Some(prefix) = text(&parts, "path") else { return Ok(false); };
    let mut fields = Vec::new();
    for (n, entry) in parts.clone().pairs::<Value, Value>().enumerate() {
        if n >= 40 { return Ok(false); }
        if let (Value::String(k), Value::Table(v)) = entry? {
            let Some(file) = text(&v, "file") else { continue; };
            fields.push((k.to_str()?.to_string(), format!("{prefix}{file}{ext}")));
        }
    }
    if fields.is_empty() || fields.len() > 32 || !fields.iter().any(|(k, _)| k == "file") { return Ok(false); }
    fields.sort_by(|a, b| (a.0 != "file", &a.0).cmp(&(b.0 != "file", &b.0)));
    let mut accepted = Vec::new();
    for (_, path) in fields {
        if accepted.contains(&path) { continue; }
        bind.call::<()>(path.clone())?;
        // setImageStack owns cache limits, reference counts and first-use order.
        let bound = g.raw_get::<Table>("cachebuff").ok().and_then(|t| t.raw_get::<Table>("img").ok());
        if bound.is_some_and(|t| t.raw_get::<bool>(path.as_str()).unwrap_or(false)) { accepted.push(path); }
    }
    record(lua, tag, &accepted)?;
    Ok(true)
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn repairs_only_the_recognized_portrait_branch() {
        let source = format!("function stackImageCache(p)\nif sz ~= \"small\" and (m == 1 or m == 3) then\n{OLD}\nend\nend\n");
        let fixed = repair_source("system/image/cache.lua", source.as_bytes()).unwrap();
        assert!(String::from_utf8(fixed.clone()).unwrap().contains("e:preloadPortraitLayers(p)"));
        assert!(repair_source("system/image/cache.lua", &fixed).is_none());
        assert!(repair_source("system/other.lua", source.as_bytes()).is_none());
        assert!(repair_source("system/image/cache.lua", source.replace("m == 1", "m == 2").as_bytes()).is_none());
    }
    #[test] fn resolves_actual_names_preserves_ast_and_honors_binding_limit() {
        let lua = Lua::new();
        lua.load(r#"
            game={fgext='.png'}; cachebuff={img={}}; calls=0
            tag={'fg',file='body_m',file1='expression_m',path=':fg/medium/'}
            function getMWFaceFile(p,flag)
                assert(flag); p.file='changed'; calls=calls+1
                return {path=':fa/portrait/',file={file='body_f'},file1={file='expression_f'},file2={file='refused'}}
            end
            function setImageStack(p) if not p:find('refused') then cachebuff.img[p]=true end end
        "#).exec().unwrap();
        let tag = lua.globals().raw_get::<Table>("tag").unwrap();
        assert!(preload(&lua, &tag).unwrap());
        assert_eq!(paths(&lua, &tag), [":fa/portrait/body_f.png", ":fa/portrait/expression_f.png"]);
        assert_eq!(tag.raw_get::<String>("file").unwrap(), "body_m");
        let generation = epoch(&lua);
        preload(&lua, &tag).unwrap();
        assert_eq!(epoch(&lua), generation);
        lua.load("cachebuff.img={}; function setImageStack(p) end").exec().unwrap();
        preload(&lua, &tag).unwrap();
        assert!(paths(&lua, &tag).is_empty());
        assert!(epoch(&lua) > generation);
    }
    #[test] fn missing_or_failing_resolver_falls_back_without_binding() {
        let lua = Lua::new();
        let tag = lua.create_table().unwrap();
        assert!(!preload(&lua, &tag).unwrap());
        lua.load("game={fgext='.png'};function setImageStack(p) error('must not bind') end;function getMWFaceFile() error('unsupported') end").exec().unwrap();
        assert!(!preload(&lua, &tag).unwrap());
    }
    #[test] fn recorded_paths_do_not_keep_a_completed_chapter_alive() {
        let lua = Lua::new();
        {
            let tag = lua.create_table().unwrap();
            record(&lua, &tag, &[":fa/layer.png".into()]).unwrap();
            assert_eq!(paths(&lua, &tag).len(), 1);
        }
        lua.gc_collect().unwrap();
        let map = lua.named_registry_value::<Table>(MAP).unwrap();
        assert_eq!(map.pairs::<Value, Value>().count(), 0);
    }
}
