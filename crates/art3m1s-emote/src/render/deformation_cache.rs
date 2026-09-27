use super::{apply_deformers_at_resolution, EmoteDrawItem, EmoteMesh, HashMap, MeshDeformer};
#[cfg(test)]
use super::apply_deformers;

// One previous mesh per live layer path, bounded independently of model size.
// No textures, model resources, or GPU targets are retained here.
const MAX_BYTES: usize = 512 * 1024;
const MAX_ENTRIES: usize = 256;

#[derive(Debug)]
struct Entry {
    mesh_side: usize,
    generation: u64,
    size: [f32; 2],
    origin: [f32; 2],
    offset: [f32; 2],
    transform: [f32; 6],
    authored: Option<EmoteMesh>,
    ancestors: Vec<MeshDeformer>,
    result: Option<EmoteMesh>,
    bytes: usize,
}

impl Entry {
    fn matches(&self, item: &EmoteDrawItem, ancestors: &[MeshDeformer], mesh_side: usize) -> bool {
        self.mesh_side == mesh_side && self.size == [item.atlas_rect[2], item.atlas_rect[3]]
            && self.origin == item.origin
            && self.offset == item.frame_offset
            && self.transform == item.world_transform
            && self.authored == item.mesh
            && self.ancestors == ancestors
    }
}

#[derive(Default, Debug)]
pub(super) struct DeformationCache {
    entries: HashMap<Vec<usize>, Entry>,
    bytes: usize,
    hits: u64,
    builds: u64,
}

impl DeformationCache {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub(super) fn stats(&self) -> (u64, u64, usize) {
        (self.hits, self.builds, self.bytes)
    }

    pub(super) fn finish(&mut self, generation: u64) {
        self.entries.retain(|_, entry| {
            if entry.generation == generation { return true; }
            self.bytes -= entry.bytes;
            false
        });
    }

    #[cfg(test)]
    fn apply(&mut self, item: &mut EmoteDrawItem, ancestors: &[MeshDeformer], path: &[usize], generation: u64) {
        self.apply_at_resolution(item, ancestors, path, generation, super::DEFORMED_MESH_SIDE);
    }

    pub(super) fn apply_at_resolution(&mut self, item: &mut EmoteDrawItem, ancestors: &[MeshDeformer], path: &[usize], generation: u64, mesh_side: usize) {
        if let Some(entry) = self.entries.get_mut(path) {
            if entry.matches(item, ancestors, mesh_side) {
                // Only geometry is reused. Opacity, color, texture, ordering,
                // stencil references and all other evaluated channels stay live.
                entry.generation = generation;
                item.mesh = entry.result.clone();
                self.hits += 1;
                return;
            }
        }
        if let Some(old) = self.entries.remove(path) { self.bytes -= old.bytes; }
        self.builds += 1;
        // Plain quads have no expensive deformation work to preserve.
        if item.mesh.is_none() && ancestors.is_empty() { return; }
        let base_bytes = std::mem::size_of::<Entry>() + 128 + std::mem::size_of_val(path)
            + mesh_bytes(item.mesh.as_ref())
            + std::mem::size_of_val(ancestors)
            + ancestors.iter().map(|ancestor| std::mem::size_of_val(ancestor.points.as_slice())).sum::<usize>();
        let admit = self.entries.len() < MAX_ENTRIES && base_bytes <= MAX_BYTES.saturating_sub(self.bytes);
        let authored = admit.then(|| item.mesh.clone());
        apply_deformers_at_resolution(item, ancestors, mesh_side);
        if let Some(authored) = authored {
            // Oversized authored grids may pass through unchanged; use actual
            // output size rather than assuming every result is tessellated.
            let bytes = base_bytes + mesh_bytes(item.mesh.as_ref());
            if bytes > MAX_BYTES.saturating_sub(self.bytes) { return; }
            self.entries.insert(path.to_vec(), Entry {
                mesh_side, generation, size: [item.atlas_rect[2], item.atlas_rect[3]], origin: item.origin,
                offset: item.frame_offset, transform: item.world_transform, authored,
                ancestors: ancestors.to_vec(), result: item.mesh.clone(), bytes,
            });
            self.bytes += bytes;
        }
    }
}

