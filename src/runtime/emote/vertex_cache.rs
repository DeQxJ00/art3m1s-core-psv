use super::{draw_mesh, DrawMesh};

const MAX_SLOTS: usize = 256;
const MAX_BYTES: usize = 512 * 1024;

struct Entry {
    points: Vec<f32>,
    size: [f32; 2],
    mesh: DrawMesh,
    bytes: usize,
}

/// One expanded CPU mesh per draw position. Exact geometry comparison makes
/// reordering safe; texture/material/transform state is never cached here.
#[derive(Default)]
pub(super) struct VertexCache {
    slots: Vec<Option<Entry>>,
    pub hits: u64,
    pub builds: u64,
    pub bytes: usize,
}

impl VertexCache {
    pub fn begin(&mut self, count: usize) {
        let count = count.min(MAX_SLOTS);
        for index in count..self.slots.len() { self.discard(index); }
        self.slots.truncate(count);
        self.slots.resize_with(count, || None);
    }

    pub fn discard(&mut self, index: usize) {
        if let Some(slot) = self.slots.get_mut(index) {
            if let Some(entry) = slot.take() { self.bytes -= entry.bytes; }
        }
    }

    pub fn get(&mut self, index: usize, points: Option<&[f32]>, width: f32, height: f32) -> Option<DrawMesh> {
        if let Some(entry) = self.slots.get(index).and_then(Option::as_ref) {
            if Some(entry.points.as_slice()) == points && entry.size == [width, height] {
                self.hits += 1;
                return Some(entry.mesh.clone());
            }
        }
        self.discard(index);
        self.builds += 1;
        let mesh = draw_mesh(points, width, height)?;
        let bytes = std::mem::size_of::<Entry>() + 32
            + std::mem::size_of_val(points?) + std::mem::size_of_val(mesh.vertices.as_ref());
        if index < self.slots.len() && bytes <= MAX_BYTES.saturating_sub(self.bytes) {
            self.slots[index] = Some(Entry { points: points?.to_vec(), size: [width, height], mesh: mesh.clone(), bytes });
            self.bytes += bytes;
        }
        Some(mesh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn points(side: usize) -> Vec<f32> {
        (0..side * side).flat_map(|i| [i as f32 / (side * side) as f32, (i % side) as f32 / side as f32]).collect()
    }

    #[test]
    fn reuses_vertices_and_invalidates_on_geometry_size_and_slot_changes() {
        let mut cache = VertexCache::default(); cache.begin(2);
        let a = points(8); let b = points(4);
        let first = cache.get(0, Some(&a), 100., 200.).unwrap();
        let same = cache.get(0, Some(&a), 100., 200.).unwrap();
        assert!(Arc::ptr_eq(&first.vertices, &same.vertices));
        for (slot, input, w, h) in [(0, &a, 101., 200.), (0, &a, 101., 199.), (0, &b, 100., 200.), (1, &a, 100., 200.)] {
            assert_eq!(cache.get(slot, Some(input), w, h), draw_mesh(Some(input), w, h));
        }
        assert_eq!(cache.hits, 1);
        assert!(cache.get(0, None, 100., 200.).is_none());
        assert!(cache.slots[0].is_none());
        cache.begin(0); assert_eq!(cache.bytes, 0);
        // A frame in flight may still own the old Arc after cache eviction.
        assert_eq!(first, draw_mesh(Some(&a), 100., 200.).unwrap());
    }

    #[test]
    fn budget_pressure_and_invalid_inputs_preserve_uncached_output() {
        let mut cache = VertexCache::default(); cache.begin(1024);
        let p = points(8);
        for i in 0..512 {
            assert_eq!(cache.get(i, Some(&p), 100., 200.), draw_mesh(Some(&p), 100., 200.));
            assert!(cache.bytes <= MAX_BYTES); assert!(cache.slots.len() <= MAX_SLOTS);
        }
        for p in [vec![], vec![0.;3], vec![0.;12]] {
            assert_eq!(cache.get(0, Some(&p), 100., 200.), draw_mesh(Some(&p), 100., 200.));
            assert!(cache.slots[0].is_none());
        }
        let old_hits = cache.hits;
        let mut nan = p.clone(); nan[0] = f32::NAN;
        cache.get(0, Some(&nan), 100., 200.); cache.get(0, Some(&nan), 100., 200.);
        assert_eq!(cache.hits, old_hits);
        cache.begin(0); assert_eq!(cache.bytes, 0);
    }

    #[test]
    #[ignore = "requires ART3M1S_FIXTURE_RGBA_EMOTE_MODEL"]
    fn fixture_draw_commands_match_cold_vertices_across_all_timelines() {
        use super::super::EmoteInstance;
        use crate::compositor::mock::MockProvider;
        use std::collections::HashSet;
        let data = std::fs::read(std::env::var("ART3M1S_FIXTURE_RGBA_EMOTE_MODEL").unwrap()).unwrap();
        let mut instance = EmoteInstance::new(1, "model.psb", data, 960, 544).unwrap();
        let labels = instance.model.timelines().keys().cloned().collect::<Vec<_>>();
        let mut provider = MockProvider::new(); let mut retained = HashSet::new(); let mut count = 0;
        for label in labels {
            instance.player.play_model_timeline(&instance.model, label, 0);
            for step in 0..40 {
                instance.advance(1.5);
                instance.player.set_variable("face_talk", (step as f32 / 4.).sin().abs(), 0., 0);
                instance.player.set_coord(step as f32, -10., 0., step as f32 * 2.);
                instance.player.set_scale(0.6 + step as f32 / 100., 3., 4.);
                instance.pose_cache.invalidate();
                let actual = instance.build_commands(&mut provider, &mut retained).unwrap();
                instance.vertex_cache.begin(0);
                instance.pose_cache.invalidate();
                let expected = instance.build_commands(&mut provider, &mut retained).unwrap();
                assert_eq!(actual, expected);
                assert!(instance.vertex_cache.bytes <= MAX_BYTES);
                count += 1;
                instance.player.take_commands().for_each(drop);
            }
        }
        assert!(instance.vertex_cache.hits > 0);
        eprintln!("vertex-cache compared={count} hits={} builds={} bytes={}", instance.vertex_cache.hits, instance.vertex_cache.builds, instance.vertex_cache.bytes);
    }
}
