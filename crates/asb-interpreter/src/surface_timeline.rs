//! Read-only hints for the standard Lua AST/cache protocol. No game functions,
//! file reads, condition evaluation, or mutation of the game's binding table.
use crate::Interpreter;
use mlua::{Table, Value};
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

fn read_block(
    tags: &Table, bound: &Table, ext: Option<&str>, fields: &[String], budget: &mut usize,
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
            }
            Some("fgf") => {
                if let Some(f) = text(&tag, "bg") { candidates.push(f); }
            }
            _ => {}
        }
        for path in candidates {
            if bound.raw_get::<bool>(path.as_str()).unwrap_or(false) && seen.insert(path.clone()) {
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
        let same_tables = cursor.ast.as_ref().is_some_and(|a| a.to_pointer() == ast.to_pointer())
            && cursor.bindings.as_ref().is_some_and(|a| a.to_pointer() == bound.to_pointer())
            && cursor.position.as_ref().is_some_and(|p| p.0 == position.0);
        if same_tables && cursor.position.as_ref() == Some(&position) { return None; }
        if !same_tables {
            cursor.blocks.clear();
            cursor.tags_left = MAX_TAGS;
        }
        // Remember unsupported positions too: don't retry malformed ASTs at 60 Hz.
        cursor.ast = Some(ast.clone());
        cursor.bindings = Some(bound.clone());
        cursor.position = Some(position.clone());
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
                    read_block(&tags, &bound, ext.as_deref(), &fields, &mut cursor.tags_left));
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
}

#[cfg(test)] mod tests {
    use super::*;
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
