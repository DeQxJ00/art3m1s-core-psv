//! Read-only hints for the standard Lua AST/cache protocol. No game functions,
//! file reads, condition evaluation, or mutation of the game's binding table.
use crate::Interpreter;
use mlua::{Lua, Table, Value};
use std::collections::{HashMap, HashSet};

const MAX_BLOCKS: usize = 1024;
const MAX_TAGS: usize = 16384;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum BlockKey { Label(String), Index(mlua::Integer) }
impl BlockKey {
    fn read(t: &Table, key: &str) -> Option<Self> {
        match t.raw_get::<Value>(key).ok()? {
            Value::Integer(n) if n > 0 => Some(Self::Index(n)),
            Value::Number(n) if n > 0.0 && n < mlua::Integer::MAX as f64 && n.fract() == 0.0 => Some(Self::Index(n as mlua::Integer)),
            Value::String(_) => text(t,key).map(Self::Label),
            _ => None,
        }
    }
    fn tags(&self, ast: &Table) -> Option<Table> {
        match self { Self::Label(s) => ast.raw_get(s.as_str()).ok(), Self::Index(n) => ast.raw_get(*n).ok() }
    }
    fn successor(&self) -> Option<Self> {
        match self { Self::Index(n) => n.checked_add(1).map(Self::Index), _ => None }
    }
}

struct BlockImages {
    paths: Vec<String>,
    next: Option<BlockKey>,
}

#[derive(Default)]
pub struct SurfaceTimelineCursor {
    // Holding these tables prevents pointer reuse across chapters/cache resets.
    ast: Option<Table>,
    bindings: Option<Table>,
    position: Option<(String, BlockKey)>,
    // AST commands are immutable within a chapter. Parse each block only once,
    // then walk these small lists when the dialogue block changes.
    blocks: HashMap<BlockKey, Option<BlockImages>>,
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
            Some(kind @ ("BG" | "EV" | "EV_C" | "CG" | "CGS")) => {
                if let Some(mut file) = text(&tag, "file") {
                    let csv = table(&lua.globals(), "csv");
                    if let Some(alias) = csv.as_ref().and_then(|c| table(c,"cgname")).and_then(|t| text(&t,&file)) { file = alias; }
                    let mode = text(&tag,"mode").unwrap_or_else(|| {
                        if kind == "EV_C" { "ev".to_owned() } else { kind.to_ascii_lowercase() }
                    });
                    if matches!(mode.as_str(), "bg"|"ev"|"cg"|"cgs") && !file.contains("**") {
                        let cut = csv.and_then(|c| table(&c,"cgsize")).and_then(|t| table(&t,&file))
                            .is_some_and(|t| !matches!(t.raw_get::<Value>("cut"), Ok(Value::Nil|Value::Boolean(false))|Err(_)));
                        if cut { candidates.extend([format!(":{mode}/{file}_a"),format!(":{mode}/{file}_b")]); }
                        else { candidates.push(format!(":{mode}/{file}")); }
                    }
                }
            }
            Some("ST" | "ST0") => {
                if let (Some(name),Some(body),Some(fg)) = (text(&tag,"name"),text(&tag,"body"),table(&lua.globals(),"csv").and_then(|c| table(&c,"fg"))) {
                    if let Some(path) = table(&fg,&body).or_else(|| table(&fg,&name)).and_then(|t| text(&t,"path")) {
                        candidates.push(format!(":fg/{path}{body}.png"));
                        if let Some(face) = text(&tag,"face") { candidates.push(format!(":fg/{path}{face}.png")); }
                    }
                }
            }
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
    Some(BlockImages { paths, next: BlockKey::read(tags, "linknext") })
}

impl Interpreter {
    pub fn query_surface_timeline(&self, cursor: &mut SurfaceTimelineCursor) -> Option<Vec<String>> {
        let g = self.lua().globals();
        let ast = table(&g, "ast")?;
        let ip = table(&table(&g, "scr")?, "ip")?;
        let bound = table(&table(&g, "cachebuff")?, "img")?;
        let position = (text(&ip, "file")?, BlockKey::read(&ip, "block")?);
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
                let images = block.tags(&ast).and_then(|tags|
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
            Some((ast, (text(&ip, "file")?, BlockKey::read(&ip, "block")?)))
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
            let Some(tags) = block.tags(&ast) else { break; };
            // A branch can have arbitrary conditions and side effects. Stop at
            // the entire block rather than guessing a destination or evaluating it.
            let mut safe = true;
            for i in 1..=512 {
                let Ok(value) = tags.raw_get::<Value>(i) else { safe = false; break; };
                match value {
                    Value::Nil => break,
                    Value::Table(t) => {
                        if matches!(t.raw_get::<String>(1).ok().as_deref(),
                            Some("select"|"sel"|"jump"|"call"|"return"|"if"|"elseif"|"else"|"lua"|"eval"|"SELECT"|"goto"|"gosub"|"end"|"RM_E"|"XM_E"|"TL")) {
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
            match images.next.or_else(|| block.successor()) { Some(next) => block = next, None => break }
        }
        Some(out)
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test]
    fn difference_cg_uses_ev_directory_before_transition() {
        let it=Interpreter::new(Default::default());
        it.lua().load(r#"
            scr={ip={file='chapter',block=1}};
            ast={{{'EV_C',file='scene_variant',time=500}},{{'EV',file='next_scene'}}};
        "#).exec().unwrap();
        let mut cursor=SurfaceTimelineCursor::default();
        assert_eq!(it.query_surface_lookahead(&mut cursor).unwrap(),[":ev/scene_variant",":ev/next_scene"]);
    }
    #[test]
    fn numeric_story_blocks_preload_images_and_stop_at_uppercase_branch() {
        let it=Interpreter::new(Default::default());
        it.lua().load(r#"
            scr={ip={file='chapter',block=1}};
            csv={cgname={alias='actual'},cgsize={},fg={hero={path='hero/'}}};
            ast={
              {{'BG',file='alias'}},
              {{'ST',name='hero',body='body',face='face'}},
              {{'SELECT'},{'EV',file='wrong-route'}},
              {{'BG',file='later'}}}
        "#).exec().unwrap();
        let mut c=SurfaceTimelineCursor::default();
        assert_eq!(it.query_surface_lookahead(&mut c).unwrap(),
            [":bg/actual",":fg/hero/body.png",":fg/hero/face.png"]);
        it.lua().load("scr.ip.block=4").exec().unwrap();
        assert_eq!(it.query_surface_lookahead(&mut c).unwrap(),[":bg/later"]);
    }
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
