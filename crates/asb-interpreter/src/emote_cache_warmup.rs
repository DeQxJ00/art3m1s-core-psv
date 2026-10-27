//! Replace a recognized, disposable desktop warmup with bounded host prefetch.
//! Do not change zero-canvas layers in general: they can be queried by scripts.
const OLD: &str = r#"function emote.cacheinit()
	if game.os == "windows" then
		local id  = 999
		local ext = game.fgext
		for i, v in ipairs(ex.cache) do
			local px = ":fg/"..v..ext
			if isFile(px) then
				local em = e:createEmoteLayer{ id=(id.."."..i), files={ px }, width=(0), height=(0), progress=(ex.getProgress(true)) }
			end
		end
		lydel2(id)
	end
end"#;
const NEW: &str = r#"function emote.cacheinit()
    if game.os == "windows" then
        local paths = {}
        for i, v in ipairs(ex.cache) do
            if i > 128 then break end
            local px = ":fg/"..v..game.fgext
            if isFile(px) then paths[#paths+1] = px end
        end
        e:preloadEmoteSources(paths)
    end
end"#;

pub(crate) fn repair_source(path:&str,bytes:&[u8])->Option<Vec<u8>> {
    if !path.replace('\\',"/").ends_with("/e-mote.lua"){return None;}
    let source=std::str::from_utf8(bytes).ok()?.replace("\r\n","\n");
    if source.matches(OLD).count()!=1{return None;}
    Some(source.replacen(OLD,NEW,1).into_bytes())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn disposable_warmup_prefetches_without_constructing_players() {
        let lua=mlua::Lua::new();
        lua.load(r#"emote={};game={os='windows',fgext='.psb'};ex={cache={}};e={}
            for i=1,200 do ex.cache[i]='model'..i end
            function isFile(p) return p~=':fg/model2.psb' end
            function e:preloadEmoteSources(paths) saved=paths end
            function e:createEmoteLayer() error('warmup must not retain players') end
            function lydel2() error('no layers were created') end
            function untouched() return 42 end"#).exec().unwrap();
        let source=format!("{OLD}\nfunction after() return 7 end");
        lua.load(repair_source("system/image/e-mote.lua",source.as_bytes()).unwrap()).exec().unwrap();
        lua.load("emote.cacheinit();assert(#saved==127 and saved[1]==':fg/model1.psb');assert(untouched()==42 and after()==7);saved=nil;game.os='other';emote.cacheinit();assert(saved==nil)").exec().unwrap();
    }
    #[test] fn unknown_or_observable_zero_canvas_layers_are_not_changed() {
        assert!(repair_source("other.lua",OLD.as_bytes()).is_none());
        let queried=OLD.replace("\n\t\t\tend", "\n                em:getVariable('face')\n\t\t\tend");
        assert!(repair_source("system/image/e-mote.lua",queried.as_bytes()).is_none());
        assert!(repair_source("system/image/e-mote.lua",b"\x1bLua").is_none());
        assert!(repair_source("system/image/e-mote.lua",OLD.replace("width=(0)","width=(960)").as_bytes()).is_none());
    }
}
