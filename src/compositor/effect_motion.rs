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
    // Particle-only motion cannot activate a filter boost. Reject it before
    // evaluating visibility tweens along every image's ancestor chain.
    if !effect(scene.root_props()) && !scene.all_layers().any(|l| effect(&l.props) || l.mask.is_some()) {
        return false;
    }
    // Most held dialogue scenes have no translation tween at all. Avoid
    // repeatedly walking every image's ancestor chain in that common case.
    // Keep the exact time/loop predicate so a delayed tween activates on time.
    if !scene.all_layers().any(|layer| layer.tweens.iter().any(|t| moving(t, now))) {
        return false;
    }
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

/// Dense looping image motion (snow/rain/etc.), independent of model/video
/// rendering. Count visible image branches, not tween parameters or redraws.
pub(crate) fn has_particle_motion(scene: &Scene, now: u64) -> bool {
    if !scene.root_props().is_visible() || scene.root_props().alpha == Some(0) { return false; }
    if !scene.all_layers().any(|l| l.tweens.iter().any(|t| (t.infinite_loop || t.loop_count.is_some()) && moving(t, now))) {
        return false;
    }
    let mut count = 0;
    for leaf in scene.all_layers().filter(|l| l.file.as_deref().is_some_and(|f| !f.is_empty())) {
        let mut looping = false;
        let mut shown = true;
        let mut id = Some(leaf.id.as_str());
        while let Some(current) = id {
            if let Some(layer) = scene.get(current) {
                if layer.host_hidden || !visible(layer, now) { shown = false; break; }
                looping |= layer.tweens.iter().any(|t| (t.infinite_loop || t.loop_count.is_some()) && moving(t, now));
            }
            id = current.rsplit_once('.').map(|(parent, _)| parent);
        }
        if shown && looping {
            count += 1;
            if count >= 32 { return true; }
        }
    }
    false
}

// Image-based reveal scripts use the conventional mask resource directory,
// while explicit layer masks may live anywhere. Do not match asset basenames.
fn mask_resource(path: &str) -> bool {
    path.trim_start_matches(':').split(['/', '\\']).any(|part| part.eq_ignore_ascii_case("mask"))
}

// Some scripts construct an aperture from a scaled ordinary image plus
// repeated opaque-border images. Detect coordinated finite motion rather than
// assuming the shape is stored in a directory named "mask".
fn has_image_aperture_motion(scene: &Scene, now: u64) -> bool {
    if !scene.all_layers().any(|l| l.tweens.iter().any(|t|
        matches!(t.param.as_str(), "zoom"|"xscale"|"yscale"|"width"|"height")
        && !t.infinite_loop && t.loop_count.is_none() && t.from!=t.to
        && t.duration_ms>0 && now>=t.start_ms && !t.is_finished(now))) { return false; }
    let mut branches = Vec::new();
    for leaf in scene.all_layers().filter(|l| l.file.is_some() || l.solid_color.is_some()) {
        let mut axes=0u8;let mut shown=true;let mut id=Some(leaf.id.as_str());
        while let Some(current)=id {
            if let Some(layer)=scene.get(current) {
                if layer.host_hidden || !visible(layer,now) { shown=false;break; }
                for t in &layer.tweens {
                    if t.infinite_loop || t.loop_count.is_some() || !t.from.is_finite() || !t.to.is_finite() || t.from==t.to || t.duration_ms==0 || now<t.start_ms || t.is_finished(now) { continue; }
                    axes |= match t.param.as_str() { "left"|"x"=>1,"top"|"y"=>2,"zoom"|"xscale"|"yscale"|"width"|"height"=>4,_=>0 };
                }
            }
            id=current.rsplit_once('.').map(|(p,_)|p);
        }
        if shown && axes!=0 { branches.push((leaf,axes)); }
    }
    branches.iter().any(|(shape,axes)| {
        if axes&4==0 || shape.file.is_none() { return false; }
        let root=shape.id.split('.').next();
        branches.iter().any(|(border,_)| {
            if border.file==shape.file && border.solid_color==shape.solid_color { return false; }
            let mut count=0;let mut directions=0;
            for (other,motion) in &branches {
                if other.id.split('.').next()==root && other.file==border.file && other.solid_color==border.solid_color && motion&3!=0 {
                    count+=1;directions|=motion&3;
                }
            }
            count>=3 && directions==3
        })
    })
}

