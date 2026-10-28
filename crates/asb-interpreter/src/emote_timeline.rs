//! Conservative PSB hints from already loaded AST data. No script execution.
use crate::Interpreter;
use mlua::{Table,Value};
use std::collections::HashSet;
#[derive(Default)]
pub struct EmoteTimelineCursor { ast:Option<Table>, position:Option<(String,BlockKey,String)> }
fn table(t:&Table,k:&str)->Option<Table>{t.raw_get(k).ok()}
fn text(t:&Table,k:&str)->Option<String>{
    match t.raw_get::<Value>(k).ok()?{Value::String(s)=>{let s=s.to_str().ok()?.to_string();
        (!s.is_empty()&&s.len()<=1024&&!s.contains('\0')).then_some(s)},_=>None}
}
// Lua numeric keys and string keys are distinct, including 1 versus "1".
#[derive(Clone,PartialEq,Eq,Hash)]
enum BlockKey { Named(String), Indexed(i64) }
impl BlockKey {
    fn read(t:&Table,key:&str)->Option<Self>{
        match t.raw_get::<Value>(key).ok()? {
            Value::String(_)=>text(t,key).map(Self::Named),
            Value::Integer(n) if n>0=>Some(Self::Indexed(n)),
            Value::Number(n) if n.is_finite()&&n>=1.&&n<=9007199254740991.&&n.fract()==0.=>Some(Self::Indexed(n as i64)),
            _=>None,
        }
    }
    fn tags(&self,ast:&Table)->Option<Table>{
        match self {Self::Named(s)=>ast.raw_get(s.as_str()).ok(),Self::Indexed(n)=>ast.raw_get(*n).ok()}
    }
    fn next(&self,tags:&Table)->Option<Self>{
        match tags.raw_get::<Value>("linknext").ok()? {
            Value::Nil=>match self {Self::Indexed(n)=>n.checked_add(1).map(Self::Indexed),Self::Named(_)=>None},
            _=>Self::read(tags,"linknext"),
        }
    }
}
impl Interpreter {
    pub fn query_emote_timeline(&self,cursor:&mut EmoteTimelineCursor)->Option<Vec<String>>{
        let g=self.lua().globals();
        let inputs=(||{
            let game=table(&g,"game")?;let ext=text(&game,"fgext")?;
            if !ext.eq_ignore_ascii_case(".psb"){return None;}
            let ast=table(&g,"ast")?;let ip=table(&table(&g,"scr")?,"ip")?;
            Some((ast,(text(&ip,"file")?,BlockKey::read(&ip,"block")?,ext)))
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
            let Some(tags)=block.tags(&ast) else{break;};
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
            match block.next(&tags){Some(next)=>block=next,None=>break}
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
    #[test]fn numeric_blocks_advance_and_restore_without_executing_lua(){
        let it=Interpreter::new(Default::default());
        it.lua().load(r#"game={fgext='.psb'};scr={ip={file='chapter',block=1}};
        ast={ {{'text'}}, {{'fg',path=':fg/',file='one'}},
          {{'fg',path=':fg/',file='one'}}, {{'fg',path=':fg/',file='two'}},
          {{'fg',path=':fg/',file='three'}} };
        ast['1']={{'fg',path=':fg/',file='string-key'}};
        setmetatable(ast,{__index=function() error('must not run') end})"#).exec().unwrap();
        let mut c=EmoteTimelineCursor::default();
        assert_eq!(it.query_emote_timeline(&mut c).unwrap(),[":fg/one.psb",":fg/two.psb"]);
        assert!(it.query_emote_timeline(&mut c).is_none());
        it.lua().load("scr.ip.block=4").exec().unwrap();
        assert_eq!(it.query_emote_timeline(&mut c).unwrap(),[":fg/two.psb",":fg/three.psb"]);
        it.lua().load("scr.ip.block=1;ast={{{'fg',path=':fg/',file='restored'}}}").exec().unwrap();
        assert_eq!(it.query_emote_timeline(&mut c).unwrap(),[":fg/restored.psb"]);
        it.lua().load("scr.ip.block=1.5").exec().unwrap();
        assert_eq!(it.query_emote_timeline(&mut c),Some(vec![]));
    }
    #[test]fn numeric_links_preserve_branch_and_stop_at_gaps_or_invalid_targets(){
        let it=Interpreter::new(Default::default());
        it.lua().load(r#"game={fgext='.psb'};scr={ip={file='chapter',block=1}};
        ast={ {{'fg',path=':fg/',file='one'},linknext=3},
          {{'fg',path=':fg/',file='skipped'}},
          {delay={{{'fg',path=':fg/',file='two'}}},linknext=1} }"#).exec().unwrap();
        let mut c=EmoteTimelineCursor::default();
        assert_eq!(it.query_emote_timeline(&mut c).unwrap(),[":fg/one.psb",":fg/two.psb"]);
        it.lua().load("ast[1].linknext=false; scr.ip.file='invalid-link'").exec().unwrap();
        assert_eq!(it.query_emote_timeline(&mut c).unwrap(),[":fg/one.psb"]);
        it.lua().load("ast[1].linknext=nil;ast[2]=nil;scr.ip.file='gap'").exec().unwrap();
        assert_eq!(it.query_emote_timeline(&mut c).unwrap(),[":fg/one.psb"]);
    }
    #[test]fn numeric_scan_stays_bounded(){
        let it=Interpreter::new(Default::default());
        it.lua().load(r#"game={fgext='.psb'};scr={ip={file='chapter',block=1}};
        ast={};for i=1,65 do ast[i]={{'text'}} end
        ast[65]={{'fg',path=':fg/',file='too-far'}}"#).exec().unwrap();
        assert_eq!(it.query_emote_timeline(&mut EmoteTimelineCursor::default()),Some(vec![]));
    }

}
