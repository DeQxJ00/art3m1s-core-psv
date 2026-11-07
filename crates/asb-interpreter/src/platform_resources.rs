//! Keep generated platform tables tied to their source asset columns.
use mlua::{Lua, Table, Value};
fn platform(path: &str) -> Option<&str> {
    let name=path.strip_prefix("system/csv_")?.strip_suffix(".tbl")?;
    ["windows","vita","android","ios","switch","ps4"].contains(&name).then_some(name)
}
pub(crate) fn apply(lua: &Lua, path: &str, source: &[u8]) -> mlua::Result<usize> {
    let path=path.replace('\\', "/").to_ascii_lowercase();
    let Some(target)=platform(&path) else { return Ok(0); };
    let Ok(source)=std::str::from_utf8(source) else { return Ok(0); };
    let Some(origin)=source.lines().next().and_then(|s|s.strip_prefix("-- art3m1s platform-table fallback: ")).and_then(platform) else { return Ok(0); };
    if origin==target { return Ok(0); }
    let globals=lua.globals();
    let (Ok(indices),Ok(init))=(globals.raw_get::<Table>("os_no"),globals.raw_get::<Table>("init")) else { return Ok(0); };
    let (Ok(from),Ok(to))=(indices.raw_get::<i64>(origin),indices.raw_get::<i64>(target)) else { return Ok(0); };
    if !(1..=16).contains(&from) || !(1..=16).contains(&to) { return Ok(0); }
    // These are asset selectors, not display dimensions. Keep game_scale and
    // writable cache directories untouched. Native platform tables never enter
    // this path, and missing source columns are not guessed.
    let mut changed=0;
    for key in ["game_size","sound_rpath","sound_ext","rule_ext","movie_ext"] {
        let Ok(column)=init.raw_get::<Table>(key) else { continue; };
        let value=column.raw_get::<Value>(from)?;
        if matches!(value,Value::String(_) | Value::Integer(_) | Value::Number(_)) {
            column.raw_set(to,value)?;
            changed+=1;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    const GENERATED: &[u8] = b"-- art3m1s platform-table fallback: system/csv_windows.tbl\ninit={}\n";
    fn fixture() -> Lua {
        let lua=Lua::new();
        lua.load(r#"
            os_no={windows=1,vita=3,android=2}
            init={game_scale=960,game_size={1280,1280,960},
                  sound_rpath={'ogg/','ogg/','at9/'},sound_ext={'.ogg','.ogg','.at9'},
                  rule_ext={'.png','.png','.gxt'},movie_ext={'.mp4','.mp4','.pam'},
                  os_path={'pc/','pa/','psv/'}}
        "#).exec().unwrap(); lua
    }
    #[test]
    fn generated_table_uses_source_assets_without_changing_stage_or_cache_paths() {
        let lua=fixture();
        assert_eq!(apply(&lua,"system/csv_vita.tbl",GENERATED).unwrap(),5);
        lua.load(r#"
            assert(init.game_size[3]==1280 and init.game_scale==960)
            assert(init.sound_rpath[3]=='ogg/' and init.sound_ext[3]=='.ogg')
            assert(init.rule_ext[3]=='.png' and init.movie_ext[3]=='.mp4')
            assert(init.os_path[3]=='psv/' and init.game_size[2]==1280)
        "#).exec().unwrap();
    }
    #[test]
    fn native_language_unknown_and_sparse_tables_stay_untouched() {
        let lua=fixture();
        for (path,data) in [("system/csv_vita.tbl",b"init={}".as_slice()),
                            ("system/csv_vita_cn.tbl",GENERATED),
                            ("system/csv_other.tbl",GENERATED)] {
            assert_eq!(apply(&lua,path,data).unwrap(),0);
        }
        lua.load("assert(init.game_size[3]==960); init.game_size[1]=nil; init.sound_ext[1]={}; init.sound_rpath=nil; init.rule_ext=nil; init.movie_ext=nil").exec().unwrap();
        assert_eq!(apply(&lua,"system/csv_vita.tbl",GENERATED).unwrap(),0);
        lua.load("assert(init.game_size[3]==960 and init.sound_ext[3]=='.at9')").exec().unwrap();
    }
}
