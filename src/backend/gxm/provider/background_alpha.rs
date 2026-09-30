//! Per-project opt-in for ordinary BG source images. Never classify generated
//! composites, UI/foreground paths, masks, or model textures by a substring.
pub(super) fn background_source(name: &str) -> bool {
    if name.chars().any(char::is_control) { return false; }
    let mut parts=name.split(['/', '\\']);
    let Some(first)=parts.next() else { return false; };
    let is_bg=first.eq_ignore_ascii_case(":bg") || first.eq_ignore_ascii_case("bg")
        || (first.eq_ignore_ascii_case("image")
            && parts.next().is_some_and(|p|p.eq_ignore_ascii_case("bg")));
    is_bg && parts.clone().any(|p|!p.is_empty())
        && parts.all(|p|p!=".." && p!=".")
}

pub(super) fn make_opaque(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) { pixel[3]=255; }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn only_explicit_background_sources() {
        for name in [":bg/room", "bg/room.png", "image/bg/day/room", r"IMAGE\BG\room.png"] {
            assert!(background_source(name),"{name}");
        }
        for name in [":bg", "bg/", "image/bg", ":fg/bg", "image/fg/bg.png", ":ui/bg/room",
            "ui/bg/room", "mask/bg/rule", "emote/bg/atlas", "image/bgx/room", "image/bg/../fg/face",
            "image/bg/room\u{1f}mask\u{1f}mask/iris"] {
            assert!(!background_source(name),"{name}");
        }
    }
}
