//! Immutable input dependencies for a generic effect node.
//! Host stores the actual pre-filter render target; this describes its inputs.
use super::*;

#[derive(Default)]
pub(super) struct InputSnapshot {
    pub valid: bool,
    size:(u32,u32), programs:u64,
    commands:Vec<DrawCommand>, groups:Vec<ShaderGroup>, masks:Vec<DrawCommand>,
    textures:Vec<(u64,u64)>,
}
impl InputSnapshot {
    pub fn describe_difference(&self,frame:&DrawList,gi:usize,size:(u32,u32))->String {
        let g=&frame.shader_groups[gi];let commands=&frame.commands[g.start..g.end];
        if self.size!=size {return format!("stage={:?}->{size:?}",self.size);}
        if self.programs!=super::super::external_effects::revision(){return "programs".into();}
        if self.commands.len()!=commands.len(){return format!("commands={}->{}",self.commands.len(),commands.len());}
        if let Some((i,(a,b)))=self.commands.iter().zip(commands).enumerate().find(|(_, (a,b))|a!=b) {
            return format!("command[{i}] texture={}->{} opacity={}->{} transform={:?}->{:?} clip={:?}->{:?}",
                a.texture.0,b.texture.0,a.opacity,b.opacity,a.transform,b.transform,a.clip_bounds,b.clip_bounds);
        }
        if self.groups.len()!=Self::nested(frame,gi).count(){return "group-count".into();}
        for (a,b) in self.groups.iter().zip(Self::nested(frame,gi)) {
            if a.start!=b.start-g.start||a.end!=b.end-g.start||a.key!=b.key {return "group-layout".into();}
            if a.effect!=b.effect{return format!("child={:?} effect={:?}->{:?}",b.key,a.effect,b.effect);}
            if a.clip_bounds!=b.clip_bounds {return format!("child={:?} clip={:?}->{:?}",b.key,a.clip_bounds,b.clip_bounds);}
        }
        if let Some(&(id,r))=self.textures.iter().find(|&&(id,r)|unsafe{art3m1s_gxm_texture_content_revision(id)!=r}) {
            return format!("texture[{id}] revision={r}->{}",unsafe{art3m1s_gxm_texture_content_revision(id)});
        }
        if self.dependencies_match(frame,gi,size){"dependencies-unchanged".into()}else{"referenced-masks".into()}
    }
    fn nested(frame:&DrawList,gi:usize)->impl Iterator<Item=&ShaderGroup> + '_ {
        let g=&frame.shader_groups[gi];
        frame.shader_groups.iter().enumerate().take(gi)
            .filter(move |(_,n)|n.start>=g.start && n.end<=g.end && n.start<n.end)
            .map(|(_,n)|n)
    }
    pub fn matches(&self,frame:&DrawList,gi:usize,size:(u32,u32))->bool {
        self.valid && self.dependencies_match(frame,gi,size)
    }
    pub fn dependencies_match(&self,frame:&DrawList,gi:usize,size:(u32,u32))->bool {
        let g=&frame.shader_groups[gi];
        self.size==size
            && self.programs==super::super::external_effects::revision()
            && self.commands==frame.commands[g.start..g.end]
            && self.groups.len()==Self::nested(frame,gi).count()
            && self.groups.iter().zip(Self::nested(frame,gi)).all(|(a,b)|
                a.start==b.start-g.start && a.end==b.end-g.start && a.key==b.key
                && a.effect==b.effect && a.clip_bounds==b.clip_bounds
                && match (a.mask_range,b.mask_range) {
                    (None,None)=>true,
                    (Some([a0,a1]),Some([b0,b1]))=>match (self.masks.get(a0..a1),frame.mask_commands.get(b0..b1)) {
                        (Some(a),Some(b))=>a==b,
                        _=>false,
                    },
                    _=>false,
                })
            && self.textures.iter().all(|&(id,r)|unsafe{art3m1s_gxm_texture_content_revision(id)==r})
    }
    pub fn store(&mut self,frame:&DrawList,gi:usize,size:(u32,u32)) {
        let g=&frame.shader_groups[gi];self.size=size;
        self.programs=super::super::external_effects::revision();
        self.commands.clear();self.commands.extend_from_slice(&frame.commands[g.start..g.end]);
        self.groups.clear();self.masks.clear();
        for n in Self::nested(frame,gi) {
            let mut n=n.clone();n.start-=g.start;n.end-=g.start;
            // Only masks consumed inside this input affect its pixels. A UI
            // portrait elsewhere in the frame may animate independently.
            if let Some([start,end])=n.mask_range {
                let Some(masks)=frame.mask_commands.get(start..end) else {self.valid=false;return;};
                let start=self.masks.len();self.masks.extend_from_slice(masks);
                n.mask_range=Some([start,self.masks.len()]);
            }
            self.groups.push(n);
        }
        let mut ids=Vec::new();
        for c in self.commands.iter().chain(self.masks.iter()) {
            ids.push(c.texture.0);
            if let Some(e)=&c.shader{ids.extend(e.mask_texture.into_iter().chain(e.user_texture).map(|t|t.0));}
        }
        for g in &self.groups {ids.extend(g.effect.mask_texture.into_iter().chain(g.effect.user_texture).map(|t|t.0));}
        ids.sort_unstable();ids.dedup();self.textures.clear();
        self.textures.extend(ids.into_iter().map(|id|(id,unsafe{art3m1s_gxm_texture_content_revision(id)})));
    }
}
