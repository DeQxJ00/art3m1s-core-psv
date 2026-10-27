//! Read-only hints for the standard Lua AST/cache protocol. No game functions,
//! file reads, condition evaluation, or mutation of the game's binding table.
use crate::Interpreter;
use mlua::{Lua, Table, Value};
use std::collections::{HashMap, HashSet};

const MAX_BLOCKS: usize = 1024;
const MAX_TAGS: usize = 16384;

struct BlockImages {
    paths: Vec<String>,
    next: Option<String>,
}

#[derive(Default)]
pub struct SurfaceTimelineCursor {
    // Holding these tables prevents pointer reuse across chapters/cache resets.
    ast: Option<Table>,
    bindings: Option<Table>,
    position: Option<(String, String)>,
    // AST commands are immutable within a chapter. Parse each block only once,
    // then walk these small lists when the dialogue block changes.
    blocks: HashMap<String, Option<BlockImages>>,
    tags_left: usize,
    portrait_epoch: u64,
}

fn table(t: &Table, key: &str) -> Option<Table> { t.raw_get(key).ok() }
fn text(t: &Table, key: &str) -> Option<String> {
    match t.raw_get::<Value>(key).ok()? {
        Value::String(s) => {
            let s = s.to_str().ok()?.to_string();
            (!s.is_empty() && s.len() <= 1024 && !s.contains('\0')).then_some(s)
        }
        _ => None,
    }
}

fn read_block(lua: &Lua,
    tags: &Table, bound: Option<&Table>, ext: Option<&str>, fields: &[String], budget: &mut usize,
) -> Option<BlockImages> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let mut visit = |tag: Table| {
        let mut candidates = Vec::new();
        match tag.raw_get::<String>(1).ok().as_deref() {
            Some("bg") => {
                if let (Some(p), Some(f)) = (text(&tag, "path"), text(&tag, "file")) {
                    candidates.push(format!("{p}{f}"));
                }
            }
            Some("fg") => {
                if let (Some(p), Some(f), Some(ext)) = (text(&tag, "path"), text(&tag, "file"), ext) {
                    candidates.push(format!("{p}{f}{ext}"));
                    for field in fields {
                        if let Some(f) = text(&tag, field) { candidates.push(format!("{p}{f}{ext}")); }
                    }
                }
                candidates.extend(crate::portrait_preload::paths(lua, &tag));
            }
            Some("fgf") => {
                if let Some(f) = text(&tag, "bg") { candidates.push(f); }
            }
            _ => {}
        }
        for path in candidates {
            let allowed = bound.map_or_else(
                || path.len() <= 1024 && !path.contains(['$', '%', '{', '}', '\0']),
                |b| b.raw_get::<bool>(path.as_str()).unwrap_or(false));
            if allowed && seen.insert(path.clone()) {
                paths.push(path);
            }
        }
    };
    let mut sequence = |items: &Table, budget: &mut usize| -> Option<()> {
        for i in 1..=MAX_TAGS {
            *budget = budget.checked_sub(1)?;
            match items.raw_get::<Value>(i).ok()? {
                Value::Table(t) => visit(t),
                Value::Nil => return Some(()),
                _ => {}
            }
        }
        None
    };
    // Keep the ENTIRE current block, not just tags after scr.ip.count. bg/fg
    // commands stage images in Lua and commit them later at image_loop/extrans.
    // Passing an instruction is not proof that its prefetched pixels were used.
    sequence(tags, budget)?;
    if let Some(delay) = table(tags, "delay") {
        for entry in delay.pairs::<Value, Table>() {
            *budget = budget.checked_sub(1)?;
            let (_, items) = entry.ok()?;
            sequence(&items, budget)?;
        }
    }
    Some(BlockImages { paths, next: text(tags, "linknext") })
}