fn mesh_bytes(mesh: Option<&EmoteMesh>) -> usize {
    mesh.map_or(0, |m| m.blend_points.as_ref().map_or(0, |v| std::mem::size_of_val(v.as_slice()))
        + m.control_coordinates.as_ref().map_or(0, |v| std::mem::size_of_val(v.as_slice())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{EmoteAffine, EmoteEvaluationHistory, EmoteModel, EmoteMotionEvaluator, EmoteRenderState};

    fn item() -> EmoteDrawItem {
        let mut points = super::super::identity_grid();
        points[11] += 0.4;
        EmoteDrawItem {
            layer_label: "part".into(), texture_id: "atlas".into(), icon_id: "0".into(),
            atlas_rect: [0., 0., 100., 80.], origin: [4., 2.], translation: [0.;3], angle: 0.,
            world_transform: EmoteAffine::identity().as_array(), frame_offset: [0.;2],
            opacity: 1., blend_mode: 0, color: vec![255.;4], z_order: 0, draw_order: vec![1],
            mesh: Some(EmoteMesh { blend_points: Some(points), control_coordinates: None }),
            stencil_mask_layers: vec![],
        }
    }

    fn ancestor() -> MeshDeformer {
        MeshDeformer { combine: false, transform: EmoteAffine::identity(), translation: [0.;2],
            angle: 0., size: [120.,120.], origin: [0.;2], offset: [0.;2],
            points: super::super::identity_grid(), side: 4 }
    }

    #[test]
    fn resolution_changes_rebuild_geometry_and_preserve_patch_corners() {
        let mut cache = DeformationCache::default();
        let mut baseline = item();
        apply_deformers(&mut baseline, &[ancestor()]);
        let original = baseline.mesh.unwrap().blend_points.unwrap();
        for (generation, side) in [8, 6, 5, 3, 2, 8].into_iter().enumerate() {
            let mut value = item();
            cache.apply_at_resolution(&mut value, &[ancestor()], &[1], generation as u64, side);
            let points = value.mesh.unwrap().blend_points.unwrap();
            assert_eq!(points.len(), side * side * 2);
            for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let a = (y * (side - 1) * side + x * (side - 1)) * 2;
                let b = (y * 7 * 8 + x * 7) * 2;
                assert_eq!(&points[a..a+2], &original[b..b+2]);
            }
            if side == 8 { assert_eq!(points, original); }
            assert_eq!(cache.hits, 0);
        }
    }

    #[test]
    #[ignore = "requires ART3M1S_FIXTURE_RGBA_EMOTE_MODEL"]
    fn fixture_ratios_reduce_geometry_without_replaying_cached_resolution() {
        let model = EmoteModel::open(std::env::var("ART3M1S_FIXTURE_RGBA_EMOTE_MODEL").unwrap()).unwrap();
        let mut cached = EmoteEvaluationHistory::default();
        let mut state = EmoteRenderState::default();
        state.variables.insert("face_talk".into(), 2.5);
        let baseline = EmoteMotionEvaluator::new(&model).evaluate_base_with_history(&state, &mut cached).unwrap();
        let count = |items: &[EmoteDrawItem]| items.iter().filter_map(|i| i.mesh.as_ref())
            .filter_map(|m| m.blend_points.as_ref()).map(|p| p.len()).sum::<usize>();
        let mut previous = count(&baseline);
        for ratio in [0.8, 0.6, 0.4, 0.25] {
            let evaluator = EmoteMotionEvaluator::new(&model).with_mesh_division_ratio(ratio);
            let warm = evaluator.evaluate_base_with_history(&state, &mut cached).unwrap();
            assert_eq!(warm, evaluator.evaluate_base(&state).unwrap());
            assert_eq!(warm.len(), baseline.len());
            let points = count(&warm);
            assert!(points < previous); previous = points;
        }
        let minimum = EmoteMotionEvaluator::new(&model).with_mesh_division_ratio(0.25)
            .evaluate_base_with_history(&state, &mut cached).unwrap();
        assert_eq!(EmoteMotionEvaluator::new(&model).with_mesh_division_ratio(0.20)
            .evaluate_base_with_history(&state, &mut cached).unwrap(), minimum);
        for ratio in [1.0, 0.0, -1.0, 2.0, f32::NAN, f32::INFINITY] {
            assert_eq!(EmoteMotionEvaluator::new(&model).with_mesh_division_ratio(ratio)
                .evaluate_base_with_history(&state, &mut cached).unwrap(), baseline);
        }
    }

    #[test]
    fn unchanged_geometry_reuses_mesh_but_live_material_and_stencil_survive() {
        let mut cache = DeformationCache::default();
        let mut first = item(); cache.apply(&mut first, &[ancestor()], &[1,2], 1);
        let mut next = item(); next.opacity = 0.4; next.color[0] = 13.; next.blend_mode = 2;
        next.texture_id = "replacement".into(); next.atlas_rect[0] = 30.;
        next.draw_order = vec![3,8]; next.stencil_mask_layers = vec!["eye".into()];
        let mut expected = next.clone(); apply_deformers(&mut expected, &[ancestor()]);
        cache.apply(&mut next, &[ancestor()], &[1,2], 2);
        assert_eq!(next, expected); assert_eq!(cache.hits, 1);
        cache.finish(2); assert_eq!(cache.entries.len(), 1);
        cache.finish(3); assert!(cache.entries.is_empty()); assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn geometry_changes_never_replay_a_stale_mesh() {
        for change in 0..12 {
            let mut cache = DeformationCache::default();
            cache.apply(&mut item(), &[ancestor()], &[1], 1);
            let mut changed = item(); let mut chain = vec![ancestor()];
            match change {
                0 => changed.atlas_rect[2] += 1.,
                1 => changed.origin[0] += 1.,
                2 => changed.frame_offset[1] += 1.,
                3 => changed.world_transform[4] += 2.,
                4 => changed.mesh.as_mut().unwrap().blend_points.as_mut().unwrap()[8] += 0.1,
                5 => changed.mesh.as_mut().unwrap().control_coordinates = Some(vec![2.]),
                6 => chain[0].transform.tx += 1.,
                7 => chain[0].size[0] += 1.,
                8 => chain[0].origin[0] += 1.,
                9 => chain[0].offset[1] += 1.,
                10 => chain[0].points[12] += 0.2,
                _ => chain.clear(),
            }
            let mut expected = changed.clone(); apply_deformers(&mut expected, &chain);
            cache.apply(&mut changed, &chain, &[1], 2);
            assert_eq!(changed, expected, "change {change}"); assert_eq!(cache.hits, 0);
        }
        let mut cache = DeformationCache::default();
        for _ in 0..2 {
            let mut nonfinite = item(); nonfinite.world_transform[0] = f32::NAN;
            cache.apply(&mut nonfinite, &[], &[1], 1);
        }
        assert_eq!(cache.hits, 0);
    }

    #[test]
    fn storage_is_bounded_and_layer_instances_do_not_alias() {
        let mut cache = DeformationCache::default();
        for n in 0..MAX_ENTRIES * 3 {
            let mut value = item(); let mut expected = value.clone();
            apply_deformers(&mut expected, &[ancestor()]);
            cache.apply(&mut value, &[ancestor()], &[n], 1);
            assert_eq!(value, expected);
            assert!(cache.bytes <= MAX_BYTES); assert!(cache.entries.len() <= MAX_ENTRIES);
        }
        assert_eq!(cache.hits, 0);
        cache.clear(); assert_eq!(cache.bytes, 0); assert!(cache.entries.is_empty());
        let mut huge = item(); huge.mesh.as_mut().unwrap().control_coordinates = Some(vec![0.;MAX_BYTES]);
        let mut expected = huge.clone(); apply_deformers(&mut expected, &[]);
        cache.apply(&mut huge, &[], &[1], 2);
        assert_eq!(huge, expected); assert!(cache.entries.is_empty());
    }

    #[test]
    #[ignore = "requires ART3M1S_FIXTURE_RGBA_EMOTE_MODEL"]
    fn fixture_all_timelines_match_uncached_meshes_and_model_switch_clears_history() {
        use std::time::Instant;
        let path = std::env::var("ART3M1S_FIXTURE_RGBA_EMOTE_MODEL").unwrap();
        let model = EmoteModel::open(&path).unwrap();
        let evaluator = EmoteMotionEvaluator::new(&model);
        let mut cached = EmoteEvaluationHistory::default();
        let mut reference = EmoteEvaluationHistory::default();
        let mut cached_us = 0; let mut reference_us = 0; let mut samples = 0;
        for label in model.timelines().keys() {
            let mut player = crate::EmotePlayer::default();
            player.play_model_timeline(&model, label, 1);
            for step in 0..40 {
                player.advance_model(&model, if step % 7 == 0 { 0. } else { 1.5 });
                player.set_variable("face_talk", (step as f32 / 4.).sin().abs(), 0., 0);
                let state = EmoteRenderState { motion_time: 0., variables: player.evaluated_variables() };
                let start = Instant::now();
                let actual = evaluator.evaluate_base_with_history(&state, &mut cached).unwrap();
                cached_us += start.elapsed().as_micros();
                reference.deformations.clear();
                let start = Instant::now();
                let expected = evaluator.evaluate_base_with_history(&state, &mut reference).unwrap();
                reference_us += start.elapsed().as_micros();
                assert_eq!(actual, expected, "timeline {label}, step {step}");
                samples += 1;
                player.take_commands().for_each(drop);
            }
        }
        let (hits, builds, bytes) = cached.deformation_cache_stats();
        assert!(hits > 0); assert!(builds > 0); assert!(bytes <= MAX_BYTES);
        eprintln!("mesh-cache samples={samples} hits={hits} builds={builds} bytes={bytes} cached_us={cached_us} reference_us={reference_us}");
        let replacement = EmoteModel::open(&path).unwrap();
        cached.begin(&replacement);
        assert!(cached.deformations.entries.is_empty()); assert_eq!(cached.deformations.bytes, 0);
    }
}
