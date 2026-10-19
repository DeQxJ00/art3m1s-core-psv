//! CPU-only fragments for static image subtrees; text/content stay live.
use super::scene::Scene;
use crate::render_pipeline::draw::{DrawList, ShaderEffect};
use glam::Affine2;
use std::collections::{BTreeMap, HashMap, HashSet};
use super::props::LayerProps;

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
struct ShaderValues {
    names: Vec<String>,
    raw: Vec<Option<String>>,
    width: Option<f32>,
    height: Option<f32>,
    values: BTreeMap<String, Vec<f32>>,
    seen: bool,
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
    shader_values: HashMap<String, ShaderValues>,
    dynamic: HashSet<String>,
    // A live descendant invalidates only its ancestors, not sibling fragments.
    dynamic_ancestors: HashSet<String>,
    // Frame-local only: Scene is immutable during one build; addresses are
    // identity keys, never dereferenced and never reused across begin().
    candidates: HashMap<usize, bool>,
    revision: Option<u64>,
    pub hits: u64,
    pub builds: u64,
    frames: u64,
    profile_frames: u64,
    profile_ns: [u64;3],
}
impl SceneBuildCache {
    pub fn begin(&mut self, revision: u64, retry: bool, dynamic: impl Iterator<Item = String>) {
        if retry || self.revision != Some(revision) {
            self.entries.clear();
        }
        self.revision = Some(revision);
        for values in self.shader_values.values_mut() { values.seen=false; }
        if self.frames % 120 == 0 { self.profile_frames += 1; }
        self.candidates.clear();
        let next_dynamic:HashSet<_>=dynamic.collect();
        if self.dynamic != next_dynamic {
            self.dynamic=next_dynamic;
            self.dynamic_ancestors.clear();
            for id in &self.dynamic {
                let mut parent = id.as_str();
                while let Some((prefix, _)) = parent.rsplit_once('.') {
                    if !self.dynamic_ancestors.insert(prefix.to_owned()) { break; }
                    parent = prefix;
                }
            }
        }
        for entry in self.entries.values_mut() {
            entry.seen = false;
        }
    }
    // Sparse CPU diagnostics; all frame construction still executes normally.
    pub(super) fn profile_mark(&self)->Option<std::time::Instant> {
        (self.frames % 120 == 0).then(std::time::Instant::now)
    }
    pub(super) fn profile_add(&mut self,slot:usize,start:Option<std::time::Instant>) {
        if let Some(start)=start {self.profile_ns[slot]+=start.elapsed().as_nanos() as u64;}
    }
    pub fn profile_sample(&self)->Option<(u64,[u64;3])> {
        (self.frames % 300 == 0 && self.profile_frames>0).then(||(self.profile_frames,self.profile_ns.map(|ns|ns/self.profile_frames/1000)))
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.shader_values.clear();
        self.candidates.clear();
        self.revision = None;
    }
    pub fn finish(&mut self) {
        self.entries.retain(|_, entry| entry.seen);
        self.shader_values.retain(|_, values| values.seen);
        self.frames += 1;
    }
    pub fn sample(&self) -> Option<(u64, u64, usize)> {
        (self.frames % 300 == 0).then_some((self.hits, self.builds, self.entries.len()))
    }
    // Only parsed scalar constants are retained. Cross-layer texture names,
    // provider generations and mask/user handles are resolved live by build.rs.
    // Compare authored inputs, not scene revision IDs (which can repeat after
    // a new scene/save). Resolved tween properties participate in this key.
    pub(super) fn shader_uniforms(&mut self, id: &str, props: &LayerProps) -> BTreeMap<String, Vec<f32>> {
        if let Some(entry)=self.shader_values.get_mut(id) {
            if entry.names==props.shader_constants && entry.width==props.width && entry.height==props.height
                && entry.raw.iter().zip(&entry.names).all(|(raw,name)|raw.as_ref()==props.custom.get(name)) {
                entry.seen=true;
                return entry.values.clone();
            }
        }
        let values: BTreeMap<_,_>=props.shader_constants.iter().filter_map(|name|
            super::build::shader_uniform_value(props,name).map(|value|(name.clone(),value))).collect();
        if props.shader_constants.len()<=64 && props.shader_constants.iter().map(|n|n.len()+props.custom.get(n).map_or(0,String::len)).sum::<usize>()<=4096
            && (self.shader_values.len()<128 || self.shader_values.contains_key(id)) {
            self.shader_values.insert(id.to_owned(),ShaderValues {
                names:props.shader_constants.clone(),raw:props.shader_constants.iter().map(|n|props.custom.get(n).cloned()).collect(),
                width:props.width,height:props.height,values:values.clone(),seen:true,
            });
        }
        values
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
    pub(super) fn is_dynamic_path(&self, id: &str) -> bool {
        self.dynamic.contains(id) || self.dynamic_ancestors.contains(id)
    }
    pub(super) fn candidate(&mut self, scene: &Scene, id: &str) -> bool {
        // Reject known live dependency paths before walking static siblings.
        // Refreshed on exact membership changes; removed/renamed content becomes static.
        if self.is_dynamic_path(id) { return false; }
        let Some(layer) = scene.get(id) else {
            return false;
        };
        let key=layer as *const super::scene::Layer as usize;
        if let Some(&candidate)=self.candidates.get(&key){return candidate;}
        // Cross-layer shader resource references and live tweens are evaluated
        // normally. Their static children can still use independent fragments.
        let candidate = layer.tweens.is_empty()
            && layer.props.shader.as_deref().is_none_or(str::is_empty)
            && layer.children.iter().all(|id| self.candidate(scene, id));
        self.candidates.insert(key,candidate);
        candidate
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
        let previous = self.entries.remove_entry(id);
        let used: usize = self
            .entries
            .values()
            .map(|e| e.frame.commands.len() + e.frame.mask_commands.len())
            .sum();
        // Empty fragments still save traversal; dependency and retry checks apply.
        if used + count > 1024 || self.entries.len() >= 128 {
            return;
        }
        let (key,mut nodes,mut fragment)=previous.map(|(key,entry)|(key,entry.nodes,entry.frame))
            .unwrap_or_else(||(id.to_owned(),Vec::new(),DrawList::new()));
        let mut cursor=0;
        fn collect(
            scene: &Scene,
            id: &str,
            edits: Option<&HashMap<String, String>>,
            out: &mut Vec<Stamp>,
            cursor: &mut usize,
        ) {
            if let Some(l) = scene.get(id) {
                let edited=edits.and_then(|e|e.get(id));
                if let Some(stamp)=out.get_mut(*cursor) {
                    if stamp.id!=id {id.clone_into(&mut stamp.id);}
                    stamp.revision=l.query_revision;
                    if stamp.edited.as_ref()!=edited {stamp.edited=edited.cloned();}
                } else {
                    out.push(Stamp {id:id.into(),revision:l.query_revision,edited:edited.cloned()});
                }
                *cursor+=1;
                for c in &l.children {
                    collect(scene, c, edits, out, cursor);
                }
            }
        }
        collect(scene, id, edits, &mut nodes, &mut cursor);
        nodes.truncate(cursor);
        // Refresh every dependency and command, preserving only CPU capacity.
        // Old mask/group offsets are discarded before applying the new origin.
        fragment.clear_for_rebuild();
        fragment.commands.extend_from_slice(&frame.commands[start[0]..]);
        fragment.command_keys.extend_from_slice(&frame.command_keys[start[0]..]);
        fragment.mask_commands.extend_from_slice(&frame.mask_commands[start[1]..]);
        fragment.shader_groups.extend_from_slice(&frame.shader_groups[start[2]..]);
        for g in &mut fragment.shader_groups {
            g.start -= start[0];
            g.end -= start[0];
            if let Some(r) = &mut g.mask_range {
                r[0] -= start[1];
                r[1] -= start[1];
            }
        }
        self.entries.insert(
            key,
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

    #[test]
    fn dynamic_ancestors_follow_removal_and_reparenting() {
        let mut cache=SceneBuildCache::default();
        cache.begin(1,false,["1.2.face".into()].into_iter());
        assert!(cache.is_dynamic_path("1.2"));assert!(!cache.is_dynamic_path("2"));
        let address=cache.dynamic_ancestors.get("1.2").unwrap().as_ptr();
        cache.begin(1,false,["1.2.face".into()].into_iter());
        assert_eq!(address,cache.dynamic_ancestors.get("1.2").unwrap().as_ptr());
        cache.begin(1,false,["2.face".into()].into_iter());
        assert!(!cache.is_dynamic_path("1"));assert!(cache.is_dynamic_path("2"));
        cache.begin(1,false,std::iter::empty());assert!(!cache.is_dynamic_path("2"));
    }
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
    fn shader_constants_follow_values_even_when_scene_ids_repeat() {
        let mut c=SceneBuildCache::default();let mut props=LayerProps::default();
        props.shader_constants=vec!["gain".into(),"width".into()];props.width=Some(960.);
        props.custom.insert("gain".into(),"1, 0.5, invalid".into());
        let first=c.shader_uniforms("same",&props);assert_eq!(first["gain"],vec![1.,0.5]);
        assert_eq!(c.shader_uniforms("same",&props),first);
        props.custom.insert("gain".into(),"0.25".into());props.width=Some(544.);
        let changed=c.shader_uniforms("same",&props);assert_eq!(changed["gain"],vec![0.25]);assert_eq!(changed["width"],vec![544.]);
        props.shader_constants=vec!["height".into()];props.height=Some(16.);
        let restored=c.shader_uniforms("same",&props);assert_eq!(restored.len(),1);assert_eq!(restored["height"],vec![16.]);
        c.begin(1,false,std::iter::empty());c.finish();assert!(c.shader_values.is_empty());
    }

    #[test]
    fn live_descendant_rejects_ancestors_without_visiting_static_siblings() {
        let mut s = scene();
        s.create("1.nested.actor", None);
        let mut c = SceneBuildCache::default();
        c.begin(1, false, std::iter::once("1.nested.actor".into()));
        assert!(!c.candidate(&s, "1"));
        assert!(c.candidates.is_empty());
        assert!(c.candidate(&s, "1.body"));
        assert!(c.candidate(&s, "2"));
        c.begin(1, false, std::iter::empty());
        assert!(c.candidate(&s, "1"));
        s.rename("1.nested", "2.nested");
        c.begin(1, false, std::iter::once("2.nested.actor".into()));
        assert!(c.candidate(&s, "1"));
        assert!(!c.candidate(&s, "2"));
    }

    #[test]
    fn empty_subtree_replays_then_invalidates_new_content_and_retry() {
        let mut s=Scene::new();s.create("1.empty.child",None);
        let mut p=Provider::default();let mut c=SceneBuildCache::default();
        check(&s,&mut p,&mut c,0,1,false,None,true);
        let builds=c.builds;
        check(&s,&mut p,&mut c,0,1,false,None,true);
        assert_eq!(c.builds,builds);assert_eq!(c.hits,1);
        s.create("1.empty.sprite",Some("pending".into()));p.inner.mark_missing("pending");
        assert!(check(&s,&mut p,&mut c,0,1,false,None,true).commands.is_empty());
        p.inner=MockProvider::new();
        assert_eq!(check(&s,&mut p,&mut c,0,1,true,None,true).commands.len(),1);
        s.get_mut("1.empty.sprite").unwrap().props.left=Some(23.);
        check(&s,&mut p,&mut c,0,1,false,None,true);
        s.replace_with(Scene::new());s.create("1.empty.child",Some("restored".into()));
        assert_eq!(check(&s,&mut p,&mut c,0,1,false,None,true).commands.len(),1);
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