impl Interpreter {
    pub fn query_surface_timeline(&self, cursor: &mut SurfaceTimelineCursor) -> Option<Vec<String>> {
        let g = self.lua().globals();
        let ast = table(&g, "ast")?;
        let ip = table(&table(&g, "scr")?, "ip")?;
        let bound = table(&table(&g, "cachebuff")?, "img")?;
        let position = (text(&ip, "file")?, text(&ip, "block")?);
        let portrait_epoch = crate::portrait_preload::epoch(self.lua());
        let same_tables = cursor.ast.as_ref().is_some_and(|a| a.to_pointer() == ast.to_pointer())
            && cursor.bindings.as_ref().is_some_and(|a| a.to_pointer() == bound.to_pointer())
            && cursor.position.as_ref().is_some_and(|p| p.0 == position.0)
            && cursor.portrait_epoch == portrait_epoch;
        if same_tables && cursor.position.as_ref() == Some(&position) { return None; }
        if !same_tables {
            cursor.blocks.clear();
            cursor.tags_left = MAX_TAGS;
        }
        // Remember unsupported positions too: don't retry malformed ASTs at 60 Hz.
        cursor.ast = Some(ast.clone());
        cursor.bindings = Some(bound.clone());
        cursor.position = Some(position.clone());
        cursor.portrait_epoch = portrait_epoch;
        let ext = table(&g, "game").and_then(|t| text(&t, "fgext"));
        let mut fields = Vec::new();
        if let Some(t) = table(&g, "init").and_then(|t| table(&t, "fgid")) {
            for entry in t.pairs::<Value, Value>().take(32) {
                if let Ok((Value::String(k), _)) = entry {
                    let k = k.to_str().ok()?.to_string();
                    if k != "file" { fields.push(k); }
                }
            }
        }
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut visited = HashSet::new();
        let mut block = position.1;
        loop {
            if !visited.insert(block.clone()) || visited.len() > MAX_BLOCKS { return None; }
            if !cursor.blocks.contains_key(&block) {
                if cursor.blocks.len() >= MAX_BLOCKS { return None; }
                let images = ast.raw_get::<Table>(block.as_str()).ok().and_then(|tags|
                    read_block(self.lua(), &tags, Some(&bound), ext.as_deref(), &fields, &mut cursor.tags_left));
                cursor.blocks.insert(block.clone(), images);
            }
            let images = cursor.blocks.get(&block)?.as_ref()?;
            for path in &images.paths {
                if seen.insert(path.clone()) { out.push(path.clone()); }
            }
            match &images.next {
                Some(next) => block = next.clone(),
                None => break,
            }
        }
        Some(out)
    }

