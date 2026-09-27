//! Conservative PSB hints from already loaded AST data. No script execution.
use crate::Interpreter;
use mlua::{Table,Value};
use std::collections::HashSet;
#[derive(Default)]
pub struct EmoteTimelineCursor { ast:Option<Table>, position:Option<(String,String,String)> }
fn table(t:&Table,k:&str)->Option<Table>{t.raw_get(k).ok()}
fn text(t:&Table,k:&str)->Option<String>{
    match t.raw_get::<Value>(k).ok()?{Value::String(s)=>{let s=s.to_str().ok()?.to_string();
        (!s.is_empty()&&s.len()<=1024&&!s.contains('\0')).then_some(s)},_=>None}
}
impl Interpreter {
    pub fn query_emote_timeline(&self,cursor:&mut EmoteTimelineCursor)->Option<Vec<String>>{
        let g=self.lua().globals();
        let inputs=(||{
            let game=table(&g,"game")?;let ext=text(&game,"fgext")?;
            if !ext.eq_ignore_ascii_case(".psb"){return None;}
            let ast=table(&g,"ast")?;let ip=table(&table(&g,"scr")?,"ip")?;
            Some((ast,(text(&ip,"file")?,text(&ip,"block")?,ext)))
        })();
        let Some((ast,position))=inputs else{
            return if cursor.ast.take().is_some(){cursor.position=None;Some(vec![])}else{None};
        };
        if cursor.ast.as_ref().is_some_and(|a|a.to_pointer()==ast.to_pointer())&&cursor.position.as_ref()==Some(&position){return None;}
        cursor.ast=Some(ast.clone());cursor.position=Some(position.clone());
        let mut block=position.1;let ext=position.2;let mut out=Vec::new();let mut seen=HashSet::new();
        let mut visited=HashSet::new();let mut left=2048usize;
        for _ in 0..64{
            if !visited.insert(block.clone()){break;}
            let Ok(tags)=ast.raw_get::<Table>(block.as_str()) else{break;};
            let mut groups=vec![tags.clone()];
            if let Some(delay)=table(&tags,"delay"){
                for (_,group) in delay.pairs::<Value,Value>().take(32).flatten(){if let Value::Table(t)=group{groups.push(t);}}
            }
            for group in groups{
                for i in 1..=2048{
                    if left==0{return Some(out);}left-=1;
                    let Ok(Value::Table(tag))=group.raw_get::<Value>(i) else{break;};
                    if tag.raw_get::<String>(1).ok().as_deref()!=Some("fg"){continue;}
                    if let (Some(path),Some(file))=(text(&tag,"path"),text(&tag,"file")){
                        let name=format!("{path}{file}{ext}");
                        if seen.insert(name.clone()){out.push(name);if out.len()==2{return Some(out);}}
                    }
                }
            }
            match text(&tags,"linknext"){Some(next)=>block=next,None=>break}
        }
        Some(out)
    }
}
#[cfg(test)] mod tests{
    use super::*;
    #[test]fn unbound_models_follow_current_branch_without_running_lua(){
        let it=Interpreter::new(Default::default());
        it.lua().load(r#"game={fgext='.psb'};scr={ip={file='chapter',block='b'}};
        ast={a={{'fg',path=':fg/',file='skipped'},linknext='b'},
        b={{'fg',path=':fg/',file='one'},{'fg',path=':fg/',file='one'},linknext='c'},
        c={{'fg',path=':fg/',file='two'},linknext='a'}};
        setmetatable(ast,{__index=function() error('must not run') end})"#).exec().unwrap();
        let mut cursor=EmoteTimelineCursor::default();
        assert_eq!(it.query_emote_timeline(&mut cursor).unwrap(),[":fg/one.psb",":fg/two.psb"]);
        assert!(it.query_emote_timeline(&mut cursor).is_none());
        it.lua().load("scr.ip.block='c'").exec().unwrap();
        assert_eq!(it.query_emote_timeline(&mut cursor).unwrap(),[":fg/two.psb",":fg/skipped.psb"]);
        it.lua().load("game.fgext='.png'").exec().unwrap();
        assert_eq!(it.query_emote_timeline(&mut cursor),Some(vec![]));
    }
}
