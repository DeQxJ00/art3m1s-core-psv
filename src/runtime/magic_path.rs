//! Artemis magic path resolution for runtime asset requests.
//!
//! Scripts use paths such as `:bg/room` and populate the prefix table through
//! `e:setMagicPath`. Runtime resolves those prefixes before asking the host to
//! load assets, while physical I/O stays in the host.

use std::collections::HashMap;
use std::sync::Mutex;

pub type MagicPathTable = Mutex<HashMap<String, String>>;

/// Resolve `:name/rest` through the magic-path table.
///
/// Names without a `:` prefix are returned unchanged. If a prefix is not
/// registered, Artemis image assets conventionally fall back to `image/rest`.
pub fn resolve_path(table: &MagicPathTable, name: &str) -> String {
    if !name.starts_with(':') { return name.to_owned(); }
    resolve_with_map(&table.lock().unwrap(), name)
}

fn resolve_with_map(map: &HashMap<String, String>, name: &str) -> String {
    if let Some(rest) = name.strip_prefix(':') {
        // Artemis accepts either separator after the magic word. Keep plain
        // paths untouched; only normalize the expanded virtual path here.
        let rest = rest.replace('\\', "/");
        let (ns, tail) = rest.split_once('/').unwrap_or((&rest, ""));
        if let Some(prefix) = map.get(ns) {
            let prefix = prefix.replace('\\', "/");
            if tail.is_empty() {
                return prefix;
            }
            return format!("{}/{tail}", prefix.trim_end_matches('/'));
        }
        return format!("image/{rest}");
    }
    name.to_string()
}

/// Exact dependency cache: animated transforms do not change resource paths.
/// Compare the whole prefix table as scripts may replace language/platform roots.
#[derive(Default)]
pub(super) struct ScenePaths {
    files: std::collections::HashSet<String>,
    prefixes: HashMap<String, String>,
    resolved: Vec<String>,
}
impl ScenePaths {
    pub fn resolve<'a>(&'a mut self, table: &MagicPathTable, files: &std::collections::HashSet<String>) -> &'a [String] {
        let map=table.lock().unwrap();
        if self.files != *files || self.prefixes != *map {
            self.resolved=files.iter().map(|name|resolve_with_map(&map,name)).collect();
            self.resolved.sort_unstable();
            self.files.clone_from(files);self.prefixes.clone_from(&map);
        }
        &self.resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_paths_reuse_and_invalidate_exact_dependencies() {
        let table=MagicPathTable::new(HashMap::from([("ui".into(),"pc/a".into())]));
        let mut cache=ScenePaths::default();
        let mut files=std::collections::HashSet::from([":ui/face.png".into(),"plain.png".into()]);
        assert_eq!(cache.resolve(&table,&files), &["pc/a/face.png", "plain.png"]);
        let pointer=cache.resolved.as_ptr();cache.resolve(&table,&files);
        assert_eq!(pointer,cache.resolved.as_ptr());
        table.lock().unwrap().insert("ui".into(),"pc/b".into());
        assert_eq!(cache.resolve(&table,&files), &["pc/b/face.png", "plain.png"]);
        files.remove("plain.png");files.insert(":ui/new.png".into());
        assert_eq!(cache.resolve(&table,&files), &["pc/b/face.png", "pc/b/new.png"]);
        files.clear();assert!(cache.resolve(&table,&files).is_empty());
    }

    #[test]
    fn resolves_registered_prefix() {
        let table = MagicPathTable::new(HashMap::from([("mv".to_string(), "movie".to_string())]));

        assert_eq!(resolve_path(&table, ":mv/opening"), "movie/opening");
    }

    #[test]
    fn falls_back_for_unknown_prefix() {
        let table = MagicPathTable::new(HashMap::new());

        assert_eq!(resolve_path(&table, ":bg/title"), "image/bg/title");
    }

    #[test]
    fn keeps_plain_paths_unchanged() {
        let table = MagicPathTable::new(HashMap::new());

        assert_eq!(resolve_path(&table, "voice/line001"), "voice/line001");
    }

    #[test]
    fn resolves_either_separator_without_doubling_the_join() {
        let table = MagicPathTable::new(HashMap::from([
            ("bg".into(), "_data\\image\\background\\".into()),
            ("title".into(), "_data/image/title.png".into()),
            ("root".into(), "/".into()),
        ]));
        assert_eq!(
            resolve_path(&table, r":bg\day\room"),
            "_data/image/background/day/room"
        );
        assert_eq!(
            resolve_path(&table, ":bg/day/room"),
            "_data/image/background/day/room"
        );
        assert_eq!(resolve_path(&table, ":title"), "_data/image/title.png");
        assert_eq!(resolve_path(&table, ":root/image.png"), "/image.png");
        assert_eq!(resolve_path(&table, r"plain\file.png"), r"plain\file.png");
    }
}
