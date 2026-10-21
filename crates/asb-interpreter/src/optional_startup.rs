//! Guard the recognized optional startup hook after loading its module.
//! This does not change file existence, install globals or suppress Lua errors.

const BLOCK: &str = "\t\tlocal nm = \"system/extend/auth.lua\"\n\t\tif isFile(nm) then\n\t\t\te:include(nm)\t\t-- 認証\n\t\t\tauthentication()\t-- 認証開始\n\t\tend";

pub(crate) fn repair_source(path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    if path.replace('\\', "/").to_ascii_lowercase() != "system/init.lua" {
        return None;
    }
    let source = std::str::from_utf8(bytes).ok()?;
    let normalized = source.replace("\r\n", "\n");
    if normalized.matches(BLOCK).count() != 1 {
        return None;
    }
    let guarded = BLOCK.replace("authentication()\t-- 認証開始", concat!(
        "if type(authentication) == \"function\" then authentication()\n",
        "\t\t\telseif authentication == nil then\n",
        "\t\t\t\te:debug{level=1, raw=true, data=\"[optional-startup] module has no initialization hook; skipped\"}\n",
        "\t\t\telse error(\"optional startup hook is not a function\") end"
    ));
    let result = normalized.replacen(BLOCK, &guarded, 1);
    Some(if source.contains("\r\n") { result.replace('\n', "\r\n") } else { result }.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Lua, Value};

    #[test]
    fn include_pipeline_guards_empty_module_and_returns_to_caller() {
        use std::sync::{Arc, Mutex};
        use crate::lua_engine::{EngineContext, init_lua_engine_api};
        let interpreter = crate::Interpreter::new(Default::default());
        let lua = interpreter.lua();
        let mut ctx = EngineContext::new(Box::new(crate::lua_engine::DefaultEngineCallbacks));
        ctx.file_reader = Some(Arc::new(|path| match path {
            "system/init.lua" => Ok(format!("function startup()\n{BLOCK}\ncontinued=true\nend").into_bytes()),
            "system/extend/auth.lua" => Ok(b"-- empty file".to_vec()),
            _ => Err(crate::Error::ScriptNotFound(path.into())),
        }));
        init_lua_engine_api(lua, Arc::new(Mutex::new(ctx))).unwrap();
        lua.load("e=__engine; function isFile(_) return true end; e:include('system/init.lua'); startup()")
            .exec().unwrap();
        assert!(lua.globals().get::<bool>("continued").unwrap());
    }

    fn run(module: &str, exists: bool) -> mlua::Result<Lua> {
        let lua = Lua::new();
        lua.globals().set("exists", exists)?;
        lua.globals().set("module", module)?;
        lua.load(r#"
            calls=0; notices=0
            function isFile(_) return exists end
            e={include=function(_, _) assert(loadstring(module))() end,
               debug=function(_, _) notices=notices+1 end}
        "#).exec()?;
        let source = format!("function startup()\n{BLOCK}\ncontinued=true\nend\nstartup()");
        lua.load(repair_source("system/init.lua", source.as_bytes()).unwrap()).exec()?;
        Ok(lua)
    }

    #[test]
    fn absent_and_empty_optional_modules_continue_without_defining_globals() {
        for (module, exists, notices) in [("", false, 0), ("", true, 1), ("-- empty file", true, 1)] {
            let lua = run(module, exists).unwrap();
            assert!(lua.globals().get::<bool>("continued").unwrap());
            assert_eq!(lua.globals().get::<i32>("notices").unwrap(), notices);
            assert!(matches!(lua.globals().get::<Value>("authentication").unwrap(), Value::Nil));
        }
    }

    #[test]
    fn existing_hook_runs_and_errors_are_preserved() {
        let lua = run("function authentication() calls=calls+1 end", true).unwrap();
        assert_eq!(lua.globals().get::<i32>("calls").unwrap(), 1);
        assert_eq!(lua.globals().get::<i32>("notices").unwrap(), 0);
        assert!(run("function authentication() error('hook failure') end", true).unwrap_err().to_string().contains("hook failure"));
        assert!(run("authentication=42", true).is_err());
        assert!(run("local =", true).is_err());
    }

    #[test]
    fn only_exact_recognized_source_is_changed() {
        assert!(repair_source("other.lua", BLOCK.as_bytes()).is_none());
        assert!(repair_source("system/init.lua", b"authentication()").is_none());
        assert!(repair_source("system/init.lua", b"\x1bLua\xff").is_none());
        let crlf = BLOCK.replace('\n', "\r\n");
        let fixed = String::from_utf8(repair_source("system\\init.lua", crlf.as_bytes()).unwrap()).unwrap();
        assert!(fixed.contains("\r\n"));
        assert!(repair_source("system/init.lua", fixed.as_bytes()).is_none());
    }
}
