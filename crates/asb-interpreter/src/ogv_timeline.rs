//! Bounded, read-only OGV hints from the current AST branch. Never execute Lua
//! helpers/conditions to guess a future filename; dynamic paths warm on use.
use crate::Interpreter;
use mlua::{Table,Value};
use std::collections::HashSet;
#[derive(Default)]
pub struct OgvTimelineCursor{ast:Option<Table>,position:Option<(String,String)>}
fn table(t:&Table,k:&str)->Option<Table>{t.raw_get(k).ok()}
fn text(t:&Table,k:impl mlua::IntoLua)->Option<String>{match t.raw_get::<Value>(k).ok()?{Value::String(s)=>{
    let s=s.to_str().ok()?.to_string();(!s.is_empty()&&s.len()<500&&!s.contains('\0')).then_some(s)},_=>None}}
fn movie(t:&Table)->bool{match t.raw_get::<Value>("movie").ok(){Some(Value::Integer(n))=>n>0,Some(Value::Number(n))=>n>0.,Some(Value::String(n))=>n.to_str().ok().and_then(|v|v.parse::<f64>().ok()).is_some_and(|v|v>0.),_=>false}}
fn ogv(path:String)->String{if path.to_ascii_lowercase().ends_with(".ogv"){path}else{format!("{path}.ogv")}}
fn paths(tag:&Table,userpath:Option<&str>)->Vec<String>{
    let mut out=Vec::new();let kind=text(tag,1).unwrap_or_default();let file=text(tag,"file");let path=text(tag,"path");
    if kind=="video"{if let Some(f)=file.as_ref().filter(|f|f.to_ascii_lowercase().ends_with(".ogv")){out.push(f.clone());}}
    if matches!(kind.as_str(),"bg"|"cg"|"ev"|"fg"|"anime"){
        if movie(tag){if let (Some(p),Some(f))=(&path,&file){out.push(ogv(format!("{p}{f}")));}}
        for i in 1..=16{let mode=text(tag,format!("ly{i}m"));if mode.as_deref().is_some_and(|m|matches!(m,"ogv"|"once")){
            if let (Some(p),Some(f))=(text(tag,format!("ly{i}p")).or(path.clone()),text(tag,format!("ly{i}"))){out.push(ogv(format!("{p}{f}")));}
        }}
    }
    if let Some(f)=text(tag,"effect"){
        if f.starts_with(':'){out.push(ogv(f));}else if let Some(p)=userpath{out.push(ogv(format!("{p}{f}")));}
    }
    out
}
impl Interpreter{
    pub fn query_ogv_timeline(&self,cursor:&mut OgvTimelineCursor)->Option<Vec<String>>{
        let g=self.lua().globals();let input=(||{let ast=table(&g,"ast")?;let ip=table(&table(&g,"scr")?,"ip")?;
            Some((ast,(text(&ip,"file")?,text(&ip,"block")?)))})();
        let Some((ast,position))=input else{return if cursor.ast.take().is_some(){cursor.position=None;Some(vec![])}else{None};};
        if cursor.ast.as_ref().is_some_and(|a|a.to_pointer()==ast.to_pointer())&&cursor.position.as_ref()==Some(&position){return None;}
        cursor.ast=Some(ast.clone());cursor.position=Some(position.clone());
        let userpath=table(&g,"ex").and_then(|t|text(&t,"userpath"));
        let mut block=position.1;let mut out=Vec::new();let mut seen=HashSet::new();let mut visited=HashSet::new();let mut left=4096usize;
        for _ in 0..128{
            if !visited.insert(block.clone()){break;}let Ok(tags)=ast.raw_get::<Table>(block.as_str())else{break;};
            let mut groups=vec![tags.clone()];if let Some(delay)=table(&tags,"delay"){
                for (_,v) in delay.pairs::<Value,Value>().take(32).flatten(){if let Value::Table(t)=v{groups.push(t);}}
            }
            for group in groups{for i in 1..=4096{
                if left==0{return Some(out);}left-=1;let Ok(Value::Table(t))=group.raw_get::<Value>(i)else{break;};
                for p in paths(&t,userpath.as_deref()){if seen.insert(p.clone()){out.push(p);if out.len()==16{return Some(out);}}}
            }}
            match text(&tags,"linknext"){Some(next)=>block=next,None=>break}
        }Some(out)
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn branch_pair_hints_layers_delays_and_dynamic_paths_are_conservative(){
        let it=Interpreter::new(Default::default());it.lua().load(r#"
        scr={ip={file='chapter',block='a'}};ex={userpath=':ani/'};
        ast={skipped={{'video',file=':ani/skipped.ogv'}},
          a={{'bg',path=':ani/',file='dust',movie='1'}, {'bg',file='static',path=':bg/',ly1='light',ly1p=':ani/',ly1m='once'},
            delay={{ {'video',file=':ani/dust.ogv'}, {'video',file='movie/open.mp4'}, {'video',file=function() error('must not run') end} }},linknext='b'},
          b={{'bg',effect='noise'},linknext='a'}};
        setmetatable(ast,{__index=function() error('must not run') end})"#).exec().unwrap();
        let mut c=OgvTimelineCursor::default();assert_eq!(it.query_ogv_timeline(&mut c).unwrap(),[":ani/dust.ogv",":ani/light.ogv",":ani/noise.ogv"]);
        assert!(it.query_ogv_timeline(&mut c).is_none());it.lua().load("scr.ip.block='b'").exec().unwrap();assert_eq!(it.query_ogv_timeline(&mut c).unwrap()[0],":ani/noise.ogv");
        it.lua().load("ast=nil").exec().unwrap();assert_eq!(it.query_ogv_timeline(&mut c),Some(vec![]));
    }
}
