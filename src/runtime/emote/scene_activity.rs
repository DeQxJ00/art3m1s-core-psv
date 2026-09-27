use crate::compositor::Scene;

pub(super) fn has_visible_emote<'a>(
    scene: &Scene,
    now: u64,
    ids: impl IntoIterator<Item = &'a str>,
) -> bool {
    if !scene.root_props().is_visible() || scene.root_props().alpha == Some(0) {
        return false;
    }
    ids.into_iter().any(|id| {
        if scene.get(id).is_none() { return false; }
        let mut path = Some(id);
        while let Some(current) = path {
            if let Some(layer) = scene.get(current) {
                if layer.host_hidden { return false; }
                let mut visible = layer.props.is_visible();
                let mut alpha = layer.props.alpha.unwrap_or(255) as f32;
                for tween in &layer.tweens {
                    match tween.param.as_str() {
                        "visible" => visible = tween.value_at(now).round() != 0.,
                        "alpha" => alpha = tween.value_at(now),
                        _ => {}
                    }
                }
                if !visible || alpha <= 0. { return false; }
            }
            path = current.rsplit_once('.').map(|(parent, _)| parent);
        }
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::{Easing, Tween};

    #[test]
    fn cached_models_need_visible_scene_layers() {
        let mut scene = Scene::new();
        assert!(!has_visible_emote(&scene, 0, ["10.0"]));
        scene.ensure("10.0");
        assert!(!has_visible_emote(&scene, 0, []));
        assert!(has_visible_emote(&scene, 0, ["10.0"]));
        scene.get_mut("10").unwrap().props.visible = Some(false);
        assert!(!has_visible_emote(&scene, 0, ["10.0"]));
        scene.ensure("20");
        assert!(has_visible_emote(&scene, 0, ["10.0", "20"]));
        scene.get_mut("20").unwrap().host_hidden = true;
        assert!(!has_visible_emote(&scene, 0, ["10.0", "20"]));
    }

    #[test]
    fn alpha_and_visible_tweens_use_the_scene_clock() {
        let mut scene = Scene::new();
        scene.ensure("10.0");
        scene.get_mut("10").unwrap().tweens.push(Tween {
            param: "alpha".into(), from: 255., to: 0., start_ms: 0,
            duration_ms: 100, easing: Easing::Linear, infinite_loop: false,
            loop_count: None, yoyo: false, yoyo_reverse: false,
            loop_delay_ms: 0, delete_on_finish: false, handler: None, set_id: None,
        });
        assert!(has_visible_emote(&scene, 50, ["10.0"]));
        assert!(!has_visible_emote(&scene, 100, ["10.0"]));
        let layer = scene.get_mut("10").unwrap();
        layer.tweens[0].param = "visible".into();
        layer.tweens[0].from = 0.; layer.tweens[0].to = 1.;
        assert!(!has_visible_emote(&scene, 0, ["10.0"]));
        assert!(has_visible_emote(&scene, 100, ["10.0"]));
        scene.set_root_props(&[("alpha".into(), "0".into())].into());
        assert!(!has_visible_emote(&scene, 100, ["10.0"]));
    }
}
