//! Compatibility for the recognized framework's last-fg-only save records.
//! Reconstruct preceding face commands from the saved block, never future tags.
const OLD: &str = "a.disp=nil a.mx=nil a.my=nil a.x2=nil a.y2=nil a.z2=nil\n\t\t\t\t\t\tfg(a)";
const NEW: &str = r#"a.disp=nil a.mx=nil a.my=nil a.x2=nil a.y2=nil a.z2=nil
                        local history = {}
                        local block = ast and t and ast[t[2]]
                        if block and t[4] then block = block.delay and block.delay[t[4]] end
                        local stop = t and tonumber(t[3])
                        if game and game.fgext == ".psb" and type(block) == "table" and
                           type(a.face) == "string" and stop and stop >= 1 and
                           stop <= 2048 and stop == math.floor(stop) then
                            local last = block[stop]
                            if type(last) == "table" and last[1] == "fg" and last.ch == a.ch and
                               last.file == a.file and last.face == a.face and last.path == a.path then
                                for n = 1, stop - 1 do
                                    local tag = block[n]
                                    if type(tag) == "table" and tag[1] == "fg" then
                                        if tag.mode == -2 then history = {}
                                        elseif tag.ch == a.ch then
                                            if tag.file ~= a.file or tag.path ~= a.path or tag.id ~= a.id then history = {}
                                            elseif type(tag.face) == "string" then history[#history + 1] = tag.face end
                                        end
                                    end
                                end
                            end
                        end
                        for _, face in ipairs(history) do
                            local base = {}
                            for k, value in pairs(a) do base[k] = value end
                            base.face = face
                            base.resize = nil
                            base.emreset = nil
                            fg(base)
                        end
                        if e and #history > 0 then e:debug{level=1,raw=true,data="[emote-restore] preceding_faces="..#history} end
                        fg(a)"#;
pub(crate) fn repair_source(path:&str,bytes:&[u8])->Option<Vec<u8>>{
    if !path.replace('\\',"/").ends_with("/quickjump.lua"){return None;}
    let source=std::str::from_utf8(bytes).ok()?.replace("\r\n","\n");
    let at=source.find(OLD)?;
    if source.matches(OLD).count()!=1{return None;}
    let mut out=source[..at].to_owned();out.push_str(NEW);out.push_str(&source[at+OLD.len()..]);
    Some(out.into_bytes())
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn ordered_faces_restore_without_replaying_unrelated_or_future_tags(){
  let lua=mlua::Lua::new();
  lua.load(r#"game={fgext='.psb'}; calls={};
  function fg(p) calls[#calls+1]=p.face; assert(p.x==42) end
  a={ch='hero',id=16,file='model',path=':fg/',face='gesture',x=42};t={'chapter','b',3}
  ast={b={{'fg',ch='hero',id=16,file='model',path=':fg/',face='closed',resize=1},
          {'sound',file='never'},
          {'fg',ch='hero',id=16,file='model',path=':fg/',face='gesture'},
          {'fg',ch='hero',id=16,file='model',path=':fg/',face='future'}}}"#).exec().unwrap();
  let source=format!("function restore(a,t)\n{OLD}\nend");
  let fixed=repair_source("system/quickjump.lua",source.as_bytes()).unwrap();
  lua.load(&fixed).exec().unwrap();
  lua.load("restore(a,t);assert(#calls==2 and calls[1]=='closed' and calls[2]=='gesture');assert(a.face=='gesture')").exec().unwrap();
  for change in ["game.fgext='.png'","t[2]='other'","t[3]=4","ast.b[1].file='other'", "ast.b[1].mode=-2"] {
   lua.load("game.fgext='.psb';t={'chapter','b',3};ast.b[1].file='model';ast.b[1].mode=nil;calls={}").exec().unwrap();
   lua.load(change).exec().unwrap();lua.load("restore(a,t);assert(#calls==1 and calls[1]=='gesture')").exec().unwrap();
  }
  lua.load("ast.b[1].mode=nil;ast.c={delay={d=ast.b}};t={'chapter','c',3,'d'};calls={};restore(a,t);assert(#calls==2)").exec().unwrap();
  assert!(repair_source("other.lua",source.as_bytes()).is_none());
  assert!(repair_source("system/quickjump.lua",b"unknown implementation").is_none());
 }
}
