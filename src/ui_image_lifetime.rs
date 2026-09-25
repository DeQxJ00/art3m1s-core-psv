//! Standard Artemis menu artwork is loaded on demand, not pinned
//! by the startup system-cache list. Unknown layouts keep their old policy.
pub(crate) fn transient_menu_image(path: &str) -> bool {
    let mut parts = path.split(['/', '\\']);
    let Some(root) = parts.next() else { return false; };
    let section = if root.eq_ignore_ascii_case(":ui") || root.eq_ignore_ascii_case("ui") {
        parts.next()
    } else if ["pc", "vita", "psv", "windows", "android"].iter().any(|p| root.eq_ignore_ascii_case(p)) {
        // Platform/language/section/file (the language is supplied by the game).
        parts.next();
        parts.next()
    } else { return false; };
    section.is_some_and(|s| ["conf", "config", "blog", "backlog", "save", "load", "saveload"].iter().any(|p| s.eq_ignore_ascii_case(p)))
        && parts.next().is_some_and(|s| !s.is_empty())
}

// The runtime supplies the actual per-game save directory; do not guess slot
// names or assume every game uses "save". This only classifies texture memory.
pub(crate) fn image_in_save_directory(path: &str, directory: &str) -> bool {
    if directory.is_empty() { return false; }
    let mut parts=path.split(['/', '\\']);
    for prefix in directory.trim_end_matches(['/', '\\']).split(['/', '\\']) {
        if !parts.next().is_some_and(|p|p.eq_ignore_ascii_case(prefix)){return false;}
    }
    parts.next().is_some_and(|p|!p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_transient_menu_artwork_in_known_ui_layouts() {
        for p in [":ui/conf/bg.png", ":ui/blog/mask", "pc/cn/conf/bg01.png", "vita/ja/blog/bt_up.png", r"PC\en\CONFIG\page\bg.png", "ui/backlog/bg", ":ui/save/bg_load.png", "pc/cn/save/bg_save.png", "vita/ja/load/slot.png", "ui/saveload/mask"] {
            assert!(transient_menu_image(p), "{p}");
        }
        for p in [":ui/mw/bt_blog.png", ":ui/title/bt_config.png", ":ui/mw/bt_save.png", ":ui/title/bt_load.png", "image/bg/conf/room.png", "image/fg/blog/body.png", "custom/conf/bg.png", "pc/cn/conf", ":ui/conf/"] {
            assert!(!transient_menu_image(p), "{p}");
        }
    }
    #[test]
    fn save_directory_matching_respects_components_and_custom_paths() {
        for p in ["profile/data/slot01.png", r"PROFILE\data\slot01", "profile/data/slot01\u{1f}mask\u{1f}:ui/save/mask"] {
            assert!(image_in_save_directory(p,"profile/data"));
        }
        for p in ["profile/database/slot01.png", "profile/data", "profile/data/", "image/bg/slot01.png"] {
            assert!(!image_in_save_directory(p,"profile/data"));
        }
        assert!(!image_in_save_directory("anything.png",""));
    }
}
