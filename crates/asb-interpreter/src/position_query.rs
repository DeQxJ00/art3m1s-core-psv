//! Scoped queries for a completely audited position-calculation Lua block.

const QUERY: &str = "e:tag({\"var\",name=\"t.l.li\",system=\"get_layer_info\",style=\"map\",writelocal=\"1\"});";

fn targeted_source(code: &str) -> Option<String> {
    let body = code.strip_prefix("function getPosCalc(e)")?;
    if body.matches(QUERY).count() != 1 { return None; }
    let query = "__scoped_query(e,\"t.l.li\",i,n,i..\".0.0.1\",i..\".0.0.0\");";
    Some(format!("return function(e,__scoped_query){}", body.replacen(QUERY, query, 1)))
}

pub(crate) fn after_block(lua: &mlua::Lua, code: &str) -> mlua::Result<()> {
    let hash = code.bytes().fold(0xcbf29ce484222325_u64, |h, b|
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    if (code.len(), hash) != (2449, 0x4b7daaf2ad064665) { return Ok(()); }
    let Some(source) = targeted_source(code) else { return Ok(()); };
    install(lua, &source)
}

fn install(lua: &mlua::Lua, source: &str) -> mlua::Result<()> {
    let original: mlua::Function = lua.globals().get("getPosCalc")?;
    let optimized: mlua::Function = lua.load(source).set_name("scoped-position-query").eval()?;
    let query = lua.create_function(|_, (engine, prefix, root, image, transform, alternate):
        (mlua::AnyUserData, String, String, String, String, String)| {
        let api = engine.borrow::<crate::lua_engine::EngineApi>()?;
        let ctx = api.ctx.lock().unwrap();
        if let Some(vars) = &ctx.variables {
            let mut vars = vars.lock().unwrap();
            vars.with_local_writes(true, |vars| {
                for id in [root, image, transform, alternate] {
                    // An absent layer contributes no fields to the original map.
                    // Do not use the single-tag fallback's placeholder dimensions.
                    if let Some(info) = ctx.callbacks.get_layer_info(&id) {
                        for (key, value) in info {
                            crate::lua_engine::set_var_auto(vars, &format!("{prefix}.{id}.{key}"), value);
                        }
                    }
                }
            });
        }
        Ok(())
    })?;
    let reports = std::sync::atomic::AtomicUsize::new(0);
    let wrapper = lua.create_function(move |_, args: mlua::MultiValue| {
        let enabled = match args.front() {
            Some(mlua::Value::UserData(engine)) => engine.borrow::<crate::lua_engine::EngineApi>().ok()
                .is_some_and(|api| {
                    let ctx = api.ctx.lock().unwrap();
                    ctx.callbacks.scoped_layer_query_enabled() && !ctx.callbacks.tag_notifications_enabled()
                }),
            _ => false,
        };
        if !enabled { return original.call::<mlua::MultiValue>(args); }
        let engine = args.front().cloned();
        let started = std::time::Instant::now();
        let result = optimized.call::<mlua::MultiValue>((engine.clone(), query.clone()));
        let elapsed = started.elapsed().as_micros();
        let sample = reports.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if sample < 8 || (sample < 32 && elapsed >= 5_000) {
            if let Some(mlua::Value::UserData(engine)) = engine {
                let api = engine.borrow::<crate::lua_engine::EngineApi>()?;
                api.ctx.lock().unwrap().callbacks.debug(0,
                    &format!("[position-query] scoped_ids=4 elapsed_us={elapsed} ok={}", result.is_ok()), false);
            }
        }
        result
    })?;
    lua.globals().set("getPosCalc", wrapper)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unopted_host_keeps_original_arguments_and_return_values() {
        let it = crate::Interpreter::new(crate::InterpreterConfig::default());
        it.lua().load("function getPosCalc(e,a,b) return a,b end").exec().unwrap();
        install(it.lua(), "return function(e,q) error('must not specialize this host') end").unwrap();
        let values: (String, i32) = it.lua().load("return getPosCalc(__engine,'original',17)").eval().unwrap();
        assert_eq!(values, ("original".into(), 17));
    }

    #[test]
    fn matching_function_name_does_not_enable_unrecognized_code() {
        let lua = mlua::Lua::new();
        let code = format!("function getPosCalc(e)local i='x';local n='y';{QUERY}end");
        lua.load(&code).exec().unwrap();
        let original = lua.globals().get::<mlua::Function>("getPosCalc").unwrap();
        after_block(&lua, &code).unwrap();
        let after = lua.globals().get::<mlua::Function>("getPosCalc").unwrap();
        assert_eq!(original.to_pointer(), after.to_pointer());
    }

    #[test]
    fn scoped_query_selects_only_the_four_required_nodes() {
        let source = format!("function getPosCalc(e)local i='10.1';local n=i..'.0.0.1.a.1';{QUERY}end");
        let optimized = targeted_source(&source).expect("one map query is replaceable");
        let lua = mlua::Lua::new();
        lua.load("seen={}; engine={}; query=function(e,prefix,...) assert(e==engine); assert(prefix=='t.l.li'); seen={...} end").exec().unwrap();
        let function: mlua::Function = lua.load(&optimized).eval().unwrap();
        function.call::<()>((lua.globals().get::<mlua::Table>("engine").unwrap(), lua.globals().get::<mlua::Function>("query").unwrap())).unwrap();
        let seen: Vec<String> = lua.globals().get::<mlua::Table>("seen").unwrap().sequence_values().collect::<mlua::Result<_>>().unwrap();
        assert_eq!(seen, ["10.1", "10.1.0.0.1.a.1", "10.1.0.0.1", "10.1.0.0.0"]);
        assert!(targeted_source("function unrelated() end").is_none());
        assert!(targeted_source(&format!("function getPosCalc(e){QUERY}{QUERY}end")).is_none());
    }
}
