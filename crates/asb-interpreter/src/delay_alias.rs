//! Preserve the framework's original-name fallback for optional voice aliases.
const LOOKUP: &str = "local d = csv.voice.name[c]\t\t-- mobキャラdelay変換(csv)確認";
const FALLBACK: &str = "local c = d.exch or c\t\t\t-- mobキャラdelay変換(csv)があれば優先、無ければスクリプトのキャラ名を使う";

pub(crate) fn repair_source(path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    if path.replace('\\', "/").to_ascii_lowercase() != "system/adv/delay.lua" { return None; }
    let source = std::str::from_utf8(bytes).ok()?;
    if !source.contains(LOOKUP) || source.matches(FALLBACK).count() != 1 { return None; }
    Some(source.replacen(FALLBACK, &FALLBACK.replace("d.exch or c", "(d and d.exch) or c"), 1).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn missing_alias_preserves_original_name_and_executes_delayed_action() {
        for (entry, expected) in [("nil", "source"), ("{}", "source"), ("{exch='mapped'}", "mapped")] {
            let lua = Lua::new();
            lua.load("csv={voice={name={}}}; acted=0").exec().unwrap();
            lua.load(format!("csv.voice.name.source={entry}")).exec().unwrap();
            let source = format!("local c='source'\n{LOOKUP}\n{FALLBACK}\nresolved=c; acted=acted+1");
            let fixed=repair_source("system/adv/delay.lua", source.as_bytes()).unwrap();
            lua.load(fixed).exec().unwrap();
            assert_eq!(lua.globals().get::<String>("resolved").unwrap(), expected);
            assert_eq!(lua.globals().get::<i32>("acted").unwrap(), 1);
        }
    }

    #[test]
    fn unrelated_sources_and_already_guarded_sources_are_unchanged() {
        let source = format!("{LOOKUP}\r\n{FALLBACK}");
        assert!(repair_source("other.lua", source.as_bytes()).is_none());
        assert!(repair_source("system/adv/delay.lua", b"local c=d.exch or c").is_none());
        let fixed = repair_source("system/adv/delay.lua", source.as_bytes()).unwrap();
        assert!(String::from_utf8_lossy(&fixed).contains("\r\n"));
        assert!(repair_source("system/adv/delay.lua", &fixed).is_none());
    }
}
