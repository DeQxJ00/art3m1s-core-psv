//! CPU-only fragments for static image subtrees; text/content stay live.
use super::scene::Scene;
use crate::render_pipeline::draw::{DrawList, ShaderEffect};
use glam::Affine2;
use std::collections::{HashMap, HashSet};

#[derive(Clone, PartialEq)]
pub(super) struct ParentState {
    pub transform: Affine2,
    pub opacity: f32,
    pub clip: Option<[f32; 4]>,
    pub shader: Option<ShaderEffect>,
    pub keys: bool,
    pub crop_allowed: bool,
}
struct Stamp {
    id: String,
    revision: u64,
    edited: Option<String>,
}
struct Entry {
    parent: ParentState,
    nodes: Vec<Stamp>,
    frame: DrawList,
    seen: bool,
}
#[derive(Default)]
pub(crate) struct SceneBuildCache {
    entries: HashMap<String, Entry>,
    dynamic: HashSet<String>,
    revision: Option<u64>,
    pub hits: u64,
    pub builds: u64,
    frames: u64,
}
impl SceneBuildCache {
    pub fn begin(&mut self, revision: u64, retry: bool, dynamic: impl Iterator<Item = String>) {
        if retry || self.revision != Some(revision) {
            self.entries.clear();
        }
        self.revision = Some(revision);
        self.dynamic.clear();
        self.dynamic.extend(dynamic);
        for entry in self.entries.values_mut() {
            entry.seen = false;
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.revision = None;
    }
    pub fn finish(&mut self) {
        self.entries.retain(|_, entry| entry.seen);
        self.frames += 1;
    }
    pub fn sample(&self) -> Option<(u64, u64, usize)> {
        (self.frames % 300 == 0).then_some((self.hits, self.builds, self.entries.len()))
    }
    pub(super) fn replay(
        &mut self,
        scene: &Scene,
        id: &str,
        parent: &ParentState,
        edits: Option<&HashMap<String, String>>,
        out: &mut DrawList,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(id) else {
            return false;
        };
        if &entry.parent != parent
            || entry.nodes.iter().any(|s| {
                self.dynamic.contains(&s.id)
                    || scene
                        .get(&s.id)
                        .is_none_or(|l| l.query_revision != s.revision)
                    || edits.and_then(|e| e.get(&s.id)) != s.edited.as_ref()
            })
        {
            return false;
        }
        let offset = out.commands.len();
        let mask_offset = out.mask_commands.len();
        out.commands.extend_from_slice(&entry.frame.commands);
        out.command_keys
            .extend_from_slice(&entry.frame.command_keys);
        out.mask_commands
            .extend_from_slice(&entry.frame.mask_commands);
        out.shader_groups
            .extend(entry.frame.shader_groups.iter().cloned().map(|mut g| {
                g.start += offset;
                g.end += offset;
                if let Some(r) = &mut g.mask_range {
                    r[0] += mask_offset;
                    r[1] += mask_offset;
                }
                g
            }));
        entry.seen = true;
        self.hits += 1;
        true
    }
    pub(super) fn candidate(&self, scene: &Scene, id: &str) -> bool {
        let Some(layer) = scene.get(id) else {
            return false;
        };
        // Cross-layer shader resource references and live tweens are evaluated
        // normally. Their static children can still use independent fragments.
        !self.dynamic.contains(id)
            && layer.tweens.is_empty()
            && layer.props.shader.as_deref().is_none_or(str::is_empty)
            && layer.children.iter().all(|id| self.candidate(scene, id))
    }
    pub(super) fn store(
        &mut self,
        scene: &Scene,
        id: &str,
        parent: ParentState,
        edits: Option<&HashMap<String, String>>,
        frame: &DrawList,
        start: [usize; 3],
    ) {
        let count = frame.commands.len() - start[0] + frame.mask_commands.len() - start[1];
        // Bound extra CPU command storage, never retain extra GPU textures.
        self.entries.remove(id);
        let used: usize = self
            .entries
            .values()
            .map(|e| e.frame.commands.len() + e.frame.mask_commands.len())
            .sum();
        if count == 0 || used + count > 1024 || self.entries.len() >= 128 {
            return;
        }
        let mut nodes = Vec::new();
        fn collect(
            scene: &Scene,
            id: &str,
            edits: Option<&HashMap<String, String>>,
            out: &mut Vec<Stamp>,
        ) {
            if let Some(l) = scene.get(id) {
                out.push(Stamp {
                    id: id.into(),
                    revision: l.query_revision,
                    edited: edits.and_then(|e| e.get(id)).cloned(),
                });
                for c in &l.children {
                    collect(scene, c, edits, out);
                }
            }
        }
        collect(scene, id, edits, &mut nodes);
        let mut fragment = DrawList {
            commands: frame.commands[start[0]..].to_vec(),
            command_keys: frame.command_keys[start[0]..].to_vec(),
            mask_commands: frame.mask_commands[start[1]..].to_vec(),
            shader_groups: frame.shader_groups[start[2]..].to_vec(),
        };
        for g in &mut fragment.shader_groups {
            g.start -= start[0];
            g.end -= start[0];
            if let Some(r) = &mut g.mask_range {
                r[0] -= start[1];
                r[1] -= start[1];
            }
        }
        self.entries.insert(
            id.into(),
            Entry {
                parent,
                nodes,
                frame: fragment,
                seen: true,
            },
        );
        self.builds += 1;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::{build::build_frame_reusing_cached, mock::MockProvider};
    use crate::render_pipeline::draw::{TextureId, TextureInfo, TextureProvider};

    #[derive(Default)]
    struct Provider {
        inner: MockProvider,
        resolves: usize,
    }
    impl TextureProvider for Provider {
        fn resolve(&mut self, n: &str) -> Option<(TextureId, TextureInfo)> {
            self.resolves += 1;
            self.inner.resolve(n)
        }
        fn upload_rgba(
            &mut self,
            n: &str,
            w: u32,
            h: u32,
            p: &[u8],
        ) -> Option<(TextureId, TextureInfo)> {
            self.inner.upload_rgba(n, w, h, p)
        }
    }
    fn scene() -> Scene {
        let mut s = Scene::new();
        s.create("0", None); // live text before cached group: exercises index relocation
        s.create("1.body", Some("body".into()));
        s.create("1.face", Some("face".into()));
        s.get_mut("1").unwrap().props.intermediate_render = Some(2);
        s.get_mut("1")
            .unwrap()
            .props
            .custom
            .insert("intermediate_render_mask".into(), "mask".into());
        s.create("2", Some("icon".into()));
        s
    }
    fn frame(
        s: &Scene,
        p: &mut Provider,
        cache: Option<&mut SceneBuildCache>,
        glyphs: usize,
        edits: Option<&HashMap<String, String>>,
        keys: bool,
    ) -> DrawList {
        let mut source_scene = Scene::new();
        source_scene.create("glyph", Some("glyph".into()));
        let glyph =
            crate::compositor::build::build_frame(&source_scene, 0, p, None).commands[0].clone();
        let mut text = |id: &str| {
            if id == "0" {
                vec![glyph.clone(); glyphs]
            } else {
                Vec::new()
            }
        };
        build_frame_reusing_cached(
            s,
            0,
            p,
            None,
            Some(&mut text),
            edits,
            keys,
            DrawList::new(),
            cache,
        )
    }
    fn check(
        s: &Scene,
        p: &mut Provider,
        c: &mut SceneBuildCache,
        glyphs: usize,
        revision: u64,
        retry: bool,
        edits: Option<&HashMap<String, String>>,
        keys: bool,
    ) -> DrawList {
        c.begin(revision, retry, std::iter::once("0".into()));
        let got = frame(s, p, Some(c), glyphs, edits, keys);
        c.finish();
        let expected = frame(s, p, None, glyphs, edits, keys);
        assert_eq!(got, expected);
        let mut materialized = got.clone();
        let mut reference = expected;
        materialized.materialize_stencil_groups(crate::render_pipeline::shader::ALPHA_MASK_SHADER);
        reference.materialize_stencil_groups(crate::render_pipeline::shader::ALPHA_MASK_SHADER);
        assert_eq!(materialized, reference);
        got
    }
    #[test]
    fn cached_images_keep_live_text_and_relocate_group_masks() {
        let s = scene();
        let mut p = Provider::default();
        let mut c = SceneBuildCache::default();
        for keys in [false, true] {
            check(&s, &mut p, &mut c, 1, 1, false, None, keys);
            let hits = c.hits;
            let builds = c.builds;
            check(&s, &mut p, &mut c, 87, 1, false, None, keys);
            assert!(c.hits > hits);
            assert_eq!(c.builds, builds);
            p.resolves = 0;
            c.begin(1, false, std::iter::once("0".into()));
            frame(&s, &mut p, Some(&mut c), 88, None, keys);
            c.finish();
            assert_eq!(p.resolves, 1, "only the live glyph source should resolve");
        }
    }
    #[test]
    fn cached_fragments_invalidate_edits_hiding_parents_structure_and_restore() {
        let mut s = scene();
        let mut p = Provider::default();
        let mut c = SceneBuildCache::default();
        check(&s, &mut p, &mut c, 2, 1, false, None, false);
        s.get_mut("1.face").unwrap().props.alpha = Some(120);
        check(&s, &mut p, &mut c, 2, 1, false, None, false);
        s.set_root_props(&HashMap::from([
            ("left".into(), "15".into()),
            ("alpha".into(), "128".into()),
        ]));
        check(&s, &mut p, &mut c, 2, 1, false, None, false);
        s.get_mut("1").unwrap().props.visible = Some(false);
        check(&s, &mut p, &mut c, 3, 1, false, None, false);
        s.get_mut("1").unwrap().props.visible = Some(true);
        s.get_mut("1").unwrap().props.clip = Some([0.0, 0.0, 75.0, 100.0]);
        check(&s, &mut p, &mut c, 3, 1, false, None, false);
        s.delete("1.face");
        s.create("1.face", Some("different-face".into()));
        check(&s, &mut p, &mut c, 4, 1, false, None, false);
        s.create("1.extra", Some("new".into()));
        check(&s, &mut p, &mut c, 4, 1, false, None, false);
        let edits = HashMap::from([("1.body".into(), "edited".into())]);
        check(&s, &mut p, &mut c, 5, 1, false, Some(&edits), false);
        check(&s, &mut p, &mut c, 6, 1, false, None, false);
        s.replace_with(scene());
        check(&s, &mut p, &mut c, 7, 1, false, None, false);
    }
    #[test]
    fn fragment_cache_retries_incomplete_uploads_and_texture_generations() {
        let s = scene();
        let mut p = Provider::default();
        let mut c = SceneBuildCache::default();
        check(&s, &mut p, &mut c, 1, 1, false, None, false);
        let builds = c.builds;
        check(&s, &mut p, &mut c, 1, 1, true, None, false);
        assert!(c.builds > builds);
        let builds = c.builds;
        check(&s, &mut p, &mut c, 1, 2, false, None, false);
        assert!(c.builds > builds);
        c.clear();
        assert!(c.entries.is_empty());
    }
    #[test]
    fn new_dynamic_content_and_cross_layer_shader_are_not_frozen() {
        let mut s = scene();
        let mut p = Provider::default();
        let mut c = SceneBuildCache::default();
        check(&s, &mut p, &mut c, 1, 1, false, None, false);
        c.begin(1, false, ["0".into(), "1.face".into()].into_iter());
        let mut content = |id: &str| {
            if id == "1.face" {
                vec![
                    crate::compositor::build::build_frame(
                        &scene(),
                        0,
                        &mut MockProvider::new(),
                        None,
                    )
                    .commands[0]
                        .clone(),
                ]
            } else {
                Vec::new()
            }
        };
        let got = build_frame_reusing_cached(
            &s,
            0,
            &mut p,
            Some(&mut content),
            None,
            None,
            false,
            DrawList::new(),
            Some(&mut c),
        );
        c.finish();
        let expected = build_frame_reusing_cached(
            &s,
            0,
            &mut p,
            Some(&mut content),
            None,
            None,
            false,
            DrawList::new(),
            None,
        );
        assert_eq!(got, expected);
        s.get_mut("1").unwrap().props.shader = Some("custom".into());
        s.get_mut("1")
            .unwrap()
            .props
            .custom
            .insert("mask".into(), "2".into());
        check(&s, &mut p, &mut c, 1, 1, false, None, false);
        s.set_file("2", Some("changed-mask".into()));
        check(&s, &mut p, &mut c, 1, 1, false, None, false);
    }
}
