//! Immutable playback metadata, rebuilt only when the model identity changes.
use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) struct LayerPlan {
    pub has_hold: bool,
    sorted: bool,
}

impl LayerPlan {
    pub fn new(layer: &EmoteLayer) -> Self {
        Self {
            has_hold: layer.frames.iter().any(|f| f.frame_type == 0),
            sorted: layer.frames.iter().all(|f| !f.time.is_nan())
                && layer.frames.windows(2).all(|p| p[0].time <= p[1].time),
        }
    }
    pub fn cursor(self, layer: &EmoteLayer, time: f32) -> Option<usize> {
        if self.sorted {
            layer.frames.partition_point(|f| f.time <= time).checked_sub(1)
        } else {
            // Preserve authored order for malformed/non-monotonic timelines.
            layer.frames.iter().rposition(|f| f.time <= time)
        }
    }
}

pub(super) fn compile(model: &EmoteModel) -> HashMap<usize, LayerPlan> {
    fn visit(layer: &EmoteLayer, plans: &mut HashMap<usize, LayerPlan>) {
        plans.insert(layer as *const EmoteLayer as usize, LayerPlan::new(layer));
        for child in &layer.children { visit(child, plans); }
    }
    let mut plans = HashMap::new();
    for motions in model.motions().characters().values() {
        for motion in motions.values() { for layer in &motion.layers { visit(layer, &mut plans); } }
    }
    plans
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiled_cursor_matches_authored_search_at_boundaries() {
        let mut layer = super::super::tests::sampled_layer(3);
        for times in [vec![0., 1., 1., 8.],vec![3., 0., 8., 2.],vec![0.,f32::NAN,8.,9.]] {
            let template=layer.frames[0].clone();
            layer.frames=times.into_iter().enumerate().map(|(i,time)|crate::EmoteLayerFrame{time,frame_type:if i==2 {0}else{3},..template.clone()}).collect();
            let plan=LayerPlan::new(&layer);assert!(plan.has_hold);
            for time in [f32::NEG_INFINITY,-1.,0.,0.999,1.,2.,3.,8.,100.,f32::INFINITY,f32::NAN] {
                assert_eq!(plan.cursor(&layer,time),layer.frames.iter().rposition(|f|f.time<=time));
            }
        }
        layer.frames.clear();let plan=LayerPlan::new(&layer);
        assert!(!plan.has_hold);assert_eq!(plan.cursor(&layer,0.),None);
    }
}
