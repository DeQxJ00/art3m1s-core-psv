//! Optional host hiding of the script-declared touch toolbar.
//! Resolve semantic IDs, not a game's layer numbers or asset filenames.
use super::CoreRuntime;
use crate::compositor::Scene;
use asb_interpreter::Interpreter;

#[derive(Default)]
pub(super) struct ToolbarVisibility {
    pub hidden: bool,
    ids: Vec<String>,
}

impl ToolbarVisibility {
    fn sync(&mut self, it: &Interpreter, scene: &mut Scene) -> bool {
        if !self.hidden && self.ids.is_empty() {
            return false;
        }
        let next = if self.hidden {
            it.query_toolbar_layer_ids()
        } else {
            vec![]
        };
        let mut changed = false;
        for key in self.ids.iter().chain(next.iter()) {
            let hidden = next.contains(key);
            if scene
                .get(key)
                .is_some_and(|layer| layer.host_hidden != hidden)
            {
                // get_mut also invalidates subtree draw and query caches.
                scene.get_mut(key).unwrap().host_hidden = hidden;
                changed = true;
            }
        }
        self.ids = next;
        changed
    }
}

impl CoreRuntime {
    pub fn set_toolbar_hidden(&mut self, hidden: bool) {
        self.toolbar.hidden = hidden;
        self.sync_toolbar_visibility();
    }
    pub(super) fn sync_toolbar_visibility(&mut self) {
        if self
            .toolbar
            .sync(&self.interpreter, &mut self.compositor.scene)
        {
            self.frame_visual_dirty = true;
            self.pointer_hit_test_dirty = true;
            self.layer_info_dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::scene::LayerEventHandler;
    use crate::compositor::{
        Compositor,
        build::{build_frame, resolved_props},
        mock::MockProvider,
    };

    #[test]
    fn toolbar_override_preserves_script_visibility_and_saves() {
        let it = Interpreter::default();
        it.lua()
            .load(
                r#"
            init={mwtabid='ui.touch.strip'}
            btn={name='config',dialogue={id='ui.',p={
              tb_bg={id='touch.strip.background'},tb_mask={id='touch.drag'}}},
              config={id='config.',p={tb_bg={id='background'},tb_mask={id='drag'}}}}
        "#,
            )
            .exec()
            .unwrap();
        let mut scene = Scene::new();
        for key in [
            "ui.touch.strip.button",
            "ui.touch.drag",
            "ui.message",
            "config.drag",
        ] {
            scene.ensure(key);
        }
        let mut v = ToolbarVisibility::default();
        assert!(!v.sync(&it, &mut scene));
        scene.create("ui.touch.strip.button", Some("toolbar".into()));
        scene
            .get_mut("ui.touch.strip.button")
            .unwrap()
            .event_handlers
            .insert(
                "click".into(),
                LayerEventHandler {
                    enabled: true,
                    ..Default::default()
                },
            );
        let mut provider = MockProvider::new();
        assert_eq!(
            build_frame(&scene, 0, &mut provider, None).commands.len(),
            1
        );
        let mut compositor = Compositor::new();
        compositor.scene = scene.clone();
        assert_eq!(
            compositor.hit_test(1.0, 1.0, &mut provider).as_deref(),
            Some("ui.touch.strip.button")
        );
        v.hidden = true;
        assert!(v.sync(&it, &mut scene));
        assert!(!scene.is_effectively_visible("ui.touch.strip.button"));
        assert!(!scene.is_effectively_visible("ui.touch.drag"));
        assert!(scene.is_effectively_visible("ui.message"));
        assert!(scene.is_effectively_visible("config.drag"));
        assert!(
            build_frame(&scene, 0, &mut provider, None)
                .commands
                .is_empty()
        );
        compositor.scene = scene.render_snapshot();
        assert!(
            build_frame(compositor.scene(), 0, &mut provider, None)
                .commands
                .is_empty()
        );
        compositor.scene = scene.clone();
        assert!(compositor.hit_test(1.0, 1.0, &mut provider).is_none());
        assert!(resolved_props(scene.get("ui.touch.strip").unwrap(), 0).is_visible());
        assert!(!v.sync(&it, &mut scene));
        // No host visibility leaks into saves; loading reapplies the setting.
        let bytes = serde_json::to_vec(&scene).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("host_hidden"));
        let mut restored: Scene = serde_json::from_slice(&bytes).unwrap();
        assert!(restored.is_effectively_visible("ui.touch.drag"));
        assert_eq!(
            build_frame(&restored, 0, &mut provider, None)
                .commands
                .len(),
            1
        );
        assert!(v.sync(&it, &mut restored));
        v.hidden = false;
        assert!(v.sync(&it, &mut restored));
        assert!(restored.is_effectively_visible("ui.touch.drag"));
        // No declarations: neither hide arbitrary layers nor create any.
        it.lua().load("init=nil").exec().unwrap();
        v.hidden = true;
        assert!(!v.sync(&it, &mut restored));
    }
}