    /// Opt-in rolling image hints. No Lua callbacks or conditions are executed.
    /// Unlike the legacy plan, images do not need a cachebuff/Lua binding.
    pub fn query_surface_lookahead(&self, cursor: &mut SurfaceTimelineCursor) -> Option<Vec<String>> {
        let g = self.lua().globals();
        let input = (|| {
            let ast = table(&g, "ast")?;
            let ip = table(&table(&g, "scr")?, "ip")?;
            Some((ast, (text(&ip, "file")?, text(&ip, "block")?)))
        })();
        let Some((ast, position)) = input else {
            return cursor.position.take().map(|_| Vec::new());
        };
        let epoch = crate::portrait_preload::epoch(self.lua());
        let same = cursor.ast.as_ref().is_some_and(|t| t.to_pointer() == ast.to_pointer())
            && cursor.position.as_ref().is_some_and(|p| p.0 == position.0)
            && cursor.portrait_epoch == epoch;
        if same && cursor.position.as_ref() == Some(&position) { return None; }
        cursor.ast = Some(ast.clone());
        cursor.position = Some(position.clone());
        cursor.portrait_epoch = epoch;
        let ext = table(&g, "game").and_then(|t| text(&t, "fgext"));
        let fields: Vec<_> = table(&g, "init").and_then(|t| table(&t, "fgid"))
            .map(|t| t.pairs::<String, Value>().take(32).filter_map(|e| e.ok())
                .map(|(k, _)| k).filter(|k| k != "file").collect()).unwrap_or_default();
        let mut block = position.1;
        let mut visited = HashSet::new();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut budget = 512;
        for _ in 0..4 {
            if !visited.insert(block.clone()) { break; }
            let Ok(tags) = ast.raw_get::<Table>(block.as_str()) else { break; };
            // A branch can have arbitrary conditions and side effects. Stop at
            // the entire block rather than guessing a destination or evaluating it.
            let mut safe = true;
            for i in 1..=512 {
                let Ok(value) = tags.raw_get::<Value>(i) else { safe = false; break; };
                match value {
                    Value::Nil => break,
                    Value::Table(t) => {
                        if matches!(t.raw_get::<String>(1).ok().as_deref(),
                            Some("select"|"sel"|"jump"|"call"|"return"|"if"|"elseif"|"else"|"lua"|"eval")) {
                            safe = false; break;
                        }
                    }
                    _ => { safe = false; break; }
                }
            }
            // Delayed commands may carry conditional branches too.
            if !safe || table(&tags, "delay").is_some() { break; }
            let Some(images) = read_block(self.lua(), &tags, None, ext.as_deref(), &fields, &mut budget) else { break; };
            for path in images.paths {
                if seen.insert(path.clone()) { out.push(path); }
                if out.len() == 32 { return Some(out); }
            }
            match images.next { Some(next) => block = next, None => break }
        }
        Some(out)
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test]
    fn lookahead_without_lua_bindings_is_bounded_and_replans() {
        let it=Interpreter::new(Default::default());
        it.lua().load(r#"
            scr={ip={file='chapter',block='b1'}}; game={fgext='.png'}; ast={}
            for i=1,100 do ast['b'..i]={{'bg',path=':bg/',file=tostring(i)},linknext='b'..(i+1)} end
            setmetatable(ast,{__index=function() error('no execution') end})
        "#).exec().unwrap();
        let mut c=SurfaceTimelineCursor::default();
        assert_eq!(it.query_surface_lookahead(&mut c).unwrap(),[":bg/1",":bg/2",":bg/3",":bg/4"]);
        assert!(it.query_surface_lookahead(&mut c).is_none());
        it.lua().load("scr.ip.block='b50'").exec().unwrap();
        assert_eq!(it.query_surface_lookahead(&mut c).unwrap()[0],":bg/50");
        it.lua().load("scr.ip.block='b1'").exec().unwrap();
        assert_eq!(it.query_surface_lookahead(&mut c).unwrap()[0],":bg/1");
        it.lua().load("scr=nil").exec().unwrap();
        assert!(it.query_surface_lookahead(&mut c).unwrap().is_empty());
        assert!(it.query_surface_lookahead(&mut c).is_none());
    }
    #[test]
    fn lookahead_stops_at_branch_caps_images_and_rejects_expressions() {
        let it=Interpreter::new(Default::default());
        it.lua().load(r#"
            scr={ip={file='chapter',block='a'}}; ast={
                a={{'bg',path=':bg/',file='first'},linknext='branch'},
                branch={{'select'},{'bg',path=':bg/',file='wrong-route'},linknext='later'},
                later={{'bg',path=':bg/',file='later'}}}
        "#).exec().unwrap();
        let mut c=SurfaceTimelineCursor::default();
        assert_eq!(it.query_surface_lookahead(&mut c).unwrap(),[":bg/first"]);
        it.lua().load(r#"ast={a={{'bg',path=':bg/',file='$variable'}}}
            for i=2,80 do ast.a[i]={'bg',path=':bg/',file=tostring(i)} end"#).exec().unwrap();
        let plan=it.query_surface_lookahead(&mut c).unwrap();
        assert_eq!(plan.len(),32);assert_eq!(plan[0],":bg/2");
    }
    #[test]
    fn portrait_bindings_refresh_same_position_follow_story_and_never_run_the_helper() {
        let it = Interpreter::new(Default::default());
        it.lua().load(r#"
            scr={ip={file='chapter',block='a',count=1}}; game={fgext='.png'};
            init={fgid={file=0,file1=1}}; cachebuff={img={}};
            ast={a={{'fg',file='one',path=':fg/',mode=1},linknext='b'},
                 b={{'fg',file='two',path=':fg/',mode=1}}};
            function getMWFaceFile(p, flag)
                return {path=':fa/',file={file=p.file},file1={file=p.file..'_face'}}
            end
            function setImageStack(p) cachebuff.img[p]=true end
        "#).exec().unwrap();
        let mut cursor = SurfaceTimelineCursor::default();
        assert!(it.query_surface_timeline(&mut cursor).unwrap().is_empty());
        let ast = it.lua().globals().raw_get::<Table>("ast").unwrap();
        for block in ["a", "b"] {
            let tag = ast.raw_get::<Table>(block).unwrap().raw_get::<Table>(1).unwrap();
            assert!(crate::portrait_preload::preload(it.lua(), &tag).unwrap());
        }
        it.lua().load("function getMWFaceFile() error('timeline must not evaluate helpers') end").exec().unwrap();
        assert_eq!(it.query_surface_timeline(&mut cursor).unwrap(),
            [":fa/one.png", ":fa/one_face.png", ":fa/two.png", ":fa/two_face.png"]);
        assert!(it.query_surface_timeline(&mut cursor).is_none());
        it.lua().load("scr.ip.block='b'").exec().unwrap();
        assert_eq!(it.query_surface_timeline(&mut cursor).unwrap(), [":fa/two.png", ":fa/two_face.png"]);
        it.lua().load("scr.ip.block='a'").exec().unwrap();
        assert_eq!(it.query_surface_timeline(&mut cursor).unwrap().len(), 4);
    }
    #[test] fn follows_position_preserves_future_reuse_and_does_not_call_game_code(){
        let it=Interpreter::new(Default::default());
        it.lua().load(r#"
            scr={ip={file='chapter',block='old',count=1}};game={fgext='.png'};init={fgid={face=1}}
            cachebuff={img={[':bg/old']=true,[':bg/reused']=true,[':bg/next']=true,[':fg/body.png']=true,[':fg/face.png']=true}}
            ast={old={{'bg',path=':bg/',file='old'},{'bg',path=':bg/',file='reused'},linknext='current'},
                 current={{'text'},linknext='next'},next={{'bg',path=':bg/',file='next'},{'fg',path=':fg/',file='body',face='face'},linknext='reuse'},
                 reuse={{'bg',path=':bg/',file='reused'}}}
            setmetatable(ast,{__index=function() error('must not run') end})
        "#).exec().unwrap();
        let mut c=SurfaceTimelineCursor::default();
        assert_eq!(it.query_surface_timeline(&mut c).unwrap(),[":bg/old",":bg/reused",":bg/next",":fg/body.png",":fg/face.png"]);
        assert!(it.query_surface_timeline(&mut c).is_none());
        it.lua().load("scr.ip.block='current'").exec().unwrap();
        assert_eq!(it.query_surface_timeline(&mut c).unwrap(),[":bg/next",":fg/body.png",":fg/face.png",":bg/reused"]);
        it.lua().load("scr.ip.block='old'").exec().unwrap();
        assert_eq!(it.query_surface_timeline(&mut c).unwrap()[0],":bg/old");
        it.lua().load("ast.reuse.linknext='old';local a=ast;ast={old=a.old,current=a.current,next=a.next,reuse=a.reuse}").exec().unwrap();
        assert!(it.query_surface_timeline(&mut c).is_none());
    }
    #[test]
    fn instruction_progress_keeps_staged_images_until_next_dialogue_block() {
        let it = Interpreter::new(Default::default());
        it.lua().load(r#"
            scr={ip={file='chapter',block='a',count=1}};
            game={fgext='.png'};init={fgid={file=0,face=1}};
            cachebuff={img={[':bg/first']=true,[':fg/body.png']=true,[':fg/face.png']=true,[':bg/next']=true}};
            ast={a={{'bg',path=':bg/',file='first'},
                    {'fg',path=':fg/',file='body',face='face'}, {'text'},linknext='b'},
                 b={{'bg',path=':bg/',file='next'},{'text'}}};
        "#).exec().unwrap();
        let mut c = SurfaceTimelineCursor::default();
        let first = it.query_surface_timeline(&mut c).unwrap();
        assert_eq!(first,[":bg/first",":fg/body.png",":fg/face.png",":bg/next"]);
        let parsed_budget = c.tags_left;
        for command in ["scr.ip.count=2", "scr.ip.count=3", "scr.ip.count=nil"] {
            it.lua().load(command).exec().unwrap();
            assert!(it.query_surface_timeline(&mut c).is_none());
            assert_eq!(c.tags_left, parsed_budget);
        }
        it.lua().load("scr.ip.block='b';scr.ip.count=1").exec().unwrap();
        assert_eq!(it.query_surface_timeline(&mut c).unwrap(),[":bg/next"]);
        assert_eq!(c.tags_left, parsed_budget, "next dialogue uses the parsed index");
        it.lua().load("scr.ip.block='a';scr.ip.count=3").exec().unwrap();
        assert_eq!(it.query_surface_timeline(&mut c).unwrap(),first);
    }

    #[test] fn replacement_bindings_and_delayed_images_refresh_without_executing_metatables(){
        let it=Interpreter::new(Default::default());
        it.lua().load(r#"
            scr={ip={file='chapter',block='a',count=2}};
            ast={a={{'bg',path=':bg/',file='past'},{'text'},delay={late={{'bg',path=':bg/',file='later'}}}}};
            cachebuff={img={[':bg/past']=true}};
        "#).exec().unwrap();
        let mut c=SurfaceTimelineCursor::default();
        assert_eq!(it.query_surface_timeline(&mut c).unwrap(),[":bg/past"]);
        it.lua().load("scr.ip.count=nil").exec().unwrap();
        assert!(it.query_surface_timeline(&mut c).is_none());
        it.lua().load("scr.ip.count=2").exec().unwrap();
        it.lua().load("cachebuff.img={[':bg/later']=true}").exec().unwrap();
        assert_eq!(it.query_surface_timeline(&mut c).unwrap(),[":bg/later"]);
        it.lua().load("scr.ip.block='missing';setmetatable(ast,{__index=function() error('not raw') end})").exec().unwrap();
        assert!(it.query_surface_timeline(&mut c).is_none());
        assert!(it.query_surface_timeline(&mut c).is_none());
    }
}
