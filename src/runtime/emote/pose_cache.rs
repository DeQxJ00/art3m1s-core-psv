use art3m1s_emote::{EmoteRenderState, EmoteTransform};
use crate::render_pipeline::draw::DrawCommand;

/// One completed pose per instance. Mesh storage is shared with the submitted
/// draw list; no GPU textures or previous models are kept alive by this cache.
#[derive(Default)]
pub(super) struct PoseCache {
    value: Option<(EmoteRenderState, EmoteTransform, Vec<DrawCommand>)>,
    pub hits: u64,
    pub builds: u64,
}

impl PoseCache {
    pub fn get(&mut self, state: &EmoteRenderState, transform: EmoteTransform) -> Option<Vec<DrawCommand>> {
        let (previous, old_transform, commands) = self.value.as_ref()?;
        if previous.motion_time != state.motion_time || previous.variables != state.variables || *old_transform != transform {
            return None;
        }
        self.hits += 1;
        Some(commands.clone())
    }

    pub fn store(&mut self, state: EmoteRenderState, transform: EmoteTransform, commands: &[DrawCommand]) {
        self.builds += 1;
        self.value = Some((state, transform, commands.to_vec()));
    }

    pub fn invalidate(&mut self) { self.value = None; }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_pose_transform_and_nonfinite_inputs_do_not_hit() {
        let mut cache=PoseCache::default();let mut state=EmoteRenderState::default();
        let transform=EmoteTransform::default();
        assert!(cache.get(&state,transform).is_none());
        cache.store(state.clone(),transform,&[]);
        assert!(cache.get(&state,transform).is_some());
        state.variables.insert("mouth".into(),0.5);
        assert!(cache.get(&state,transform).is_none());
        cache.store(state.clone(),transform,&[]);
        let mut moved=transform;moved.coord[0]=3.0;
        assert!(cache.get(&state,moved).is_none());
        state.motion_time=1.;assert!(cache.get(&state,transform).is_none());
        state.variables.insert("eye".into(),f32::NAN);
        cache.store(state.clone(),transform,&[]);assert!(cache.get(&state,transform).is_none());
        cache.invalidate();assert!(cache.value.is_none());
    }

    #[test]
    #[ignore = "requires ART3M1S_FIXTURE_RGBA_EMOTE_MODEL"]
    fn model_cache_matches_fresh_evaluation_across_motion_and_upload_retry() {
        use super::super::{EmoteInstance, EmoteLayerCommand};
        use crate::compositor::mock::MockProvider;
        use crate::render_pipeline::draw::{TextureId, TextureInfo, TextureProvider};
        use std::collections::HashSet;
        let path=std::env::var("ART3M1S_FIXTURE_RGBA_EMOTE_MODEL").unwrap();
        let bytes=std::fs::read(path).unwrap();
        let mut cached=EmoteInstance::new(1,"model.psb",bytes.clone(),960,544).unwrap();
        let mut fresh=EmoteInstance::new(1,"model.psb",bytes.clone(),960,544).unwrap();
        let mut a=MockProvider::new();let mut b=MockProvider::new();
        for step in 0..80 {
            let command=match step {
                10=>Some(EmoteLayerCommand::SetVariable{label:"face_talk".into(),value:0.8,frames:0.,easing:0}),
                20=>Some(EmoteLayerCommand::SetScale{scale:0.6,origin_x:3.,origin_y:-2.}),
                30=>Some(EmoteLayerCommand::SetCoord{x:4.,y:10.,z:0.,angle:12.}),
                40=>Some(EmoteLayerCommand::PlayTimeline{label:cached.model.timelines().keys().next().unwrap().clone(),flags:1}),
                _=>None,
            };
            if let Some(command)=command{cached.command(command.clone());fresh.command(command);}
            let frames=if step%3==0{1.}else{0.};cached.advance(frames);fresh.advance(frames);
            let mut keep_a=HashSet::new();let mut keep_b=HashSet::new();
            let left=cached.build_commands(&mut a,&mut keep_a).unwrap();
            fresh.pose_cache.invalidate();
            let right=fresh.build_commands(&mut b,&mut keep_b).unwrap();
            assert!(!left.is_empty());assert_eq!(left,right,"pose step {step}");assert_eq!(keep_a,keep_b);
        }
        assert!(cached.pose_cache.hits>0);assert!(cached.pose_cache.builds>1);
        eprintln!("pose cache hits={} builds={} fresh={}",cached.pose_cache.hits,cached.pose_cache.builds,fresh.pose_cache.builds);

        struct RetryProvider { fail:bool, inner:MockProvider }
        impl TextureProvider for RetryProvider {
            fn resolve(&mut self,n:&str)->Option<(TextureId,TextureInfo)>{self.inner.resolve(n)}
            fn upload_rgba(&mut self,n:&str,w:u32,h:u32,d:&[u8])->Option<(TextureId,TextureInfo)>{
                if self.fail{None}else{self.inner.upload_rgba(n,w,h,d)}
            }
        }
        let mut retry=EmoteInstance::new(2,"model.psb",bytes,960,544).unwrap();
        let mut provider=RetryProvider{fail:true,inner:MockProvider::new()};let mut keep=HashSet::new();
        assert!(retry.build_commands(&mut provider,&mut keep).unwrap().is_empty());
        assert_eq!(retry.pose_cache.builds,0);
        provider.fail=false;let complete=retry.build_commands(&mut provider,&mut keep).unwrap();assert!(!complete.is_empty());
        assert_eq!(retry.build_commands(&mut provider,&mut keep).unwrap(),complete);
        assert_eq!(retry.pose_cache.hits,1);
    }
}
