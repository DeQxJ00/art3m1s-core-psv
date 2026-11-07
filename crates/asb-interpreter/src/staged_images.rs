//! Dependencies of images already staged by Lua, independent of story lookahead.
use mlua::{Lua, Table, Value};

const PROTOCOL: &str = "art3m1s.staged-image-protocol";
pub(crate) fn recognize(lua: &Lua, source: &[u8]) {
    let Ok(source)=std::str::from_utf8(source) else{return;};
    // These independent source units describe the data protocol and filenames.
    // Unknown implementations and bytecode retain their normal demand loading.
    let s:String=source.chars().filter(|c|!c.is_whitespace()).collect();
    let mut flags:u8=lua.named_registry_value(PROTOCOL).unwrap_or(0);
    if s.contains("functionimage_store(name,p)") && s.contains("table.insert(scr.img.buff,{name,p})")
        && s.contains("functionimage_loop()") && s.contains("image_view(\"bg\",v[2],true)") {flags|=1;}
    if s.contains("localt=csv.fg[p.name]") && s.contains("ifcsv.fg[body]thent=csv.fg[body]end")
        && s.contains("file=(\":fg/\"..path..face..\".png\")") {flags|=2;}
    if s.contains("localname=t.name:gsub(\"[ML]\",\"S\")") && s.contains("localbody=t.body:gsub(\"[ML]\",\"S\")")
        && s.contains("file=(\":face/\"..path..body..\".png\")") {flags|=4;}
    let _=lua.set_named_registry_value(PROTOCOL,flags);
}
fn table(t:&Table,key:&str)->Option<Table>{t.raw_get(key).ok()}
fn text(t:&Table,key:&str)->Option<String>{
    let Value::String(s)=t.raw_get::<Value>(key).ok()? else{return None;};
    let s=s.to_str().ok()?.to_string();
    (!s.is_empty()&&s.len()<=1024&&!s.contains(['\0','*','$','%','{','}'])).then_some(s)
}
fn paths(lua: &Lua) -> Vec<String> {
    let flags:u8=lua.named_registry_value(PROTOCOL).unwrap_or(0);
    if flags&1==0{return vec![];}
    let g=lua.globals();
    let Some(buff)=table(&g,"scr").and_then(|s|table(&s,"img")).and_then(|s|table(&s,"buff")) else{return vec![];};
    let Some(csv)=table(&g,"csv") else{return vec![];};
    let mut out=Vec::new();
    let mut add=|p:String|{if out.len()<32&&p.len()<=1024&&!out.contains(&p){out.push(p);}};
    for i in 1..=32 {
        let Ok(row)=buff.raw_get::<Table>(i) else{break;};
        let (Ok(mode),Ok(p))=(row.raw_get::<String>(1),row.raw_get::<Table>(2)) else{continue;};
        match mode.as_str() {
            "bg"|"cg"|"ev"=>{
                let Some(file)=text(&p,"file") else{continue;};
                let file=table(&csv,"cgname").and_then(|t|text(&t,&file)).unwrap_or(file);
                let mode=text(&p,"mode").unwrap_or(mode);
                if !matches!(mode.as_str(),"bg"|"cg"|"ev"){continue;}
                let cut=table(&csv,"cgsize").and_then(|t|table(&t,&file))
                    .is_some_and(|t| !matches!(t.raw_get::<Value>("cut"),Ok(Value::Nil|Value::Boolean(false))|Err(_)));
                if cut{add(format!(":{mode}/{file}_a"));add(format!(":{mode}/{file}_b"));}
                else{add(format!(":{mode}/{file}"));}
            }
            "fg" if flags&2!=0=>{
                let (Some(name),Some(body),Some(fg))=(text(&p,"name"),text(&p,"body"),table(&csv,"fg")) else{continue;};
                if let Some(path)=table(&fg,&body).or_else(||table(&fg,&name)).and_then(|t|text(&t,"path")){
                    add(format!(":fg/{path}{body}.png"));
                    if let Some(face)=text(&p,"face"){add(format!(":fg/{path}{face}.png"));}
                }
                if flags&4==0||table(&csv,"mw").and_then(|t|table(&t,"face")).is_none(){continue;}
                let small=|s:&str|s.replace(['M','L'],"S");
                let (name,body)=(small(&name),small(&body));
                // Portrait layout uses its own CSV row, not the body directory.
                if let Some(path)=table(&fg,&body).or_else(||table(&fg,&name)).and_then(|t|text(&t,"path")){
                    add(format!(":face/{path}{body}.png"));
                    if let Some(face)=text(&p,"face"){add(format!(":face/{path}{face}.png"));}
                }
            }
            _=>{}
        }
    }
    out
}