pub(crate) fn has_image_mask_motion(scene: &Scene, now: u64) -> bool {
    if !scene.root_props().is_visible() || scene.root_props().alpha == Some(0) { return false; }
    if has_image_aperture_motion(scene, now) { return true; }
    scene.all_layers().filter(|l| l.mask.is_some() || l.props.custom.get("intermediate_render_mask").is_some_and(|s| !s.is_empty()) || l.file.as_deref().is_some_and(mask_resource)).any(|leaf| {
        let mut animated = false;
        let mut id = Some(leaf.id.as_str());
        while let Some(current) = id {
            if let Some(layer) = scene.get(current) {
                if layer.host_hidden || !visible(layer, now) { return false; }
                animated |= layer.tweens.iter().any(|t| {
                    if !matches!(t.param.as_str(), "left"|"top"|"x"|"y"|"zoom"|"xscale"|"yscale"|"rotate"|"alpha"|"width"|"height") { return false; }
                    if !t.from.is_finite() || !t.to.is_finite() || t.from == t.to || t.duration_ms == 0 || now < t.start_ms || t.is_finished(now) { return false; }
                    let cycle = t.duration_ms.saturating_add(t.loop_delay_ms);
                    !(t.infinite_loop || t.loop_count.is_some()) || (now-t.start_ms)%cycle < t.duration_ms
                });
            }
            id = current.rsplit_once('.').map(|(parent, _)| parent);
        }
        animated
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
    #[test] fn ordinary_image_with_moving_borders_is_an_aperture() {
        let mut s=Scene::new();s.set_file("1.8.0",Some(":cg/shape.png".into()));
        s.get_mut("1.8").unwrap().tweens.push(Tween{param:"xscale".into(),..pan()});
        for i in 1..5 {
            let id=format!("1.{i}.0");s.set_file(&id,Some(":bg/fill.png".into()));
            s.get_mut(&format!("1.{i}")).unwrap().tweens.push(Tween{param:if i<3{"left"}else{"top"}.into(),..pan()});
        }
        assert!(!has_image_mask_motion(&s,99));assert!(has_image_mask_motion(&s,200));
        assert!(!has_image_mask_motion(&s,1100));
        s.get_mut("1.8").unwrap().host_hidden=true;assert!(!has_image_mask_motion(&s,200));
        s.get_mut("1.8").unwrap().host_hidden=false;
        for i in 1..5 { s.get_mut(&format!("1.{i}")).unwrap().tweens[0].infinite_loop=true; }
        assert!(!has_image_mask_motion(&s,200));
    }
    #[test] fn image_mask_activity_tracks_parent_motion_and_release() {
        let mut s=Scene::new();s.set_file("1.0",Some(":image/mask/reveal.png".into()));
        s.get_mut("1").unwrap().tweens.push(Tween{param:"zoom".into(),..pan()});
        assert!(!has_image_mask_motion(&s,99));assert!(has_image_mask_motion(&s,100));
        assert!(!has_image_mask_motion(&s,1100));
        s.get_mut("1").unwrap().host_hidden=true;assert!(!has_image_mask_motion(&s,200));
        s.get_mut("1").unwrap().host_hidden=false;
        s.set_file("1.0",Some("image/background.png".into()));assert!(!has_image_mask_motion(&s,200));
        s.get_mut("1.0").unwrap().mask=Some("custom/reveal.png".into());assert!(has_image_mask_motion(&s,200));
        s.get_mut("1").unwrap().tweens[0].infinite_loop=true;
        s.get_mut("1").unwrap().tweens[0].loop_delay_ms=500;
        assert!(!has_image_mask_motion(&s,1200));assert!(has_image_mask_motion(&s,1600));
        s.get_mut("1.0").unwrap().props.alpha=Some(0);assert!(!has_image_mask_motion(&s,1600));
    }
    #[test] fn dense_looping_images_clock_policy() {
        let mut s=Scene::new();
        for i in 0..31 { s.set_file(&format!("1.{i}"),Some("particle.png".into())); }
        s.get_mut("1").unwrap().tweens.push(Tween{infinite_loop:true,loop_delay_ms:500,..pan()});
        assert!(!has_particle_motion(&s,200));
        s.set_file("1.31",Some("particle.png".into()));
        assert!(!has_particle_motion(&s,99));
        assert!(has_particle_motion(&s,200));
        assert!(!has_particle_motion(&s,1200));
        assert!(has_particle_motion(&s,1600));
        s.get_mut("1").unwrap().host_hidden=true;assert!(!has_particle_motion(&s,200));
        s.get_mut("1").unwrap().host_hidden=false;
        s.get_mut("1").unwrap().props.alpha=Some(0);assert!(!has_particle_motion(&s,200));
        s.get_mut("1").unwrap().props.alpha=None;
        s.get_mut("1").unwrap().tweens[0].infinite_loop=false;
        assert!(!has_particle_motion(&s,200));
        s.get_mut("1").unwrap().tweens[0].loop_count=Some(2);
        assert!(has_particle_motion(&s,200));
        s.get_mut("1").unwrap().props.visible=Some(false);
        assert!(!has_particle_motion(&s,200));
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
