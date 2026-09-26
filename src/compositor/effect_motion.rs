//! Read-only activity query for hosts that temporarily boost effect panning.
//! Inspect live tweens, not cache misses or redraws: a static filtered image
//! must release the boost even when its cached draw list is submitted again.
use super::{Layer, LayerProps, Scene, Tween};

fn moving(t: &Tween, now: u64) -> bool {
    if !matches!(t.param.as_str(), "left" | "top" | "x" | "y")
        || !t.from.is_finite() || !t.to.is_finite() || t.from == t.to
        || t.duration_ms == 0 || now < t.start_ms || t.is_finished(now)
    { return false; }
    // Do not hold the boost during a delayed loop's stationary interval.
    let cycle = t.duration_ms.saturating_add(t.loop_delay_ms);
    !(t.infinite_loop || t.loop_count.is_some()) || (now-t.start_ms)%cycle < t.duration_ms
}

fn effect(p: &LayerProps) -> bool {
    p.shader.as_deref().is_some_and(|s| !s.trim().is_empty())
        || p.grayscale == Some(true) || p.negative == Some(true)
        || p.color_multiply.is_some_and(|c| c != [1., 1., 1.])
}

fn visible(layer: &Layer, now: u64) -> bool {
    let mut shown = layer.props.is_visible();
    let mut alpha = layer.props.alpha.unwrap_or(255) as f32;
    for t in &layer.tweens {
        match t.param.as_str() {
            "visible" => shown = t.value_at(now).round() != 0.,
            "alpha" => alpha = t.value_at(now),
            _ => {}
        }
    }
    shown && alpha > 0.
}

pub(crate) fn has_effect_pan(scene: &Scene, now: u64) -> bool {
    if !scene.root_props().is_visible() || scene.root_props().alpha == Some(0) { return false; }
    // Start at actual images and walk their ancestors. This covers both a
    // moving child under a filter group and a filtered child under a moving
    // parent, without pairing an unrelated UI tween with a background filter.
    scene.all_layers().filter(|l| l.file.as_deref().is_some_and(|f| !f.is_empty()) || l.solid_color.is_some()).any(|leaf| {
        let mut pan = false;
        let mut filtered = effect(scene.root_props());
        let mut id = Some(leaf.id.as_str());
        while let Some(current) = id {
            if let Some(layer) = scene.get(current) {
                if !visible(layer, now) { return false; }
                pan |= layer.tweens.iter().any(|t| moving(t, now));
                filtered |= effect(&layer.props) || layer.mask.is_some();
            }
            id = current.rsplit_once('.').map(|(parent, _)| parent);
        }
        pan && filtered
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::Easing;
    fn pan() -> Tween {
        Tween { param:"top".into(),from:0.,to:200.,easing:Easing::Linear,
            start_ms:100,duration_ms:1000,infinite_loop:false,loop_count:None,
            yoyo:false,yoyo_reverse:false,loop_delay_ms:0,delete_on_finish:false,handler:None,set_id:None }
    }
    fn scene() -> Scene {
        let mut s=Scene::new();s.set_file("1.0",Some("image.png".into()));
        s.get_mut("1").unwrap().props.shader=Some("blur".into());
        s.get_mut("1.0").unwrap().tweens.push(pan());s
    }
    #[test] fn active_interval_and_visibility() {
        let mut s=scene();
        assert!(!has_effect_pan(&s,99));assert!(has_effect_pan(&s,100));
        assert!(has_effect_pan(&s,1099));assert!(!has_effect_pan(&s,1100));
        s.get_mut("1").unwrap().props.visible=Some(false);assert!(!has_effect_pan(&s,200));
        s.get_mut("1").unwrap().props.visible=None;
        s.get_mut("1.0").unwrap().props.alpha=Some(0);assert!(!has_effect_pan(&s,200));
    }
    #[test] fn unrelated_motion_and_static_filters_do_not_trigger() {
        let mut s=scene();s.get_mut("1.0").unwrap().tweens.clear();
        s.set_file("2",Some("button.png".into()));s.get_mut("2").unwrap().tweens.push(pan());
        assert!(!has_effect_pan(&s,200));
        s.get_mut("1.0").unwrap().tweens.push(Tween{param:"alpha".into(),..pan()});
        assert!(!has_effect_pan(&s,200));
        s.get_mut("1.0").unwrap().tweens=vec![Tween{to:0.,..pan()}];
        assert!(!has_effect_pan(&s,200));
    }
    #[test] fn moving_parent_and_loop_delay() {
        let mut s=scene();s.get_mut("1.0").unwrap().tweens.clear();
        s.get_mut("1").unwrap().props.shader=None;
        s.get_mut("1.0").unwrap().props.grayscale=Some(true);
        s.get_mut("1").unwrap().tweens.push(Tween{infinite_loop:true,loop_delay_ms:500,..pan()});
        assert!(has_effect_pan(&s,200));assert!(!has_effect_pan(&s,1200));
        assert!(has_effect_pan(&s,1600));
        s.get_mut("1").unwrap().tweens.clear();assert!(!has_effect_pan(&s,1700));
    }
}