impl crate::Interpreter {
    pub fn query_staged_images(&self) -> Vec<String> {
        let mut pending=self.queued_image_files();
        for p in paths(self.lua()) {
            if pending.len()==32{break;}
            if !pending.contains(&p){pending.push(p);}
        }
        pending
    }
}

#[cfg(test)] mod tests {
    use super::*;
    const PROTOCOL: &[u8] = br#"
        function image_store(name, p)
            table.insert(scr.img.buff, { name, p })
        end
        function image_loop()
            fg_view(v[2])
            image_view("bg", v[2], true)
        end
        local t = csv.fg[p.name]
        if csv.fg[body] then t = csv.fg[body] end
        lyc2{eq=true, id=(idx..".1"), file=(":fg/"..path..face..".png"), x=(t.fx), y=(t.fy) }
        local name = t.name:gsub("[ML]", "S")
        local body = t.body:gsub("[ML]", "S")
        lyc2{eq=true, id=(id..".0"), file=(":face/"..path..body..".png"), x=(t.mwx), y=(t.mwy)}
    "#;
    fn setup() -> Lua {
        let lua=Lua::new();
        lua.load(r#"
            csv={cgname={alias='actual'},cgsize={},fg={heroM={path='medium/'},heroS={path='portrait/'}},mw={face={}}};
            scr={img={buff={{'bg',{file='alias'}},{'fg',{name='heroM',body='body_M',face='expression'}}}}};
            ast={{{'BG',file='unexecuted'}}};
            function fg_view() error('must not execute') end
        "#).exec().unwrap();
        lua
    }
    #[test] fn staged_dependencies_include_portrait_and_expression_without_ast_lookahead() {
        let lua=setup();recognize(&lua,PROTOCOL);
        assert_eq!(paths(&lua),[":bg/actual",":fg/medium/body_M.png",":fg/medium/expression.png",
            ":face/portrait/body_S.png",":face/portrait/expression.png"]);
        lua.load("scr.img.buff=nil").exec().unwrap();
        assert!(paths(&lua).is_empty());
    }
    #[test] fn staged_unknown_protocol_and_metatables_do_not_run() {
        let lua=setup();assert!(paths(&lua).is_empty());
        recognize(&lua,b"function image_store() end");assert!(paths(&lua).is_empty());
        recognize(&lua,PROTOCOL);
        lua.load("scr.img.buff={{'bg',{file='missing'}},{'unknown',{file='no'}}};setmetatable(csv.cgsize,{__index=function() error('no') end})").exec().unwrap();
        assert_eq!(paths(&lua),[":bg/missing"]);
    }
    #[test] fn staged_alias_cut_and_body_table_override_are_preserved() {
        let lua=setup();recognize(&lua,PROTOCOL);
        lua.load("csv.cgsize.actual={cut=320};csv.fg.body_M={path='special/'};csv.mw.face=nil").exec().unwrap();
        assert_eq!(paths(&lua),[":bg/actual_a",":bg/actual_b",":fg/special/body_M.png",":fg/special/expression.png"]);
    }
}
