//! Direct GXM built-in effects bridge. Opt-in so the legacy host ABI is intact.
use crate::render_pipeline::{draw::*, shader::*};
thread_local! { static INPUT_TRACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub(super) fn set_profile_enabled(enabled:bool){INPUT_TRACE.with(|v|v.set(enabled));}

#[repr(C)]
#[derive(Clone, Copy)]
struct Effects {
    flags: [f32; 4],
    transition: [f32; 4],
    corners: [f32; 16],
    uv_rect: [f32; 4],
    model_clip: [f32; 4],
    wipe: [f32; 4],
    model_x: [f32; 4],
    model_y: [f32; 4],
}
#[repr(C)]
struct EffectDraw {
    texture: u64,
    mask: u64,
    transform: [f32; 6],
    quad: [f32; 2],
    uv: [f32; 4],
    tint: [f32; 4],
    clip: [f32; 4],
    effects: Effects,
    blend: u32,
    has_clip: u32,
    mesh: *const [f32; 4],
    mesh_count: usize,
    custom: super::external_effects::CustomDraw,
    grid_side: u32,
}
unsafe extern "C" {
    fn art3m1s_gxm_draw_effect(draw: *const EffectDraw);
    fn art3m1s_gxm_group_filter_chain(draw:*const EffectDraw,count:u32)->i32;
    fn art3m1s_gxm_group_end_half_blur(draw:*const EffectDraw,count:u32)->i32;
    fn art3m1s_gxm_draw_cached_blur(source:*const EffectDraw,passes:*const EffectDraw,count:u32,revision:u64)->i32;
    fn art3m1s_gxm_draw_cached_effect(sources:*const EffectDraw,n:u32,passes:*const EffectDraw,kinds:*const u32,count:u32,revision:u64,half:u32)->i32;
    fn art3m1s_gxm_node_source_draw(draw:*const EffectDraw,slot:u32)->i32;
    fn art3m1s_gxm_node_source_enabled()->i32;
    fn art3m1s_gxm_cache_slot_revision(slot:u32)->u64;
    fn art3m1s_gxm_node_source_end(draw:*const EffectDraw,slot:u32)->i32;
    fn art3m1s_gxm_group_input_reuse_enabled()->i32;
    fn art3m1s_gxm_group_begin_cached_input(slot:u32)->i32;
    fn art3m1s_gxm_group_begin() -> i32;
    fn art3m1s_gxm_overlay_cache_enabled() -> i32;
    fn art3m1s_gxm_overlay_end_cached(slot:u32, bounds:*const f32) -> i32;
    fn art3m1s_gxm_group_mask_begin() -> i32;
    fn art3m1s_gxm_group_mask_revision() -> u64;
    fn art3m1s_gxm_group_mask_reuse(revision: u64) -> i32;
    fn art3m1s_gxm_emote_composite_cache_enabled() -> i32;
    fn art3m1s_gxm_group_end(draw: *const EffectDraw);
    fn art3m1s_gxm_texture_is_opaque(id: u64) -> i32;
    fn art3m1s_gxm_texture_is_empty(id: u64) -> i32;
    fn art3m1s_gxm_texture_region_is_opaque(id:u64,u0:f32,v0:f32,u1:f32,v1:f32)->i32;
    fn art3m1s_gxm_report_groups(total: u32, flattened: u32);
    fn art3m1s_gxm_group_passthrough_enabled() -> i32;
    fn art3m1s_gxm_local_base_enabled() -> i32;
    fn art3m1s_gxm_texture_revision() -> u64;
    fn art3m1s_gxm_texture_content_revision(id:u64) -> u64;
    fn art3m1s_gxm_draw_cached_group(slot:u32) -> i32;
    fn art3m1s_gxm_group_end_cached(draw: *const EffectDraw,slot:u32) -> i32;
}
fn blend_code(blend: BlendMode) -> u32 {
    match blend {
        BlendMode::Alpha => 0,
        BlendMode::Add => 1,
        BlendMode::Multiply => 2,
        BlendMode::Screen => 3,
        BlendMode::NativeReverseSubtract => 4,
        BlendMode::PremultipliedAlpha => 5,
        BlendMode::PremultipliedAdd => 6,
        BlendMode::NativeAdd => 7,
        BlendMode::NativeMultiply => 8,
        BlendMode::NativeScreen => 9,
    }
}
fn scalar(effect: &ShaderEffect, name: &str, default: f32) -> f32 {
    effect
        .uniforms
        .get(name)
        .and_then(|v| v.first())
        .copied()
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}
fn kind(effect: Option<&ShaderEffect>) -> u32 {
    match effect.map(|e| e.name.as_str()) {
        Some(RULE_TRANS_SHADER) => 1,
        Some(ALPHA_MASK_SHADER) => 2,
        Some(GROUP_COMPOSITE_SHADER) => 3,
        _ => 0,
    }
}
fn encode(cmd: &DrawCommand, width: u32, height: u32) -> Option<EffectDraw> {
    let clip = super::stage_clip(cmd.clip_bounds, width, height).ok()?;
    let m = cmd.transform.matrix2;
    let t = cmd.transform.translation;
    let mut draw = EffectDraw {
        texture: cmd.texture.0,
        mask: cmd
            .shader
            .as_ref()
            .and_then(|e| e.mask_texture)
            .map_or(0, |t| t.0),
        transform: [m.x_axis.x, m.x_axis.y, m.y_axis.x, m.y_axis.y, t.x, t.y],
        quad: cmd.clip.quad_size,
        uv: [
            cmd.clip.uv_offset[0],
            cmd.clip.uv_offset[1],
            cmd.clip.uv_scale[0],
            cmd.clip.uv_scale[1],
        ],
        tint: [
            cmd.color.multiply[0],
            cmd.color.multiply[1],
            cmd.color.multiply[2],
            cmd.opacity,
        ],
        clip: clip.unwrap_or_default(),
        has_clip: u32::from(clip.is_some()),
        blend: blend_code(cmd.blend),
        mesh: std::ptr::null(),
        mesh_count: 0,
        grid_side: 0,
        custom: super::external_effects::encode(cmd.shader.as_ref(),cmd.opacity,cmd.color.multiply),
        effects: Effects {
            flags: [
                kind(cmd.shader.as_ref()) as f32,
                u32::from(cmd.color.grayscale) as f32,
                u32::from(cmd.color.negative) as f32,
                0.0,
            ],
            transition: [0.0, 1.0 / 255.0, 0.0, 0.0],
            corners: [0.0; 16],
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            model_clip: [0.0, 0.0, 1.0, 1.0],
            wipe: [0.0; 4],
            model_x: [0.0; 4],
            model_y: [0.0; 4],
        },
    };
    if let Some(mesh) = &cmd.mesh {
        draw.mesh = mesh.vertices.as_ptr();
        draw.mesh_count = mesh.vertices.len();
        draw.grid_side = mesh.grid_side;
    }
    if let Some(e) = &cmd.shader {
        if kind(Some(e)) != 0 {
            draw.tint[3] = scalar(e, "alpha", draw.tint[3]);
            if let Some(v) = e.uniforms.get("colorMultiply").filter(|v| v.len() >= 3) {
                draw.tint[..3].copy_from_slice(&v[..3]);
            }
            draw.effects.transition = [
                scalar(e, "progress", 0.0),
                scalar(e, "vague", 1.0 / 255.0),
                scalar(e, "opaque", 0.0),
                0.0,
            ];
            // These names exist on group-composite, not rule/alpha-mask.
            if kind(Some(e)) == 3 {
                draw.effects.flags[1] = scalar(e, "grayscale", 0.0);
                draw.effects.flags[2] = scalar(e, "negative", 0.0);
            }
        }
    }
    if let Some(e) = &cmd.native_emote {
        draw.effects.flags[3] = 1.0;
        for (i, c) in e.corner_colors.iter().enumerate() {
            draw.effects.corners[i * 4..i * 4 + 4].copy_from_slice(c);
        }
        draw.effects.uv_rect = e.uv_rect;
        draw.effects.model_clip = e.clip_rect;
        draw.effects.wipe = [e.wipe[0], e.wipe[1], e.wipe[2], (e.blend_mode & 15) as f32];
        draw.effects.model_x[3] = u32::from((e.blend_mode & 0xf0) == 0x10) as f32;
    }
    Some(draw)
}
fn next_group(
    frame: &DrawList,
    start: usize,
    end: usize,
    limit: usize,
) -> Option<(usize, &ShaderGroup)> {
    frame
        .shader_groups
        .iter()
        .enumerate()
        .take(limit)
        .filter(|(_, g)| g.start == start && g.end > start && g.end <= end)
        .max_by_key(|(i, g)| (g.end, *i))
}
fn group_command(group: &ShaderGroup, width: u32, height: u32) -> DrawCommand {
    let blend = if group.effect.name == GROUP_COMPOSITE_SHADER {
        match scalar(&group.effect, "blendMode", 0.0) as i32 {
            1 => BlendMode::PremultipliedAdd,
            2 => BlendMode::Screen,
            3 => BlendMode::Multiply,
            _ => BlendMode::PremultipliedAlpha,
        }
    } else {
        BlendMode::PremultipliedAlpha
    };
    let size = TextureInfo { width, height };
    DrawCommand {
        texture: TextureId(0),
        size,
        transform: glam::Affine2::IDENTITY,
        opacity: 1.0,
        blend,
        color: ColorFilter::default(),
        clip: ClipRect::full(size),
        clip_bounds: group.clip_bounds,
        shader: Some(group.effect.clone()),
        mesh: None,
        stencil: None,
        native_emote: None,
    }
}
fn full_stage_clip(clip: Option<[f32; 4]>, width: u32, height: u32) -> bool {
    clip.is_none_or(|r| r.iter().all(|v| v.is_finite()) && r[0] <= 0.0 && r[1] <= 0.0
        && r[0]+r[2] >= width as f32 && r[1]+r[3] >= height as f32)
}
// Reduced-resolution blur is restricted to one opaque image, never a text or
// character composite. Every pass must match a bundled Kawase source hash.
fn half_blur_chain(frame:&DrawList,chain:&[usize],inner:usize,width:u32,height:u32)->bool {
    if chain.len()<3{return false;}
    let g=&frame.shader_groups[chain[0]];
    if g.end!=g.start+1 || next_group(frame,g.start,g.end,inner).is_some(){return false;}
    let c=&frame.commands[g.start];
    if c.opacity!=1. || c.shader.is_some() || c.mesh.is_some() || c.stencil.is_some()
        || c.native_emote.is_some() || !matches!(c.blend,BlendMode::Alpha|BlendMode::PremultipliedAlpha)
        || !full_stage_clip(c.clip_bounds,width,height)
        // Use the already prepared opacity certificate, including the full image,
        // without rescanning pixels or inferring opacity from a resource path.
        || unsafe{art3m1s_gxm_texture_region_is_opaque(c.texture.0,0.,0.,1.,1.)==0}{return false;}
    chain.iter().all(|&i|{
        let g=&frame.shader_groups[i];
        g.mask_range.is_none() && full_stage_clip(g.clip_bounds,width,height)
            && super::external_effects::verified_kawase(&g.effect)
    })
}
// A reusable local stack may contain several transparent expression layers.
// Effects declare their coordinate contract; masks, destination blending and
// arbitrary shader programs are intentionally left on the ordinary path.
fn translated_chain(frame:&DrawList,chain:&[usize],inner:usize,width:u32,height:u32)->Option<Vec<u32>> {
    if chain.is_empty()||chain.len()>16{return None;}
    let g=&frame.shader_groups[chain[0]];
    if g.start>=g.end||g.end>frame.commands.len()||g.end-g.start>32
        ||next_group(frame,g.start,g.end,inner).is_some(){return None;}
    if frame.commands[g.start..g.end].iter().any(|c|c.shader.is_some()||c.mesh.is_some()
        ||c.stencil.is_some()||c.native_emote.is_some()||c.color.grayscale||c.color.negative
        ||!matches!(c.blend,BlendMode::Alpha|BlendMode::PremultipliedAlpha)
        ||!full_stage_clip(c.clip_bounds,width,height)){return None;}
    let kinds:Vec<_>=chain.iter().rev().map(|&i|{
        let g=&frame.shader_groups[i];
        if g.mask_range.is_some()||!full_stage_clip(g.clip_bounds,width,height){0}
        else{super::external_effects::translation_kind(&g.effect)}
    }).collect();
    if kinds.contains(&0)||(g.end==g.start+1&&kinds.as_slice()==[6]){return None;}
    Some(kinds)
}
fn opaque_cover(c: &DrawCommand, width: u32, height: u32) -> bool {
    if c.opacity != 1.0 || c.mesh.is_some() || c.stencil.is_some() || c.native_emote.is_some()
        || !full_stage_clip(c.clip_bounds,width,height) || c.shader.is_some()
        || !matches!(c.blend,BlendMode::Alpha|BlendMode::PremultipliedAlpha) { return false; }
    let m=c.transform.matrix2;let t=c.transform.translation;
    if m.x_axis.y!=0.0 || m.y_axis.x!=0.0 { return false; }
    let x=t.x+m.x_axis.x*c.clip.quad_size[0];let y=t.y+m.y_axis.y*c.clip.quad_size[1];
    if ![t.x,t.y,x,y].iter().all(|v|v.is_finite()) || t.x.min(x)>0.0 || t.y.min(y)>0.0
        || t.x.max(x)<width as f32 || t.y.max(y)<height as f32 || x==t.x || y==t.y {return false;}
    let uv=|px:f32,py:f32| (c.clip.uv_offset[0]+(px-t.x)/(x-t.x)*c.clip.uv_scale[0],
        c.clip.uv_offset[1]+(py-t.y)/(y-t.y)*c.clip.uv_scale[1]);
    let (u0,v0)=uv(0.,0.);let (u1,v1)=uv(width as f32,height as f32);
    unsafe {art3m1s_gxm_texture_region_is_opaque(c.texture.0,u0,v0,u1,v1)!=0}
}
// These names currently encode as the identity sprite program, not a Kawase
// filter. Elide only that existing fallback's redundant isolation; this is NOT
// blur support. Once encode() gains a real implementation, kind()!=0 disables
// this route. Masks and non-full clips stay on the conservative original path.
fn identity_blur_fallback(g:&ShaderGroup,width:u32,height:u32)->bool {
    matches!(g.effect.name.as_str(),"blur_k"|"blur_kx"|"blur_ky")
        && kind(Some(&g.effect))==0 && !super::external_effects::registered(&g.effect.name) && g.mask_range.is_none()
        && g.effect.mask_texture.is_none() && g.effect.user_texture.is_none()
        && full_stage_clip(g.clip_bounds,width,height)
}
// Only remove an isolation boundary when normal source-over composition is
// associative and the boundary makes no change to the resulting RGBA image.
// Never distribute a group opacity/filter/mask to its overlapping children.
fn passthrough_group(frame: &DrawList, index: usize, width: u32, height: u32) -> bool {
    passthrough_group_inner(frame,index,width,height,false)
}
fn passthrough_group_inner(frame: &DrawList, index: usize, width: u32, height: u32, rebuilding:bool) -> bool {
    let g=&frame.shader_groups[index];let e=&g.effect;
    let identity_blur=identity_blur_fallback(g,width,height);
    if !identity_blur && (e.name!=GROUP_COMPOSITE_SHADER || g.mask_range.is_some() || e.mask_texture.is_some()
        || !full_stage_clip(g.clip_bounds,width,height)
        || scalar(e,"alpha",1.0)!=1.0 || scalar(e,"grayscale",0.0)!=0.0
        || scalar(e,"negative",0.0)!=0.0 || scalar(e,"blendMode",0.0)!=0.0
        || e.uniforms.get("colorMultiply").is_some_and(|v|v.as_slice()!=[1.0,1.0,1.0])) { return false; }
    if frame.commands[g.start..g.end].iter().any(|c| c.blend!=BlendMode::Alpha
        || c.stencil.is_some() || c.native_emote.is_some()) { return false; }
    let nested=|i:usize,n:&ShaderGroup| i<index && n.start>=g.start && n.end<=g.end;
    // Screen depends on the destination. A certified, ungrouped opaque prefix
    // replaces that destination before the light is drawn, making it identical
    // inside and outside this neutral wrapper. Keep the Screen child isolated.
    let screen_prefix=frame.shader_groups.iter().enumerate().any(|(i,n)|nested(i,n)
        && n.effect.name==GROUP_COMPOSITE_SHADER && scalar(&n.effect,"blendMode",0.0)==2.0)
        .then(||frame.commands[g.start..g.end].iter().enumerate().find_map(|(offset,c)|{
            let pos=g.start+offset;
            (opaque_cover(c,width,height) && !frame.shader_groups.iter().enumerate()
                .any(|(i,n)|nested(i,n)&&n.start<=pos&&pos<n.end)).then_some(pos)
        })).flatten();
    if frame.shader_groups.iter().enumerate().any(|(i,n)| nested(i,n)
        && !(identity_blur_fallback(n,width,height) || n.effect.name==ALPHA_MASK_SHADER || n.effect.name==RULE_TRANS_SHADER
            // A source-verified gray pass keeps premultiplied source-over
            // pixels and still executes in its own isolation. A transparent,
            // neutral wrapper adds no operation. Only flatten during rebuild:
            // keep the outer retained result and its existing slot policy.
            || (rebuilding && (super::external_effects::premultiplied_gray(&n.effect)
                || super::external_effects::premultiplied_spatial_filter(&n.effect)))
            || (n.effect.name==GROUP_COMPOSITE_SHADER && (scalar(&n.effect,"blendMode",0.0)==0.0
                || (scalar(&n.effect,"blendMode",0.0)==2.0 && screen_prefix.is_some_and(|p|p<n.start)))))) { return false; }
    if identity_blur || scalar(e,"opaque",0.0)==0.0 { return true; }
    // Forced opacity is redundant only with a certified opaque, full-stage
    // source. A source inside nested groups is also valid only when every
    // enclosing boundary is independently proven neutral (strictly lower indices).
    frame.commands[g.start..g.end].iter().enumerate().any(|(offset,c)| {
        let pos=g.start+offset;
        opaque_cover(c,width,height) && frame.shader_groups.iter().enumerate()
            .filter(|(i,n)|nested(*i,n)&&n.start<=pos&&pos<n.end)
            .all(|(i,_)|passthrough_group(frame,i,width,height))
    })
}
fn fused_group(frame:&DrawList,index:usize,width:u32,height:u32)->Option<EffectDraw> {
    let g=&frame.shader_groups[index];
    if g.start>=g.end || g.end>frame.commands.len() || g.effect.name!=GROUP_COMPOSITE_SHADER || g.mask_range.is_some()
        || g.effect.mask_texture.is_some() || !full_stage_clip(g.clip_bounds,width,height)
        || frame.shader_groups[..index].iter().any(|n|n.start>=g.start&&n.end<=g.end) {return None;}
    // Transparent placeholders do not contribute to a source-over group.
    // Require the host's current pixel proof; IDs, size and file names are not evidence.
    let mut visible=None;
    for c in &frame.commands[g.start..g.end] {
        let empty=c.blend==BlendMode::Alpha && c.shader.is_none() && c.mesh.is_none()
            && c.stencil.is_none() && c.native_emote.is_none()
            && unsafe {art3m1s_gxm_texture_is_empty(c.texture.0)!=0};
        if empty {continue;}
        if visible.replace(c).is_some(){return None;}
    }
    let c=visible?;
    single_group_draw(g,c,width,height)
}
fn single_group_draw(g:&ShaderGroup,c:&DrawCommand,width:u32,height:u32)->Option<EffectDraw>{
    if c.blend!=BlendMode::Alpha || c.mesh.is_some() || c.shader.is_some()
        || c.stencil.is_some() || c.native_emote.is_some() || c.color.grayscale || c.color.negative {return None;}
    let mut draw=encode(c,width,height)?;
    let group=encode(&group_command(g,width,height),width,height)?;
    if group.effects.transition[2]!=0.0 {
        // An opaque target also paints black outside the sprite. Fuse only
        // when this sprite covers the complete target; texture alpha may vary.
        let m=c.transform.matrix2;let t=c.transform.translation;
        let x=t.x+m.x_axis.x*c.clip.quad_size[0];let y=t.y+m.y_axis.y*c.clip.quad_size[1];
        if m.x_axis.y!=0.0 || m.y_axis.x!=0.0 || !full_stage_clip(c.clip_bounds,width,height)
            || ![t.x,t.y,x,y].iter().all(|v|v.is_finite()) || t.x.min(x)>0.0 || t.y.min(y)>0.0
            || t.x.max(x)<width as f32 || t.y.max(y)<height as f32 {return None;}
    }
    let source_tint=draw.tint;draw.tint=group.tint;draw.effects=group.effects;
    draw.effects.corners[..4].copy_from_slice(&source_tint);
    draw.effects.flags[0]=4.0;draw.effects.transition[3]=source_tint[3];draw.blend=group.blend;
    Some(draw)
}
// A small opaque base only changes the pixels it covers. Fuse the full-cover
// image elsewhere, then restore that rectangle with the original source-over
// draws. No group filter/opacity may be distributed over the overlapping pair.
fn local_opaque_group(frame:&DrawList,index:usize,width:u32,height:u32)->Option<[EffectDraw;3]>{
    if unsafe {art3m1s_gxm_local_base_enabled()==0}{return None;}
    let g=frame.shader_groups.get(index)?;let e=&g.effect;
    if g.end!=g.start+2 || g.end>frame.commands.len() || e.name!=GROUP_COMPOSITE_SHADER
        || g.mask_range.is_some() || e.mask_texture.is_some() || e.user_texture.is_some()
        || !full_stage_clip(g.clip_bounds,width,height)
        || scalar(e,"opaque",0.)!=1. || scalar(e,"alpha",1.)!=1.
        || scalar(e,"grayscale",0.)!=0. || scalar(e,"negative",0.)!=0. || scalar(e,"blendMode",0.)!=0.
        || e.uniforms.get("colorMultiply").is_some_and(|v|v.as_slice()!=[1.,1.,1.])
        || frame.shader_groups[..index].iter().any(|n|n.start>=g.start&&n.end<=g.end){return None;}
    let base=&frame.commands[g.start];let image=&frame.commands[g.start+1];
    if base.opacity!=1. || base.blend!=BlendMode::Alpha || base.shader.is_some()
        || base.mesh.is_some() || base.stencil.is_some() || base.native_emote.is_some()
        || base.color!=ColorFilter::default() || !full_stage_clip(base.clip_bounds,width,height)
        || unsafe {art3m1s_gxm_texture_is_opaque(base.texture.0)==0}{return None;}
    let m=base.transform.matrix2;let t=base.transform.translation;
    let w=m.x_axis.x*base.clip.quad_size[0];let h=m.y_axis.y*base.clip.quad_size[1];
    if m.x_axis.y!=0. || m.y_axis.x!=0. || ![t.x,t.y,w,h].iter().all(|x|x.is_finite())
        || t.x<0. || t.y<0. || w<=0. || h<=0. || t.x+w>width as f32 || t.y+h>height as f32
        || w*h>(width as f32*height as f32)/16. {return None;}
    let fused=single_group_draw(g,image,width,height)?;
    let base_draw=encode(base,width,height)?;
    let mut correction=image.clone();correction.clip_bounds=Some([t.x,t.y,w,h]);
    let im=image.transform;let iw=im.matrix2.x_axis.x*image.clip.quad_size[0];
    let ih=im.matrix2.y_axis.y*image.clip.quad_size[1];
    correction.transform=base.transform;correction.clip.quad_size=base.clip.quad_size;
    correction.clip.uv_offset=[image.clip.uv_offset[0]+(t.x-im.translation.x)/iw*image.clip.uv_scale[0],
        image.clip.uv_offset[1]+(t.y-im.translation.y)/ih*image.clip.uv_scale[1]];
    correction.clip.uv_scale=[w/iw*image.clip.uv_scale[0],h/ih*image.clip.uv_scale[1]];
    Some([fused,base_draw,encode(&correction,width,height)?])
}
fn render_range(frame: &DrawList, start: usize, end: usize, limit: usize, width: u32, height: u32, stats: &mut [u32;2], allow_flatten: bool, nodes:&mut NodeCache, admit:bool) {
    let mut index = start;
    while index < end {
        if let Some((group_index, group)) = next_group(frame, index, end, limit) {
            stats[0]+=1;
            // A proven neutral boundary needs no composite shader at all.
            // This must precede fusion, matching the retained-cache exclusion.
            if allow_flatten && passthrough_group_inner(frame,group_index,width,height,true) {
                stats[1]+=1;
                render_range(frame,group.start,group.end,group_index,width,height,stats,allow_flatten,nodes,admit);
                index=group.end;continue;
            }
            if allow_flatten && let Some(draws)=local_opaque_group(frame,group_index,width,height){
                stats[1]+=1;for draw in &draws {unsafe {art3m1s_gxm_draw_effect(draw)}}
                index=group.end;continue;
            }
            if allow_flatten && let Some(draw)=fused_group(frame,group_index,width,height) {
                stats[1]+=1;unsafe {art3m1s_gxm_draw_effect(&draw)};index=group.end;continue;
            }
            // Cache an existing isolation node's INPUT. It already contains
            // completed child filters; its own filter/blend remains unchanged.
            // This works without recognizing shader names or baking arbitrary
            // shader output into a different numerical format.
            if allow_flatten && unsafe{art3m1s_gxm_node_source_enabled()!=0} && node_cache::eligible(frame,group_index) {
                if let Some(draw)=encode(&group_command(group,width,height),width,height) {
                    if let Some(slot)=nodes.find(frame,group_index,(width,height)) {
                        if unsafe{art3m1s_gxm_node_source_draw(&draw,slot as u32)!=0} {
                            nodes.use_slot(slot);index=group.end;continue;
                        }
                        nodes.invalidate(slot);
                    }
                    nodes.trace_miss(frame,group_index,(width,height));
                    let ready=nodes.ready_to_build(frame,group_index,(width,height));
                    if admit && !ready {nodes.observe_change(frame,group_index,(width,height));}
                    if admit && ready && let Some(slot)=nodes.reserve(frame,group_index) {
                        let explore=nodes.seen_changed(frame,group_index);
                        if unsafe{art3m1s_gxm_group_begin()!=0} {
                            render_range(frame,group.start,group.end,group_index,width,height,stats,allow_flatten,nodes,explore);
                            let valid=unsafe{art3m1s_gxm_node_source_end(&draw,slot as u32)!=0};
                            nodes.store(slot,frame,group_index,(width,height),valid);
                            index=group.end;continue;
                        }
                        nodes.invalidate(slot);
                    }
                }
            }
            let Some(draw) = encode(&group_command(group, width, height), width, height) else {
                index = group.end;
                continue;
            };
            // Consecutive same-range custom wrappers are unary passes. Run
            // inner-to-outer through one target + scratch, preserving each pass,
            // clip and constants without exceeding the host's nesting depth.
            let mut chain=vec![group_index];
            let mut inner=group_index;
            if draw.custom.program!=0 && group.mask_range.is_none(){
                while let Some((i,g))=next_group(frame,group.start,group.end,inner){
                    if g.start!=group.start||g.end!=group.end||g.mask_range.is_some(){break;}
                    // Script-generated intermediate layers sit between the
                    // unary passes. Cross only independently proven neutral
                    // boundaries, never an opacity, mask or blend operation.
                    if allow_flatten && passthrough_group_inner(frame,i,width,height,true) {
                        stats[0]+=1;stats[1]+=1;inner=i;continue;
                    }
                    if !super::external_effects::registered(&g.effect.name){break;}
                    // Keep a coordinate-dependent outer pass separate so its
                    // stable, translation-invariant child can retain its result.
                    // Flattening across this boundary would consume the entire
                    // chain before the child gets a chance to use its cache.
                    if (super::external_effects::translation_kind(&group.effect)!=0)
                        !=(super::external_effects::translation_kind(&g.effect)!=0) {break;}
                    chain.push(i);inner=i;
                }
            }
            let half_passes=half_blur_chain(frame,&chain,inner,width,height).then(||
                chain.iter().rev().filter_map(|&i|
                    encode(&group_command(&frame.shader_groups[i],width,height),width,height)).collect::<Vec<_>>());
            if allow_flatten && let Some(kinds)=translated_chain(frame,&chain,inner,width,height) {
                let passes:Vec<_>=chain.iter().rev().filter_map(|&i|
                    encode(&group_command(&frame.shader_groups[i],width,height),width,height)).collect();
                let sources:Vec<_>=frame.commands[group.start..group.end].iter().filter_map(|c|encode(c,width,height)).collect();
                if passes.len()==chain.len()&&sources.len()==group.end-group.start&&unsafe{
                    art3m1s_gxm_draw_cached_effect(sources.as_ptr(),sources.len() as u32,passes.as_ptr(),kinds.as_ptr(),passes.len() as u32,
                        super::external_effects::revision(),u32::from(half_passes.is_some()))!=0
                } {
                    stats[0]+=(chain.len()-1) as u32;
                    index=group.end;continue;
                }
            }
            if unsafe { art3m1s_gxm_group_begin() } != 0 {
                render_range(frame, group.start, group.end, inner, width, height, stats, allow_flatten,nodes,admit);
                if let Some(all_passes)=half_passes {
                    if all_passes.len()==chain.len() && unsafe{
                        art3m1s_gxm_group_end_half_blur(all_passes.as_ptr(),all_passes.len() as u32)!=0
                    } {
                        stats[0]+=(chain.len()-1) as u32;
                        index=group.end;continue;
                    }
                }
                let passes:Vec<_>=chain.iter().rev().take(chain.len()-1).filter_map(|&i|
                    encode(&group_command(&frame.shader_groups[i],width,height),width,height)).collect();
                if !passes.is_empty() {
                    if unsafe{art3m1s_gxm_group_filter_chain(passes.as_ptr(),passes.len() as u32)}==0 {
                        crate::core_warn!("GXM external filter target/pass failed");
                    }
                    stats[0]+=passes.len() as u32;
                }
                if let Some([start, end]) = group.mask_range {
                    if !nodes.masks.draw(frame.mask_commands.get(start..end).unwrap_or_default(), width, height) {
                        crate::core_warn!("GXM mask target allocation failed");
                    }
                }
                unsafe { art3m1s_gxm_group_end(&draw) };
            } else {
                crate::core_warn!("GXM group target allocation failed: {}", group.effect.name);
                render_range(frame, group.start, group.end, group_index, width, height, stats, allow_flatten,nodes,admit);
            }
            index = group.end;
        } else {
            if let Some(draw) = encode(&frame.commands[index], width, height) {
                unsafe { art3m1s_gxm_draw_effect(&draw) };
            }
            index += 1;
        }
    }
}
mod input_snapshot;
mod mask_reuse;
use input_snapshot::InputSnapshot;
mod node_cache;
use node_cache::NodeCache;
#[derive(Default)]
pub(super) struct RetainedGroup {
    commands: Vec<DrawCommand>,
    groups: Vec<ShaderGroup>,
    masks: Vec<DrawCommand>,
    group: Option<ShaderGroup>,
    revision: u64,
    size: (u32,u32),
    baked: bool,
    input_cached: bool,
    textures: Vec<(u64,u64)>,
    changing_frames: u32,
    stable_frames: u32,
    pool_revision:u64,
    animated_output:bool,
    programs:u64,
}
impl RetainedGroup {
    fn matches(&self, frame:&DrawList, g:&ShaderGroup, size:(u32,u32), revision:u64)->bool {
        self.size==size && self.programs==super::external_effects::revision()
            && (self.revision==revision || self.textures.iter()
            .all(|&(id,stamp)|unsafe {art3m1s_gxm_texture_content_revision(id)==stamp}))
            && self.group.as_ref()==Some(g)
            && self.commands==frame.commands[g.start..g.end]
            && self.groups.iter().eq(frame.shader_groups.iter()
                .filter(|other| other.start<g.end && other.end>g.start))
            && self.masks==frame.mask_commands
    }
    fn same_identity(&self,g:&ShaderGroup)->bool {
        self.group.as_ref().is_some_and(|old|match (&old.key,&g.key) {
            (Some(a),Some(b))=>a==b,
            (None,None)=>old.start==g.start && old.end==g.end,
            _=>false,
        })
    }
    fn same_input(&self,frame:&DrawList,g:&ShaderGroup,size:(u32,u32),revision:u64)->bool {
        let Some(old)=self.group.as_ref() else{return false;};
        self.same_identity(g) && self.size==size && self.programs==super::external_effects::revision()
            && (self.revision==revision || self.textures.iter().all(|&(id,r)|unsafe{art3m1s_gxm_texture_content_revision(id)==r}))
            && self.commands==frame.commands[g.start..g.end] && self.masks==frame.mask_commands
            && self.groups.iter().filter(|n|*n==old).count()==1
            && self.groups.len()==frame.shader_groups.iter().filter(|n|n.start<g.end&&n.end>g.start).count()
            && self.groups.iter().zip(frame.shader_groups.iter().filter(|n|n.start<g.end&&n.end>g.start))
                .all(|(a,b)|a==b || (a==old && b==g))
    }
    fn store(&mut self,frame:&DrawList,g:&ShaderGroup,size:(u32,u32),revision:u64){
        self.commands.clear();self.commands.extend_from_slice(&frame.commands[g.start..g.end]);
        // Only overlapping group metadata can affect this command range. An
        // unrelated portrait's fade must not invalidate the background cache.
        // Keep all masks conservatively until mask-range dependency tracking exists.
        self.groups.clear();self.groups.extend(frame.shader_groups.iter()
            .filter(|other| other.start<g.end && other.end>g.start).cloned());
        self.masks.clone_from(&frame.mask_commands);
        self.textures.clear();
        let mut ids=Vec::new();
        for c in self.commands.iter().chain(self.masks.iter()) {
            ids.push(c.texture.0);
            if let Some(e)=&c.shader {
                ids.extend(e.mask_texture.into_iter().chain(e.user_texture).map(|t|t.0));
            }
        }
        for group in &self.groups {
            ids.extend(group.effect.mask_texture.into_iter().chain(group.effect.user_texture).map(|t|t.0));
        }
        ids.sort_unstable();ids.dedup();
        self.textures.extend(ids.into_iter().map(|id|
            (id,unsafe {art3m1s_gxm_texture_content_revision(id)})));
        self.group=Some(g.clone());self.size=size;self.revision=revision;
        self.programs=super::external_effects::revision();
    }
}
fn retainable_group(frame:&DrawList,index:usize,width:u32,height:u32)->Option<&ShaderGroup>{
    let g=frame.shader_groups.get(index)?;let e=&g.effect;
    let commands=frame.commands.get(g.start..g.end)?;
    // This is an existing isolated character group, not a new isolation
    // boundary. Exact command/mask/texture checks still invalidate every pose.
    let native_pose=!commands.is_empty() && commands.iter().all(|c|c.native_emote.is_some()&&c.shader.is_none())
        && unsafe{art3m1s_gxm_emote_composite_cache_enabled()!=0};
    if g.start>=g.end || g.end>frame.commands.len() || e.name!=GROUP_COMPOSITE_SHADER
        || super::stage_clip(g.clip_bounds,width,height).is_err()
        || !matches!(scalar(e,"blendMode",0.),0.0|2.0)
        || (!native_pose && commands.iter().any(|c|c.mesh.is_some()||c.native_emote.is_some()||c.stencil.is_some()))
        // Fused draws still run a nontrivial fragment program over the screen.
        // Let stable groups bake once; changing groups keep their direct route.
        || passthrough_group(frame,index,width,height){return None;}
    Some(g)
}
#[derive(Default)]
pub(super) struct RetainedGroups {
    slots:[RetainedGroup;4], overlay:RetainedGroup, nodes:NodeCache,
    clock:u64, touched:[u64;4], identity_blur_reported:bool,
    overlay_pressure:u32, overlay_blocked:bool, overlay_layout:Vec<(usize,usize)>,
    input_reports:u32,
}
impl RetainedGroups {
    fn select(&self, frame:&DrawList, g:&ShaderGroup, size:(u32,u32), revision:u64,
        used:&[bool;4], limit:usize)->Option<usize>{
        // Keep exact earlier states of a looping effect in the existing four
        // targets. Never overwrite a target already submitted in this frame.
        (0..limit).find(|&i|!used[i]&&!self.nodes.busy(i)&&self.slots[i].matches(frame,g,size,revision))
            .or_else(||(0..limit).filter(|&i|!used[i]&&!self.nodes.busy(i))
                .min_by_key(|&i|(self.slots[i].group.is_some(),self.touched[i])))
    }
}
// Only an ungrouped, normal-alpha tail can be isolated without distributing
// effects across layers. Texture stamps and exact commands invalidate the bake.
#[cfg(test)]
fn overlay_tail(frame:&DrawList,width:u32,height:u32)->Option<ShaderGroup>{
    overlay_span(frame,width,height,None)
}
fn overlay_span(frame:&DrawList,width:u32,height:u32,previous:Option<&RetainedGroup>)->Option<ShaderGroup>{
    let floor=frame.shader_groups.iter().map(|g|g.end).max().unwrap_or(0);
    let mut start=frame.commands.len();
    for i in (floor..frame.commands.len()).rev(){
        let c=&frame.commands[i];
        if c.blend!=BlendMode::Alpha||c.shader.is_some()||c.mesh.is_some()||c.stencil.is_some()||c.native_emote.is_some()
            ||c.color.grayscale||c.color.negative||!c.transform.is_finite(){break;}
        start=i;
    }
    let mut end=frame.commands.len();
    if end-start<32{return None;}
    // A wait icon or one fading button must not invalidate all finished text.
    // Reuse only an exactly unchanged contiguous run inside the eligible tail.
    // Keep the original order on either side; never cross an effect boundary.
    if let Some(old)=previous.filter(|c|c.group.is_some()) {
        let g=old.group.as_ref().unwrap();
        let (lo,hi)=(start.max(g.start),end.min(g.end));
        let (mut run,mut best)=(lo,lo..lo);
        for i in lo..hi {
            if old.commands.get(i-g.start)==Some(&frame.commands[i]) {
                if i+1-run>best.len(){best=run..i+1;}
            }else{run=i+1;}
        }
        if best.len()>=32 {start=best.start;end=best.end;}
    }
    let mut bounds=[width as f32,height as f32,0f32,0f32];
    for c in &frame.commands[start..end] {
        let [w,h]=c.clip.quad_size;
        if !w.is_finite()||!h.is_finite(){return None;}
        for p in [glam::Vec2::ZERO,glam::vec2(w,0.),glam::vec2(0.,h),glam::vec2(w,h)]{
            let p=c.transform.transform_point2(p);
            if !p.is_finite(){return None;}
            bounds[0]=bounds[0].min(p.x);bounds[1]=bounds[1].min(p.y);
            bounds[2]=bounds[2].max(p.x);bounds[3]=bounds[3].max(p.y);
        }
    }
    bounds[0]=bounds[0].max(0.);bounds[1]=bounds[1].max(0.);
    bounds[2]=bounds[2].min(width as f32)-bounds[0];bounds[3]=bounds[3].min(height as f32)-bounds[1];
    if bounds[2]<=0.||bounds[3]<=0.{return None;}
    Some(ShaderGroup{key:None,start,end,effect:ShaderEffect{name:GROUP_COMPOSITE_SHADER.into(),uniforms:Default::default(),mask_texture:None,user_texture:None},clip_bounds:Some(bounds),mask_range:None})
}
pub(super) fn render_cached(frame: &DrawList, width: u32, height: u32, caches:&mut RetainedGroups) {
    unsafe { super::art3m1s_gxm_frame_begin(width, height) };
    let mut stats=[0,0];
    if !caches.identity_blur_reported && frame.shader_groups.iter().any(|g|identity_blur_fallback(g,width,height)) {
        caches.identity_blur_reported=true;
        crate::core_warn!("GXM blur_k fallback is identity, not Kawase; eliding neutral offscreen copies only");
    }
    let enabled=unsafe {art3m1s_gxm_group_passthrough_enabled()!=0};
    caches.nodes.begin();caches.nodes.protect_inputs(frame,(width,height));
    // Final pictures remain useful across returning to earlier scene states.
    // Input caches must not silently replace their physical slots.
    for (slot,cache) in caches.slots.iter().enumerate() {
        if (cache.baked || cache.input_cached) && cache.pool_revision==unsafe{art3m1s_gxm_cache_slot_revision(slot as u32)} {
            caches.nodes.claim_final(slot);
        }
    }
    let candidate=if enabled && unsafe{art3m1s_gxm_overlay_cache_enabled()!=0}{overlay_span(frame,width,height,Some(&caches.overlay))}else{None};
    if !caches.overlay_layout.iter().copied().eq(frame.shader_groups.iter().map(|g|(g.start,g.end))) || candidate.is_none(){
        caches.overlay_layout.clear();caches.overlay_layout.extend(frame.shader_groups.iter().map(|g|(g.start,g.end)));
        caches.overlay_pressure=0;caches.overlay_blocked=false;
    }
    let overlay=if caches.overlay_blocked||caches.nodes.protected(3){None}else{candidate};
    if overlay.is_some(){caches.nodes.use_slot(3);}
    let overlay_start=overlay.as_ref().map_or(frame.commands.len(),|g|g.start);
    let mut index=0;let mut used=[false;4];
    caches.clock=caches.clock.saturating_add(1);
    let limit=if overlay.is_some(){3}else{4};
    // The overlay and group pool share physical slot 3, never its metadata.
    if overlay.is_some()||caches.overlay.group.is_some(){caches.slots[3].group=None;caches.slots[3].baked=false;caches.slots[3].input_cached=false;}
    // Descend through proven neutral boundaries without leaving the cache-aware
    // traversal. Non-neutral ancestors still use the original isolated path.
    // Each scope bounds both commands and group indices, including equal ranges.
    let mut scopes=vec![(overlay_start,frame.shader_groups.len())];
    while index<overlay_start{
    while scopes.len()>1 && index>=scopes.last().unwrap().0 {scopes.pop();}
    let &(scope_end,scope_limit)=scopes.last().unwrap();
    if let Some((gi,g))=next_group(frame,index,scope_end,scope_limit){
    if enabled && passthrough_group(frame,gi,width,height){
        stats[0]+=1;stats[1]+=1;
        scopes.push((g.end,gi));continue;
    }
    let revision=unsafe {art3m1s_gxm_texture_revision()};
    // Varying only a node's own output parameters is an animation, not a
    // different reusable scene. Drop obsolete final snapshots of this node so
    // they cannot starve its stable nested inputs. Changed images stay distinct.
    let animated_output=enabled && caches.slots.iter().any(|c|c.same_input(frame,g,(width,height),revision)
        && c.group.as_ref()!=Some(g));
    if animated_output {
        for slot in 0..limit {
            if !used[slot] && !caches.nodes.busy(slot) && caches.slots[slot].same_identity(g) {
                caches.slots[slot].baked=false;
                caches.slots[slot].input_cached=false;
                caches.slots[slot].animated_output=true;
                if caches.slots[slot].group.as_ref()!=Some(g) {caches.slots[slot].group=None;}
                caches.nodes.release_final(slot);
            }
        }
    }
    let selected=if enabled&&retainable_group(frame,gi,width,height).is_some(){
        caches.select(frame,g,(width,height),revision,&used,limit)
    }else{None};
    if let Some(slot)=selected {
        used[slot]=true;caches.touched[slot]=caches.clock;
        caches.nodes.claim_final(slot);
        let cache=&mut caches.slots[slot];
        let same=cache.matches(frame,g,(width,height),revision);
        if !same {cache.animated_output=animated_output;}
        if overlay.is_some()&&!same&&cache.baked {
            caches.overlay_pressure=caches.overlay_pressure.saturating_add(1);
            if caches.overlay_pressure>=6&&!caches.overlay_blocked {
                caches.overlay_blocked=true;
                crate::core_info!("GXM overlay-yield slots=4 baked_replacements={} groups={}; freeing slot3 next frame until layout changes",caches.overlay_pressure,frame.shader_groups.len());
            }
        }
        cache.changing_frames=if same {0}else{cache.changing_frames.saturating_add(1)};
        cache.stable_frames=if same {cache.stable_frames.saturating_add(1)}else{0};
        if cache.changing_frames==8 {
            crate::core_info!("GXM moving-group slot={} stage={}x{} range={}..{} clip={:?} mask_range={:?} effect={:?}",
                slot,width,height,g.start,g.end,g.clip_bounds,g.mask_range,g.effect);
            for (offset,c) in frame.commands[g.start..g.end].iter().enumerate().take(8) {
                crate::core_info!("GXM moving-child slot={} offset={} opaque_cover={} command={:?}",
                    slot,offset,opaque_cover(c,width,height),c);
            }
        }
        if same {cache.revision=revision;}
        let hit=same && cache.baked && cache.pool_revision==unsafe{art3m1s_gxm_cache_slot_revision(slot as u32)}
            && unsafe {art3m1s_gxm_draw_cached_group(slot as u32)!=0};
        if hit {caches.nodes.use_slot(slot);}
        if !same {
            // A changing group must not pay an extra cache-baking pass. Observe
            // one identical subsequent frame before building a retained result.
            // Keep the already isolated input in this group's reserved slot.
            // Swapping ownership adds no pixel copy. A flattened wrapper with
            // many child passes may add one outer pass now to avoid rebuilding
            // all children next frame. Keep simple flattened/fused paths and
            // the dedicated policy for animated mosaic/output unchanged.
            cache.input_cached=false;
            let flattened=passthrough_group_inner(frame,gi,width,height,true);
            let complex_flattened=flattened && frame.shader_groups[..gi].iter().enumerate().filter(|(i,n)|
                n.start>=g.start && n.end<=g.end && n.start<n.end
                && !passthrough_group_inner(frame,*i,width,height,true)
                && fused_group(frame,*i,width,height).is_none()
                && local_opaque_group(frame,*i,width,height).is_none()).take(4).count()>=4;
            let capture = !cache.animated_output
                && g.mask_range.is_none() && unsafe{art3m1s_gxm_group_input_reuse_enabled()!=0}
                && (!flattened || (complex_flattened && cache.changing_frames==1))
                && local_opaque_group(frame,gi,width,height).is_none()
                && fused_group(frame,gi,width,height).is_none()
                && !frame.shader_groups[..=gi].iter().any(|n|n.start>=g.start && n.end<=g.end
                    && super::external_effects::cacheable_mosaic(&n.effect));
            if g.end-g.start>=8 && caches.input_reports<24 {
                caches.input_reports+=1;
                crate::core_info!("GXM retained-input slot={} commands={} groups={} flattened={} complex={} capture={} changes={}",
                    slot,g.end-g.start,gi+1,flattened,complex_flattened,capture,cache.changing_frames);
            }
            if capture && let Some(draw)=encode(&group_command(g,width,height),width,height)
                && unsafe{art3m1s_gxm_group_begin()!=0} {
                caches.nodes.use_slot(slot);
                render_range(frame,g.start,g.end,gi,width,height,&mut stats,enabled,&mut caches.nodes,true);
                cache.input_cached=unsafe{art3m1s_gxm_node_source_end(&draw,slot as u32)!=0};
                cache.pool_revision=unsafe{art3m1s_gxm_cache_slot_revision(slot as u32)};
                stats[0]+=1;
            }else{
                // No final image is submitted or captured in this branch.
                // Let stable child inputs use this slot immediately instead of
                // reserving empty storage throughout an animated transition.
                caches.nodes.release_final(slot);
                render_range(frame,g.start,g.end,gi+1,width,height,&mut stats,enabled,&mut caches.nodes,true);
            }
            cache.store(frame,g,(width,height),revision);cache.baked=false;
        }else if !hit && cache.stable_frames<24 && (cache.animated_output || frame.shader_groups.iter().take(gi+1).any(|n|
            n.start>=g.start && n.end<=g.end && super::external_effects::cacheable_mosaic(&n.effect))) {
            // Scripted mosaic holds each grid for only a few frames. Baking
            // a final target on the second frame adds another full GPU fence,
            // only to discard it at the next step. The same applies to any
            // observed output-only animation. Other settled scenes bake early.
            if !cache.input_cached {caches.nodes.release_final(slot);}
            render_range(frame,g.start,g.end,gi+1,width,height,&mut stats,enabled,&mut caches.nodes,true);
        }else if !hit {
            caches.nodes.use_slot(slot);
            let draw=encode(&group_command(g,width,height),width,height);
            let source=if cache.input_cached && cache.pool_revision==unsafe{art3m1s_gxm_cache_slot_revision(slot as u32)} {
                Some(slot)
            }else{caches.nodes.find(frame,gi,(width,height))};
            let restored=draw.is_some() && source.is_some_and(|source|{
                caches.nodes.use_slot(source);
                let ok=unsafe{art3m1s_gxm_group_begin_cached_input(source as u32)!=0};
                caches.nodes.invalidate(source);ok
            });
            cache.input_cached=false;
            if let Some(draw)=draw
                && (restored || unsafe {art3m1s_gxm_group_begin()!=0}) {
                if !restored {render_range(frame,g.start,g.end,gi,width,height,&mut stats,enabled,&mut caches.nodes,true);}
                let mask_ready = if let Some([start, end]) = g.mask_range {
                    caches.nodes.masks.draw(frame.mask_commands.get(start..end).unwrap_or_default(), width, height)
                } else { true };
                if !mask_ready {
                    crate::core_warn!("GXM retained mask target allocation failed");
                    unsafe { art3m1s_gxm_group_end(&draw) };
                    cache.group = None;
                } else if unsafe {art3m1s_gxm_group_end_cached(&draw,slot as u32)!=0}{
                    cache.store(frame,g,(width,height),revision);
                    cache.baked=true;cache.pool_revision=unsafe{art3m1s_gxm_cache_slot_revision(slot as u32)};
                }else{cache.group=None;}
            }else{
                cache.group=None;
                render_range(frame,g.start,g.end,gi,width,height,&mut stats,enabled,&mut caches.nodes,true);
            }
        }
        if same {stats[0]+=1;}
    }else{
        render_range(frame,g.start,g.end,gi+1,width,height,&mut stats,enabled,&mut caches.nodes,true);
    }
    index=g.end;
    }else{
        render_range(frame,index,index+1,0,width,height,&mut stats,enabled,&mut caches.nodes,true);index+=1;
    }
    }
    if let Some(g)=overlay{
        caches.slots[3].group=None;caches.slots[3].baked=false;caches.slots[3].input_cached=false;
        let revision=unsafe{art3m1s_gxm_texture_revision()};
        let cache=&mut caches.overlay;
        let same=cache.matches(frame,&g,(width,height),revision);
        cache.changing_frames=if same{cache.changing_frames.saturating_add(1)}else{0};
        let hit=same&&cache.baked&&cache.pool_revision==unsafe{art3m1s_gxm_cache_slot_revision(3)}&&unsafe{art3m1s_gxm_draw_cached_group(3)!=0};
        if !hit{
            let bake=same&&cache.changing_frames>=8&&unsafe{art3m1s_gxm_group_begin()!=0};
            render_range(frame,g.start,g.end,0,width,height,&mut stats,enabled,&mut caches.nodes,true);
            cache.baked=bake&&unsafe{art3m1s_gxm_overlay_end_cached(3,g.clip_bounds.unwrap().as_ptr())!=0};
            cache.pool_revision=unsafe{art3m1s_gxm_cache_slot_revision(3)};
            cache.store(frame,&g,(width,height),revision);
        }
        render_range(frame,g.end,frame.commands.len(),0,width,height,&mut stats,enabled,&mut caches.nodes,true);
    }else{caches.overlay.group=None;caches.overlay.baked=false;}
    // Keep unused versions until LRU replacement; exact dependencies guard reuse.
    unsafe { art3m1s_gxm_report_groups(stats[0],stats[1]); }
    unsafe { super::art3m1s_gxm_frame_end() };
}

#[cfg(test)]
fn render(frame:&DrawList,width:u32,height:u32){render_cached(frame,width,height,&mut RetainedGroups::default());}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    thread_local! { static EVENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) }; }
    thread_local! { static DRAW_KINDS: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) }; }
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_texture_is_opaque(id:u64)->i32 {i32::from(id==42)}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_texture_is_empty(id:u64)->i32 {i32::from(id==4041)}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_report_groups(_:u32,_:u32) {}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_group_passthrough_enabled()->i32 {1}
    #[unsafe(no_mangle)] extern "C" fn art3m1s_gxm_overlay_cache_enabled()->i32 {1}
    #[unsafe(no_mangle)] extern "C" fn art3m1s_gxm_overlay_end_cached(_:u32,_:*const f32)->i32 {bump_pool(3);event("overlay-bake".into());1}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_local_base_enabled()->i32 {1}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_texture_revision()->u64 {CHANGED_TEXTURE.with(|c|if c.get()==0{1}else{2})}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_texture_region_is_opaque(id:u64,u0:f32,v0:f32,u1:f32,v1:f32)->i32 {
        if id==45 {return i32::from(u0.min(u1)>=0. && u0.max(u1)<=1. && v0.min(v1)>=0. && v0.max(v1)<=1.);}
        if id==44 {return i32::from(u0.min(u1)>=0.24&&u0.max(u1)<=0.76&&v0.min(v1)>=0.24&&v0.max(v1)<=0.76);}
        art3m1s_gxm_texture_is_opaque(id)
    }
    thread_local! { static CHANGED_TEXTURE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_texture_content_revision(id:u64)->u64 {
        CHANGED_TEXTURE.with(|c|if c.get()==id {2}else{1})
    }
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_draw_cached_group(_:u32)->i32 {event("cached".into());1}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_group_end_cached(_: *const EffectDraw,slot:u32)->i32 {bump_pool(slot as usize);event("bake".into());1}
    #[unsafe(no_mangle)] extern "C" fn art3m1s_gxm_group_input_reuse_enabled()->i32 {1}
    thread_local! { static RESTORE_FAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    #[unsafe(no_mangle)] extern "C" fn art3m1s_gxm_group_begin_cached_input(slot:u32)->i32 {
        bump_pool(slot as usize);
        if RESTORE_FAIL.with(|v|v.get()) {return 0;}
        event("restore-input".into());1
    }
    #[unsafe(no_mangle)] extern "C" fn art3m1s_gxm_node_source_draw(_: *const EffectDraw,_:u32)->i32 {event("node-source-hit".into());1}
    #[unsafe(no_mangle)] extern "C" fn art3m1s_gxm_node_source_enabled()->i32 {1}
    #[unsafe(no_mangle)] extern "C" fn art3m1s_gxm_cache_slot_revision(slot:u32)->u64 {POOL_SERIAL.with(|v|v.borrow()[slot as usize])}
    #[unsafe(no_mangle)] extern "C" fn art3m1s_gxm_node_source_end(_: *const EffectDraw,slot:u32)->i32 {bump_pool(slot as usize);event("node-source-build".into());1}
    thread_local! { static POOL_SERIAL: RefCell<[u64;5]> = const { RefCell::new([1;5]) }; }
    fn bump_pool(slot:usize){POOL_SERIAL.with(|v|v.borrow_mut()[slot]+=1);}
    fn event(s: String) {
        EVENTS.with(|v| v.borrow_mut().push(s));
    }
    fn mosaic_frame()->DrawList {
        use super::super::external_effects as ex;
        let src=b"float size; float ratio; void vs(float4 position:POSITION){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(float2 texCoord0:TEXCOORD0,float2 texCoord1:TEXCOORD1,out float4 result:COLOR0){result=float4(size,ratio,0,1);}";
        ex::register_source("verified-mosaic-test",src).unwrap();
        ex::mark_test_builtin_mosaic("verified-mosaic-test");
        let mut f=neutral_frame();let mut outer=f.shader_groups[0].clone();
        outer.effect.name="verified-mosaic-test".into();
        outer.effect.uniforms=[("size".into(),vec![20.]),("ratio".into(),vec![960./544.])].into();
        f.shader_groups.push(outer);f
    }
    #[test]
    fn mosaic_reuses_input_when_only_grid_or_output_clip_changes() {
        let mut f=mosaic_frame();let mut src=NodeCache::default();let mut stats=[0,0];
        render_range(&f,0,2,2,960,544,&mut stats,true,&mut src,true);
        src.begin();
        f.shader_groups[1].effect.uniforms.insert("size".into(),vec![80.]);
        f.shader_groups[1].clip_bounds=Some([10.,20.,400.,200.]);
        EVENTS.with(|v|v.borrow_mut().clear());
        render_range(&f,0,2,2,960,544,&mut stats,true,&mut src,true);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["node-source-hit"]));
        f.commands[0].transform.translation.x+=1.;
        src.begin();
        EVENTS.with(|v|v.borrow_mut().clear());
        render_range(&f,0,2,2,960,544,&mut stats,true,&mut src,true);
        EVENTS.with(|v|assert!(!v.borrow().iter().any(|s|s=="node-source-hit"||s=="node-source-build")));
        src.begin();render_range(&f,0,2,2,960,544,&mut stats,true,&mut src,true);
        EVENTS.with(|v|assert!(v.borrow().iter().any(|s|s=="node-source-build")));
    }
    #[test]
    fn output_animation_releases_old_final_states_and_settles_back_to_a_cached_image() {
        let mut f=neutral_frame();let mut caches=RetainedGroups::default();
        f.shader_groups[0].effect.uniforms.insert("alpha".into(),vec![0.8]);
        for _ in 0..3 {render_cached(&f,960,544,&mut caches);}
        for alpha in [0.7,0.6,0.5] {
            f.shader_groups[0].effect.uniforms.insert("alpha".into(),vec![alpha]);
            EVENTS.with(|v|v.borrow_mut().clear());
            for _ in 0..3 {render_cached(&f,960,544,&mut caches);}
            EVENTS.with(|v|assert!(!v.borrow().iter().any(|s|s=="bake")));
        }
        for _ in 0..30 {render_cached(&f,960,544,&mut caches);}
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","end-frame"]));
        // A new image/position is a different scene, not an output-only animation.
        f.commands[0].transform.translation.x+=1.;
        EVENTS.with(|v|v.borrow_mut().clear());
        for _ in 0..3 {render_cached(&f,960,544,&mut caches);}
        EVENTS.with(|v|assert!(v.borrow().iter().any(|s|s=="bake")));
    }
    #[test]
    fn nested_filter_animation_keeps_static_blur_inputs_under_final_cache_pressure() {
        use super::super::external_effects as ex;
        let mut frame=mosaic_frame();let mosaic=frame.shader_groups[1].clone();
        let src=b"float alpha; void vs(float4 position:POSITION){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(float2 texCoord0:TEXCOORD0,float2 texCoord1:TEXCOORD1,out float4 result:COLOR0){result=float4(alpha,0,0,1);}";
        for name in ["test-blur-h","test-blur-v"] {ex::register_source(name,src).unwrap();ex::mark_test_builtin_blur(name);}
        frame.commands=frame.commands.iter().cloned().cycle().take(6).collect();frame.shader_groups.clear();
        for i in 0..3 {
            for name in ["test-blur-h","test-blur-v",GROUP_COMPOSITE_SHADER] {
                let mut g=test_group(name,i*2,i*2+2);
                if name!=GROUP_COMPOSITE_SHADER {g.effect.uniforms.insert("weights".into(),vec![0.1;8]);}
                frame.shader_groups.push(g);
            }
        }
        let mut mosaic=mosaic;mosaic.end=6;frame.shader_groups.push(mosaic);
        frame.shader_groups.push(test_group(GROUP_COMPOSITE_SHADER,0,6));
        for (i,g) in frame.shader_groups.iter_mut().enumerate() {
            g.key=Some(crate::render_pipeline::draw::ShaderGroupKey::Layer {
                layer_id:format!("filter{i}"),kind:crate::render_pipeline::draw::LayerShaderGroupKind::Intermediate,
            });
        }
        let mut caches=RetainedGroups::default();
        let mut previous=frame.clone();previous.shader_groups.remove(9);
        // Old scene results occupy all four final targets before the transition.
        for i in 0..4 {previous.commands[0].transform.translation.x=i as f32;
            let g=previous.shader_groups.last().unwrap();
            caches.slots[i].store(&previous,g,(960,544),1);caches.slots[i].baked=true;
            caches.slots[i].pool_revision=unsafe{art3m1s_gxm_cache_slot_revision(i as u32)};
        }
        for step in 0..8 {
            frame.shader_groups[9].effect.uniforms.insert("size".into(),vec![20.+step as f32*10.]);
            // A separate group's output fades while its expensive child passes stay fixed.
            frame.shader_groups[8].effect.uniforms.insert("alpha".into(),vec![0.2+step as f32*0.1]);
            EVENTS.with(|v|v.borrow_mut().clear());render_cached(&frame,960,544,&mut caches);
            let events=EVENTS.with(|v|v.borrow().clone());
            if step>=2 {assert!(!events.iter().any(|e|e.starts_with("draw:")),"step {step}: {events:?}");}
        }
        // Borrowing storage must never reuse a stale face or altered geometry.
        CHANGED_TEXTURE.with(|v|v.set(43));
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&frame,960,544,&mut caches);
        EVENTS.with(|v|assert!(v.borrow().iter().any(|e|e=="draw:43")));
        CHANGED_TEXTURE.with(|v|v.set(0));
        frame.commands[0].transform.translation.x+=10.;
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&frame,960,544,&mut caches);
        EVENTS.with(|v|assert!(v.borrow().iter().any(|e|e=="draw:42")));
        // A settled scene still acquires a final picture and stops rerunning filters.
        for _ in 0..30 {render_cached(&frame,960,544,&mut caches);}
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&frame,960,544,&mut caches);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","end-frame"]));
    }
    #[test]
    fn input_cache_cannot_evict_live_or_reserved_final_composites() {
        let f=mosaic_frame();let mut cache=NodeCache::default();cache.begin();
        for slot in 0..4 {cache.claim_final(slot);}
        assert_eq!(cache.reserve(&f,1),Some(4));
        assert!(cache.reserve(&f,1).is_none());
        cache.begin(); // cleared/replaced final entries can be reclaimed next frame
        assert!(cache.reserve(&f,1).is_some());
    }
    #[test]
    fn neutral_multi_effect_wrapper_does_not_create_a_new_isolation() {
        let mut f=mosaic_frame();let mut wrapper=f.shader_groups[0].clone();
        wrapper.effect=f.shader_groups[1].effect.clone();f.shader_groups.push(wrapper);
        f.shader_groups.push(f.shader_groups[0].clone());
        assert!(node_cache::eligible(&f,3));
        assert!(passthrough_group_inner(&f,3,960,544,true));
        let mut cache=NodeCache::default();cache.begin();
        EVENTS.with(|v|v.borrow_mut().clear());
        let mut stats=[0,0];render_range(&f,0,2,4,960,544,&mut stats,true,&mut cache,true);
        assert!(stats[1]>0); // the neutral parent was flattened before admission
        // Only the actual outer mosaic input is isolated, not its parent.
        EVENTS.with(|v|assert_eq!(v.borrow().iter().filter(|e|e.as_str()=="node-source-build").count(),1));
    }
    #[test]
    fn generic_input_ignores_own_filter_but_tracks_child_filter_and_mask_pixels() {
        let mut f=mosaic_frame();let mut src=InputSnapshot::default();src.valid=true;
        src.store(&f,1,(960,544));
        f.shader_groups[1].effect.name="a-different-filter".into();
        f.shader_groups[1].effect.mask_texture=Some(TextureId(91));
        f.shader_groups[1].effect.uniforms.insert("alpha".into(),vec![0.25]);
        assert!(src.matches(&f,1,(960,544))); // applied live, never baked into input
        f.shader_groups[0].effect.mask_texture=Some(TextureId(91));
        assert!(!src.matches(&f,1,(960,544)));
        src.store(&f,1,(960,544));CHANGED_TEXTURE.with(|v|v.set(91));
        assert!(!src.matches(&f,1,(960,544)));CHANGED_TEXTURE.with(|v|v.set(0));
    }
    #[test]
    fn input_snapshot_survives_unrelated_prefix_insertion() {
        let mut f=mosaic_frame();let mut src=InputSnapshot::default();src.valid=true;
        src.store(&f,1,(960,544));
        f.commands.insert(0,f.commands[0].clone());
        for g in &mut f.shader_groups {g.start+=1;g.end+=1;}
        assert!(src.matches(&f,1,(960,544)));
        f.commands[2].transform.translation.x+=1.;
        assert!(!src.matches(&f,1,(960,544)));
    }
    #[test]
    fn effect_input_tracks_only_referenced_masks_and_survives_mask_prefix_changes() {
        let mut f=mosaic_frame();let mut src=InputSnapshot::default();src.valid=true;
        let mut portrait=f.commands[1].clone();portrait.texture=TextureId(77);
        f.mask_commands.push(portrait.clone());src.store(&f,1,(960,544));
        // An unrelated UI portrait must not invalidate a filtered scene input.
        f.mask_commands[0].opacity=0.25;
        assert!(src.matches(&f,1,(960,544)));
        CHANGED_TEXTURE.with(|c|c.set(77));
        assert!(src.matches(&f,1,(960,544)));CHANGED_TEXTURE.with(|c|c.set(0));
        f.shader_groups[0].mask_range=Some([0,1]);
        assert!(!src.matches(&f,1,(960,544)));src.store(&f,1,(960,544));
        // Absolute indices can shift when an earlier, unrelated mask appears.
        f.mask_commands.insert(0,portrait);f.shader_groups[0].mask_range=Some([1,2]);
        assert!(src.matches(&f,1,(960,544)));
        f.mask_commands[1].opacity=0.5;assert!(!src.matches(&f,1,(960,544)));
        src.store(&f,1,(960,544));CHANGED_TEXTURE.with(|c|c.set(77));
        assert!(!src.matches(&f,1,(960,544)));CHANGED_TEXTURE.with(|c|c.set(0));
        f.shader_groups[0].mask_range=Some([1,3]);assert!(!src.matches(&f,1,(960,544)));
    }
    #[test]
    fn shared_pool_generation_and_reservations_prevent_stale_input_reuse() {
        let f=mosaic_frame();let mut cache=NodeCache::default();cache.begin();
        let slot=cache.reserve(&f,1).unwrap();cache.store(slot,&f,1,(960,544),true);
        assert_eq!(cache.find(&f,1,(960,544)),Some(slot));
        bump_pool(slot); // final-composite or overlay overwrote this physical slot
        assert_eq!(cache.find(&f,1,(960,544)),None);
        for i in 0..5 {cache.use_slot(i);}
        assert!(cache.reserve(&f,1).is_none());
        cache.begin();assert!(cache.reserve(&f,1).is_some());
    }
    #[test]
    fn needed_input_is_protected_before_traversal_and_released_when_stale() {
        let mut f=mosaic_frame();let mut cache=NodeCache::default();
        cache.store(4,&f,1,(960,544),true);cache.begin();cache.protect_inputs(&f,(960,544));
        assert!(!cache.busy(4));assert!(cache.protected(4));
        assert_ne!(cache.reserve(&f,1),Some(4));
        // If a source changes, its old storage becomes available immediately.
        f.commands[0].opacity=0.4;cache.begin();cache.protect_inputs(&f,(960,544));
        assert!(!cache.protected(4));assert_eq!(cache.reserve(&f,1),Some(4));
    }
    #[test]
    fn simple_wrapper_does_not_duplicate_single_effect_input_cache() {
        let mut f=mosaic_frame();let mut wrapper=f.shader_groups[0].clone();
        f.shader_groups.push(wrapper.clone());
        assert!(!node_cache::eligible(&f,2));
        wrapper.effect=f.shader_groups[1].effect.clone();
        f.shader_groups.insert(2,wrapper);
        assert!(node_cache::eligible(&f,3));
    }
    #[test]
    fn full_input_pool_yields_to_a_stable_final_result_without_early_churn() {
        let mut f=mosaic_frame();f.shader_groups.push(f.shader_groups[0].clone());
        let mut cache=RetainedGroups::default();
        for slot in 0..5 {cache.nodes.store(slot,&f,1,(960,544),true);}
        EVENTS.with(|v|v.borrow_mut().clear());
        for _ in 0..20 {render_cached(&f,960,544,&mut cache);}
        EVENTS.with(|v|assert!(!v.borrow().iter().any(|e|e=="bake")));
        for _ in 0..15 {render_cached(&f,960,544,&mut cache);}
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","end-frame"]));
    }
    #[test]
    fn unsupported_dependency_keeps_original_path() {
        let mut f=mosaic_frame();assert!(node_cache::eligible(&f,1));
        f.shader_groups[1].mask_range=Some([0,1]);assert!(!node_cache::eligible(&f,1));
        f.shader_groups[1].mask_range=None;
        f.shader_groups[1].effect.name="unregistered".into();assert!(!node_cache::eligible(&f,1));
    }
    #[test]
    fn input_cache_metadata_has_an_admission_bound() {
        let mut f=mosaic_frame();f.commands.resize(4097,f.commands[0].clone());
        f.shader_groups[1].end=4097;assert!(!node_cache::eligible(&f,1));
        f.shader_groups[1].end=4096;assert!(node_cache::eligible(&f,1));
        f.mask_commands.resize(4097,f.commands[0].clone());assert!(!node_cache::eligible(&f,1));
    }
    #[test]
    fn mosaic_source_tracks_nested_effects_masks_pixels_and_program_replacement() {
        let mut f=mosaic_frame();let mut src=InputSnapshot::default();src.valid=true;
        src.store(&f,1,(960,544));assert!(src.matches(&f,1,(960,544)));
        assert!(!src.matches(&f,1,(1280,720)));
        f.shader_groups[0].effect.uniforms.insert("alpha".into(),vec![0.5]);
        assert!(!src.matches(&f,1,(960,544)));src.store(&f,1,(960,544));
        f.mask_commands.push(f.commands[0].clone());
        f.shader_groups[0].mask_range=Some([0,1]);
        assert!(!src.matches(&f,1,(960,544)));src.store(&f,1,(960,544));
        CHANGED_TEXTURE.with(|v|v.set(f.commands[1].texture.0));
        assert!(!src.matches(&f,1,(960,544)));CHANGED_TEXTURE.with(|v|v.set(0));
        assert!(src.matches(&f,1,(960,544)));
        super::super::external_effects::clear();assert!(!src.matches(&f,1,(960,544)));
    }
    #[test]
    fn mosaic_unverified_program_or_invalid_coordinates_keep_original_route() {
        use super::super::external_effects as ex;
        let f=mosaic_frame();let mut e=f.shader_groups[1].effect.clone();
        assert!(ex::cacheable_mosaic(&e));
        for n in [0.,-1.,f32::NAN,f32::INFINITY] {e.uniforms.insert("size".into(),vec![n]);assert!(!ex::cacheable_mosaic(&e));}
        e=f.shader_groups[1].effect.clone();e.mask_texture=Some(TextureId(42));assert!(!ex::cacheable_mosaic(&e));
        e.mask_texture=None;e.user_texture=Some(TextureId(42));assert!(!ex::cacheable_mosaic(&e));
        let src=b"float alpha; void vs(float4 position:POSITION){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(float2 texCoord0:TEXCOORD0,float2 texCoord1:TEXCOORD1,out float4 result:COLOR0){result=float4(alpha,0,0,1);}";
        ex::register_source("verified-mosaic-test",src).unwrap();
        assert!(!ex::cacheable_mosaic(&f.shader_groups[1].effect));
    }
    #[test]
    fn mosaic_neutral_wrapper_flattens_only_when_premultiplication_is_preserved() {
        let mut f=mosaic_frame();let wrapper=f.shader_groups[0].clone();f.shader_groups.push(wrapper);
        assert!(!passthrough_group(&f,2,960,544)); // retain the static final cache
        assert!(passthrough_group_inner(&f,2,960,544,true));
        f.shader_groups[1].effect.uniforms.insert("alpha".into(),vec![0.5]);
        assert!(!passthrough_group_inner(&f,2,960,544,true));
        f.shader_groups[1].effect.uniforms.remove("alpha");
        f.shader_groups[2].effect.uniforms.insert("alpha".into(),vec![0.5]);
        assert!(!passthrough_group_inner(&f,2,960,544,true));
        f.shader_groups[2].effect.uniforms.remove("alpha");
        f.commands[0].blend=BlendMode::Add;
        assert!(!passthrough_group_inner(&f,2,960,544,true));
    }
    #[test]
    fn mosaic_steps_do_not_bake_short_lived_outputs_but_settled_frame_does() {
        let mut f=mosaic_frame();let wrapper=f.shader_groups[0].clone();f.shader_groups.push(wrapper);
        let mut caches=RetainedGroups::default();
        for size in [20.,30.,40.,50.,60.,70.] {
            f.shader_groups[1].effect.uniforms.insert("size".into(),vec![size]);
            EVENTS.with(|v|v.borrow_mut().clear());
            for _ in 0..4 {render_cached(&f,960,544,&mut caches);}
            EVENTS.with(|v|assert!(!v.borrow().iter().any(|s|s=="bake")));
        }
        EVENTS.with(|v|v.borrow_mut().clear());
        for _ in 0..30 {render_cached(&f,960,544,&mut caches);}
        EVENTS.with(|v|{assert_eq!(v.borrow().iter().filter(|s|*s=="bake").count(),1);assert!(v.borrow().iter().any(|s|s=="cached"));});
    }
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_frame_begin(_: u32, _: u32) {
        event("frame".into());
    }
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_frame_end() {
        event("end-frame".into());
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn art3m1s_gxm_draw_effect(draw: *const EffectDraw) {
        DRAW_KINDS.with(|k|k.borrow_mut().push(unsafe {(*draw).effects.flags[0]}));
        event(format!("draw:{}", unsafe { (*draw).texture }));
    }
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_group_filter_chain(_:*const EffectDraw,count:u32)->i32 {for _ in 0..count{event("filter".into());}1}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_group_end_half_blur(_:*const EffectDraw,count:u32)->i32 {event(format!("half-blur:{count}"));1}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_draw_cached_blur(_:*const EffectDraw,_:*const EffectDraw,_:u32,_:u64)->i32 {0}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_draw_cached_effect(_:*const EffectDraw,n:u32,_:*const EffectDraw,_:*const u32,count:u32,_:u64,_:u32)->i32 {event(format!("translated:{n}:{count}"));0}
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_group_begin() -> i32 {
        event("begin-group".into());
        1
    }
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_group_mask_begin() -> i32 {
        event("begin-mask".into());
        MASK_TOKEN.with(|v|v.set(v.get()+1));
        1
    }
    thread_local! {
        static MASK_TOKEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        static MASK_REUSE_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_group_mask_revision() -> u64 {
        MASK_TOKEN.with(|v| v.get())
    }
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_emote_composite_cache_enabled() -> i32 { 1 }
    #[unsafe(no_mangle)]
    extern "C" fn art3m1s_gxm_group_mask_reuse(token: u64) -> i32 {
        let ok = token != 0 && MASK_REUSE_ENABLED.with(|v|v.get()) && MASK_TOKEN.with(|v|v.get()) == token;
        if ok { event("reuse-mask".into()); }
        i32::from(ok)
    }
    #[test]
    fn native_mask_reuse_rejects_content_target_texture_size_and_frame_changes() {
        let mut mask = neutral_frame().commands.remove(0);
        mask.shader = None;
        mask.native_emote = Some(NativeEmoteMaterial {
            corner_colors: [[1.;4];4], uv_rect: [0.,0.,1.,1.], blend_mode: 0,
            clip_rect: [-1.0e30,-1.0e30,1.0e30,1.0e30], wipe: [0.;3],
        });
        let mut cache = mask_reuse::MaskReuse::default();
        MASK_REUSE_ENABLED.with(|v|v.set(true));
        let draw = |cache: &mut mask_reuse::MaskReuse, mask: &DrawCommand, width| {
            EVENTS.with(|v|v.borrow_mut().clear());
            assert!(cache.draw(std::slice::from_ref(mask), width, 544));
            EVENTS.with(|v|v.borrow().contains(&"reuse-mask".to_string()))
        };
        assert!(!draw(&mut cache,&mask,960));
        assert!(draw(&mut cache,&mask,960));
        mask.opacity = 0.5;assert!(!draw(&mut cache,&mask,960));
        mask.transform.translation.x += 4.;assert!(!draw(&mut cache,&mask,960));
        MASK_TOKEN.with(|v|v.set(v.get()+1));assert!(!draw(&mut cache,&mask,960));
        CHANGED_TEXTURE.with(|v|v.set(41));assert!(!draw(&mut cache,&mask,960));
        assert!(!draw(&mut cache,&mask,800));
        cache.clear();assert!(!draw(&mut cache,&mask,800));
        mask.native_emote = None;assert!(!draw(&mut cache,&mask,800));
        assert!(!draw(&mut cache,&mask,800));
        MASK_REUSE_ENABLED.with(|v|v.set(false));
        CHANGED_TEXTURE.with(|v|v.set(0));
    }
    #[test]
    fn native_composite_replays_only_the_exact_pose_and_mask() {
        let mut frame=neutral_frame();
        for c in &mut frame.commands {
            c.native_emote=Some(NativeEmoteMaterial {
                corner_colors:[[1.;4];4],uv_rect:[0.,0.,1.,1.],blend_mode:0,
                clip_rect:[-1.0e30,-1.0e30,1.0e30,1.0e30],wipe:[0.;3],
            });
        }
        let mut masked=frame.shader_groups[0].clone();masked.start=0;masked.end=1;
        masked.effect.name=ALPHA_MASK_SHADER.into();masked.mask_range=Some([0,1]);
        frame.mask_commands=vec![frame.commands[0].clone()];
        frame.shader_groups.insert(0,masked);
        let mut cache=RetainedGroups::default();
        assert!(retainable_group(&frame,1,960,544).is_some());
        for _ in 0..4{render_cached(&frame,960,544,&mut cache);}
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&frame,960,544,&mut cache);
        EVENTS.with(|v|assert!(v.borrow().contains(&"cached".to_string())));
        // A changed transform, expression/mask opacity, texture replacement, or
        // disappearing actor must not replay an earlier expression.
        for change in 0..4 {
            match change {
                0=>frame.commands[0].transform.translation.x+=5.,
                1=>frame.mask_commands[0].opacity=0.25,
                2=>frame.commands[1].native_emote.as_mut().unwrap().corner_colors[0][3]=0.5,
                _=>CHANGED_TEXTURE.with(|v|v.set(frame.commands[0].texture.0)),
            }
            EVENTS.with(|v|v.borrow_mut().clear());render_cached(&frame,960,544,&mut cache);
            EVENTS.with(|v|assert!(!v.borrow().contains(&"cached".to_string()),"change {change}"));
            for _ in 0..4{render_cached(&frame,960,544,&mut cache);}
        }
        frame.commands.clear();frame.shader_groups.clear();frame.mask_commands.clear();
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&frame,960,544,&mut cache);
        EVENTS.with(|v|assert!(!v.borrow().contains(&"cached".to_string())));
        CHANGED_TEXTURE.with(|v|v.set(0));
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn art3m1s_gxm_group_end(draw: *const EffectDraw) {
        event(format!("end-group:{}", unsafe { (*draw).effects.flags[0] }));
    }
    #[test]
    fn static_overlay_reuses_then_invalidates_on_content_texture_and_hide(){
        let mut f=DrawList::default();
        let c=DrawCommand{texture:TextureId(41),size:TextureInfo{width:4,height:4},transform:glam::Affine2::from_translation(glam::vec2(20.,400.)),opacity:1.,blend:BlendMode::Alpha,color:ColorFilter::default(),clip:ClipRect{uv_offset:[0.,0.],uv_scale:[1.,1.],quad_size:[4.,4.]},clip_bounds:None,shader:None,mesh:None,stencil:None,native_emote:None};
        f.commands=vec![c;40];let mut cache=RetainedGroups::default();
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        assert!(!cache.overlay.baked);
        for _ in 0..8{render_cached(&f,960,544,&mut cache);}assert!(cache.overlay.baked);
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert!(v.borrow().contains(&"cached".to_string())));
        f.commands[3].opacity=0.5;render_cached(&f,960,544,&mut cache);assert!(!cache.overlay.baked);
        for _ in 0..8{render_cached(&f,960,544,&mut cache);}assert!(cache.overlay.baked);
        CHANGED_TEXTURE.with(|v|v.set(41));render_cached(&f,960,544,&mut cache);assert!(!cache.overlay.baked);
        CHANGED_TEXTURE.with(|v|v.set(0));f.commands.clear();render_cached(&f,960,544,&mut cache);assert!(cache.overlay.group.is_none());
    }
    #[test]
    fn overlay_does_not_cut_through_effect_group_or_nonalpha_tail(){
        let mut f=DrawList::default();assert!(overlay_tail(&f,960,544).is_none());
        // Coverage of the ordinary path is tested by the reuse/invalidation test;
        // an effect spanning the full list must reserve all of its children.
        f.shader_groups.push(ShaderGroup{key:None,start:0,end:100,effect:ShaderEffect{name:GROUP_COMPOSITE_SHADER.into(),uniforms:Default::default(),mask_texture:None,user_texture:None},clip_bounds:None,mask_range:None});
        assert!(overlay_tail(&f,960,544).is_none());
    }
    #[test]
    fn overlay_reuses_static_run_without_freezing_surrounding_animation(){
        for moving in [0,40,80] {
            let mut f=neutral_frame();let mut c=f.commands[1].clone();
            c.texture=TextureId(41);c.shader=None;
            f.shader_groups.clear();f.commands=vec![c;81];
            let mut caches=RetainedGroups::default();
            for tick in 0..14 {
                f.commands[moving].transform.translation.x=tick as f32;
                render_cached(&f,960,544,&mut caches);
            }
            assert!(caches.overlay.baked,"moving={moving}");
            let g=caches.overlay.group.as_ref().unwrap().clone();
            assert!(!(g.start..g.end).contains(&moving));
            EVENTS.with(|v|v.borrow_mut().clear());
            f.commands[moving].transform.translation.x+=1.;
            render_cached(&f,960,544,&mut caches);
            let expected:Vec<_>=std::iter::once("frame".to_string())
                .chain((0..g.start).map(|_|"draw:41".to_string()))
                .chain(std::iter::once("cached".to_string()))
                .chain((g.end..81).map(|_|"draw:41".to_string()))
                .chain(std::iter::once("end-frame".to_string())).collect();
            EVENTS.with(|v|assert_eq!(*v.borrow(),expected));
            // New pixels in an atlas invalidate the baked run even when the
            // draw commands have not changed. Empty text must not replay it.
            CHANGED_TEXTURE.with(|v|v.set(41));
            render_cached(&f,960,544,&mut caches);assert!(!caches.overlay.baked);
            CHANGED_TEXTURE.with(|v|v.set(0));
            f.commands.clear();render_cached(&f,960,544,&mut caches);
            assert!(caches.overlay.group.is_none());
        }
    }
    #[test]
    fn overlay_static_run_stays_outside_new_effect_boundaries(){
        let mut f=neutral_frame();let c=f.commands[1].clone();
        f.shader_groups.clear();f.commands=vec![c;81];
        let mut caches=RetainedGroups::default();
        for tick in 0..14 {
            f.commands[80].opacity=0.5+tick as f32/100.;
            render_cached(&f,960,544,&mut caches);
        }
        assert!(caches.overlay.baked);
        let mut group=neutral_frame().shader_groups.remove(0);group.end=50;
        f.shader_groups.push(group);
        // Only 31 ungrouped commands remain; reusing the old 80-command
        // range would apply the group's effect incorrectly or draw twice.
        assert!(overlay_span(&f,960,544,Some(&caches.overlay)).is_none());
        render_cached(&f,960,544,&mut caches);assert!(caches.overlay.group.is_none());
    }
    #[test]
    fn blend_modes_remain_distinct_and_effect_layout_matches_c() {
        let modes = [
            BlendMode::Alpha,
            BlendMode::Add,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::NativeReverseSubtract,
            BlendMode::PremultipliedAlpha,
            BlendMode::PremultipliedAdd,
            BlendMode::NativeAdd,
            BlendMode::NativeMultiply,
            BlendMode::NativeScreen,
        ];
        for (i, m) in modes.iter().enumerate() {
            assert_eq!(blend_code(*m), i as u32);
        }
        assert_eq!(std::mem::size_of::<Effects>(), 176);
        assert_eq!(std::mem::offset_of!(EffectDraw, effects), 96);
        assert_eq!(std::mem::offset_of!(EffectDraw, blend), 272);
    }
    #[test]
    fn group_uniforms_override_color_and_keep_premultiplied_blend() {
        let effect = ShaderEffect {
            name: GROUP_COMPOSITE_SHADER.into(),
            uniforms: [
                ("alpha".into(), vec![0.4]),
                ("grayscale".into(), vec![1.0]),
                ("negative".into(), vec![1.0]),
                ("opaque".into(), vec![1.0]),
                ("blendMode".into(), vec![1.0]),
            ]
            .into(),
            mask_texture: Some(TextureId(42)),
            user_texture: None,
        };
        let group = ShaderGroup {
            key: None,
            start: 0,
            end: 1,
            effect,
            clip_bounds: None,
            mask_range: None,
        };
        let draw = encode(&group_command(&group, 960, 544), 960, 544).unwrap();
        assert_eq!(draw.blend, 6);
        assert_eq!(draw.mask, 42);
        assert_eq!(draw.tint[3], 0.4);
        assert_eq!(draw.effects.flags, [3.0, 1.0, 1.0, 0.0]);
        assert_eq!(draw.effects.transition[2], 1.0);
    }
    fn test_group(kind: &str, start: usize, end: usize) -> ShaderGroup {
        ShaderGroup {
            key: None,
            start,
            end,
            clip_bounds: None,
            mask_range: None,
            effect: ShaderEffect {
                name: kind.into(),
                uniforms: Default::default(),
                mask_texture: None,
                user_texture: None,
            },
        }
    }
    #[test]
    fn generated_stencil_runs_inside_existing_character_groups() {
        let mut inner = test_group(GROUP_COMPOSITE_SHADER, 1, 2);
        inner.effect.uniforms.insert("grayscale".into(), vec![1.0]);
        let mut outer = test_group(GROUP_COMPOSITE_SHADER, 0, 3);
        outer.effect.uniforms.insert("alpha".into(), vec![0.8]);
        let mut frame = DrawList::new();
        for (id, label, masks) in [
            (10, "face", vec![]),
            (20, "shadow", vec!["face".to_owned()]),
            (30, "following", vec![]),
        ] {
            let mut c = group_command(&inner, 960, 544);
            c.texture = TextureId(id);
            c.shader = None;
            c.stencil = Some(StencilMetadata {
                namespace: 7,
                source_label: label.into(),
                mask_labels: masks,
            });
            frame.push(c);
        }
        frame.shader_groups = vec![inner, outer];
        frame.materialize_stencil_groups(ALPHA_MASK_SHADER);
        EVENTS.with(|v| v.borrow_mut().clear());
        render(&frame, 960, 544);
        EVENTS.with(|v| assert_eq!(*v.borrow(), [
            "frame", "begin-group", "draw:10", "begin-group", "begin-group",
            "draw:20", "begin-mask", "draw:10", "end-group:2", "end-group:3",
            "draw:30", "end-group:3", "end-frame",
        ]));
    }

    #[test]
    fn nested_groups_masks_and_following_sprites_keep_draw_order() {
        let mut inner = test_group(GROUP_COMPOSITE_SHADER, 0, 1);
        inner.effect.uniforms.insert("grayscale".into(),vec![1.0]);
        let mut outer = test_group(ALPHA_MASK_SHADER, 0, 2);
        outer.mask_range = Some([0, 1]);
        let mut frame = DrawList::new();
        for id in [10, 20, 30] {
            let mut c = group_command(&inner, 960, 544);
            c.texture = TextureId(id);
            c.shader = None;
            frame.push(c);
        }
        let mut mask = frame.commands[0].clone();
        mask.texture = TextureId(99);
        frame.mask_commands.push(mask);
        frame.shader_groups = vec![inner, outer];
        EVENTS.with(|v| v.borrow_mut().clear());
        render(&frame, 960, 544);
        EVENTS.with(|v| {
            assert_eq!(
                *v.borrow(),
                [
                    "frame",
                    "begin-group",
                    "begin-group",
                    "draw:10",
                    "end-group:3",
                    "draw:20",
                    "begin-mask",
                    "draw:99",
                    "end-group:2",
                    "draw:30",
                    "end-frame"
                ]
            )
        });
    }
    #[test]
    fn mesh_and_native_emote_parameters_cross_the_ffi_intact() {
        let mut cmd = group_command(&test_group(SPRITE_SHADER, 0, 1), 960, 544);
        cmd.mesh = Some(DrawMesh {
            grid_side: 0,
            vertices: std::sync::Arc::from([[0., 0., 0., 0.], [1., 0., 1., 0.], [0., 1., 0., 1.]]),
        });
        cmd.native_emote = Some(NativeEmoteMaterial {
            corner_colors: [[0.2, 0.3, 0.4, 0.5]; 4],
            uv_rect: [0.1, 0.2, 0.7, 0.8],
            blend_mode: 0x13,
            clip_rect: [0.1, 0.1, 0.9, 0.9],
            wipe: [2., -0.2, 1.],
        });
        let draw = encode(&cmd, 960, 544).unwrap();
        assert_eq!(draw.mesh_count, 3);
        assert_eq!(draw.mesh, cmd.mesh.as_ref().unwrap().vertices.as_ptr());
        assert_eq!(draw.effects.flags[3], 1.);
        assert_eq!(draw.effects.wipe, [2., -0.2, 1., 3.]);
        assert_eq!(draw.effects.model_x[3], 1.);
        assert_eq!(&draw.effects.corners[..4], &[0.2, 0.3, 0.4, 0.5]);
        let mesh = cmd.mesh.as_mut().unwrap();
        mesh.grid_side = 8;
        mesh.vertices = vec![[0.; 4]; 64].into();
        let indexed = encode(&cmd, 960, 544).unwrap();
        assert_eq!(indexed.grid_side, 8);
        assert_eq!(indexed.mesh_count, 64);
    }

    #[test]
    fn grayscale_toggle_preserves_texture_alpha_and_tint_at_ffi_boundary() {
        let mut cmd = group_command(&test_group(SPRITE_SHADER, 0, 1), 960, 544);
        cmd.shader = None;
        cmd.texture = TextureId(125);
        cmd.opacity = 0.35;
        cmd.color.multiply = [0.6, 0.8, 0.4];
        for (gray, negative) in [(true, false), (false, false), (true, true), (false, false)] {
            cmd.color.grayscale = gray;
            cmd.color.negative = negative;
            let draw = encode(&cmd, 960, 544).unwrap();
            assert_eq!(draw.texture, 125);
            assert_eq!(draw.tint, [0.6, 0.8, 0.4, 0.35]);
            assert_eq!(draw.effects.flags, [0.0, u32::from(gray) as f32, u32::from(negative) as f32, 0.0]);
        }
    }
    fn neutral_frame()->DrawList {
        let g=test_group(GROUP_COMPOSITE_SHADER,0,2);
        let mut f=DrawList::new();
        for id in [42,43] {let mut c=group_command(&g,960,544);c.texture=TextureId(id);
            c.shader=None;c.blend=BlendMode::Alpha;f.push(c);}
        f.shader_groups.push(g);f
    }
    #[test]
    fn rebuilding_neutral_external_wrapper_keeps_filter_and_retained_boundary(){
        let src=b"float alpha; void vs(float4 position:POSITION){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(float2 texCoord0:TEXCOORD0,float2 texCoord1:TEXCOORD1,out float4 result:COLOR0){result=float4(alpha,0,0,1);}";
        super::super::external_effects::register_source("test_gray_wrapper",src).unwrap();
        let mut f=neutral_frame();
        f.shader_groups.insert(0,test_group("test_gray_wrapper",0,2));
        assert!(!passthrough_group_inner(&f,1,960,544,true));
        super::super::external_effects::mark_test_builtin_gray("test_gray_wrapper");
        assert!(!passthrough_group(&f,1,960,544));
        assert!(passthrough_group_inner(&f,1,960,544,true));
        EVENTS.with(|v|v.borrow_mut().clear());
        let mut cache=RetainedGroups::default();
        render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|{let v=v.borrow();assert_eq!(v.iter().filter(|e|*e=="begin-group").count(),1);assert!(v.contains(&"node-source-build".into()));});
        render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|v.borrow_mut().clear());
        render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","end-frame"]));
        for (name,value) in [("alpha",0.5),("grayscale",1.),("negative",1.),("blendMode",1.),("opaque",1.)] {
            f.shader_groups[1].effect.uniforms.insert(name.into(),vec![value]);
            assert!(!passthrough_group_inner(&f,1,960,544,true),"{name}");
            f.shader_groups[1].effect.uniforms.remove(name);
        }
        f.shader_groups[1].mask_range=Some([0,1]);assert!(!passthrough_group_inner(&f,1,960,544,true));
        f.shader_groups[1].mask_range=None;
        f.shader_groups[0].effect.uniforms.insert("alpha".into(),vec![0.5]);
        assert!(!passthrough_group_inner(&f,1,960,544,true));
        f.shader_groups[0].effect.uniforms.remove("alpha");
        f.shader_groups[0].effect.mask_texture=Some(TextureId(9));
        assert!(!passthrough_group_inner(&f,1,960,544,true));
        f.shader_groups[0].effect.mask_texture=None;
        // Replacing the same ID with an arbitrary shader must drop the proof.
        super::super::external_effects::register_source("test_gray_wrapper",src).unwrap();
        assert!(!passthrough_group_inner(&f,1,960,544,true));
        super::super::external_effects::clear();
        assert!(!passthrough_group_inner(&f,1,960,544,true));
    }
    #[test]
    fn registered_external_chain_executes_every_pass_and_never_uses_identity_fallback(){
        let src=b"float alpha; void vs(float4 position:POSITION){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(float2 texCoord0:TEXCOORD0,float2 texCoord1:TEXCOORD1,out float4 result:COLOR0){result=float4(alpha,0,0,1);}";
        super::super::external_effects::register_source("blur_k",src).unwrap();
        let mut f=neutral_frame();f.shader_groups.clear();
        for _ in 0..5{f.shader_groups.push(test_group("blur_k",0,2));}
        assert!(!identity_blur_fallback(&f.shader_groups[4],960,544));
        EVENTS.with(|v|v.borrow_mut().clear());render(&f,960,544);
        EVENTS.with(|v|{let v=v.borrow();assert_eq!(v.iter().filter(|e|*e=="begin-group").count(),2);assert_eq!(v.iter().filter(|e|*e=="filter").count(),3);assert_eq!(v.iter().filter(|e|*e=="end-group:0").count(),1);assert_eq!(v.iter().filter(|e|*e=="node-source-build").count(),1);});
        super::super::external_effects::clear();
    }
    #[test]
    fn moving_kawase_chain_crosses_only_neutral_intermediates(){
        let id="verified_kawase_chain";
        let src=b"float alpha; void vs(float4 position:POSITION){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(float2 texCoord0:TEXCOORD0,float2 texCoord1:TEXCOORD1,out float4 result:COLOR0){result=float4(alpha,0,0,1);}";
        super::super::external_effects::register_source(id,src).unwrap();
        super::super::external_effects::mark_test_builtin_kawase(id);
        let mut f=neutral_frame();f.shader_groups.clear();
        for _ in 0..5 {
            let mut g=test_group(id,0,2);
            g.effect.uniforms.insert("offset".into(),vec![2.5]);
            g.effect.uniforms.insert("size".into(),vec![1./1920.]);
            f.shader_groups.push(g);
            f.shader_groups.push(test_group(GROUP_COMPOSITE_SHADER,0,2));
        }
        let mut nodes=NodeCache::default();
        for y in [-300.,-320.,-400.] {
            f.commands[0].transform.translation.y=y;
            let mut stats=[0,0];EVENTS.with(|v|v.borrow_mut().clear());
            render_range(&f,0,2,10,960,544,&mut stats,true,&mut nodes,false);
            EVENTS.with(|v|{let v=v.borrow();
                assert_eq!(v.iter().filter(|e|*e=="begin-group").count(),1);
                assert_eq!(v.iter().filter(|e|*e=="filter").count(),4);
                assert_eq!(v.iter().filter(|e|*e=="end-group:0").count(),1);
            });
            assert_eq!(stats,[10,5]);
        }
        // One opaque background qualifies; mixed composites and alpha sprites do not.
        let mut background=f.clone();
        background.commands.truncate(1);
        for g in &mut background.shader_groups{g.end=1;}
        EVENTS.with(|v|v.borrow_mut().clear());
        render_range(&background,0,1,10,960,544,&mut [0,0],true,&mut nodes,false);
        EVENTS.with(|v|{let v=v.borrow();assert!(v.contains(&"half-blur:5".into()));assert!(!v.iter().any(|e|e=="filter"));});
        // Large opaque image: the cheap flag is false but every tile is proven.
        background.commands[0].texture=TextureId(45);
        assert_eq!(art3m1s_gxm_texture_is_opaque(45),0);
        assert!(half_blur_chain(&background,&[8,6,4,2,0],0,960,544));
        EVENTS.with(|v|v.borrow_mut().clear());
        render_range(&background,0,1,10,960,544,&mut [0,0],true,&mut nodes,false);
        EVENTS.with(|v|assert!(v.borrow().contains(&"half-blur:5".into())));
        // A tile proof covering only the center cannot certify the whole image.
        background.commands[0].texture=TextureId(44);
        assert!(!half_blur_chain(&background,&[8,6,4,2,0],0,960,544));
        background.commands[0].texture=TextureId(43);
        assert!(!half_blur_chain(&background,&[8,6,4,2,0],0,960,544));
        background.commands[0].texture=TextureId(42);
        background.shader_groups[4].clip_bounds=Some([0.,0.,400.,400.]);
        assert!(!half_blur_chain(&background,&[8,6,4,2,0],0,960,544));
        // A translucent intermediate really changes the image: it must survive.
        f.shader_groups[5].effect.uniforms.insert("alpha".into(),vec![0.5]);
        EVENTS.with(|v|v.borrow_mut().clear());
        render_range(&f,0,2,10,960,544,&mut [0,0],true,&mut nodes,false);
        EVENTS.with(|v|assert!(v.borrow().iter().filter(|e|*e=="begin-group").count()>=3));
        // Replacing the source under the same ID revokes the proof.
        super::super::external_effects::register_source(id,src).unwrap();
        assert!(!passthrough_group_inner(&f,9,960,544,true));
        super::super::external_effects::clear();
    }
    #[test]
    fn translated_filters_require_verified_coordinates_and_independent_sources(){
        let src=b"float alpha; void vs(float4 position:POSITION){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(float2 texCoord0:TEXCOORD0,float2 texCoord1:TEXCOORD1,out float4 result:COLOR0){result=float4(alpha,0,0,1);}";
        for (id,kind) in [("pan_h",1),("pan_v",2),("pan_gray",6)] {
            super::super::external_effects::register_source(id,src).unwrap();
            super::super::external_effects::mark_test_translation(id,kind);
        }
        let mut f=neutral_frame();f.shader_groups.clear();
        for (id,step) in [("pan_h","width"),("pan_v","height")] {
            let mut g=test_group(id,0,2);
            g.effect.uniforms.insert("weights".into(),vec![0.2,0.16,0.12,0.07,0.03,0.015,0.004,0.001]);
            g.effect.uniforms.insert(step.into(),vec![1./960.]);f.shader_groups.push(g);
        }
        assert_eq!(translated_chain(&f,&[1,0],0,960,544),Some(vec![1,2]));
        f.commands[0].opacity=0.5;f.commands[0].texture=TextureId(43);
        assert_eq!(translated_chain(&f,&[1,0],0,960,544),Some(vec![1,2]));
        f.commands[1].clip_bounds=Some([0.,0.,100.,100.]);
        assert!(translated_chain(&f,&[1,0],0,960,544).is_none());f.commands[1].clip_bounds=None;
        f.commands[1].blend=BlendMode::Screen;
        assert!(translated_chain(&f,&[1,0],0,960,544).is_none());f.commands[1].blend=BlendMode::Alpha;
        f.shader_groups[0].effect.mask_texture=Some(TextureId(9));
        assert!(translated_chain(&f,&[1,0],0,960,544).is_none());f.shader_groups[0].effect.mask_texture=None;
        f.shader_groups[1].effect.uniforms.insert("height".into(),vec![f32::NAN]);
        assert!(translated_chain(&f,&[1,0],0,960,544).is_none());
        f.shader_groups[1].effect.uniforms.insert("height".into(),vec![1./544.]);
        f.shader_groups[1].effect.uniforms.insert("alpha".into(),vec![0.5]);
        assert!(translated_chain(&f,&[1,0],0,960,544).is_none());
        // A program ID reused for an arbitrary implementation revokes metadata.
        super::super::external_effects::register_source("pan_h",src).unwrap();
        assert_eq!(super::super::external_effects::translation_kind(&f.shader_groups[0].effect),0);
        f.shader_groups=vec![test_group("pan_gray",0,2)];
        assert_eq!(translated_chain(&f,&[0],0,960,544),Some(vec![6]));
        f.shader_groups[0].end=1;
        assert!(translated_chain(&f,&[0],0,960,544).is_none()); // cheap single sprite
        super::super::external_effects::clear();
    }
    #[test]
    fn coordinate_dependent_outer_keeps_moving_inner_translation_cache_reachable(){
        let src=b"float alpha; void vs(float4 position:POSITION){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(float2 texCoord0:TEXCOORD0,float2 texCoord1:TEXCOORD1,out float4 result:COLOR0){result=float4(alpha,0,0,1);}";
        for (id,kind) in [("inner_h",1),("inner_v",2),("screen_outer",0)] {
            super::super::external_effects::register_source(id,src).unwrap();
            super::super::external_effects::mark_test_translation(id,kind);
        }
        let mut f=neutral_frame();f.shader_groups.clear();
        for (id,step) in [("inner_h","width"),("inner_v","height")] {
            let mut g=test_group(id,0,2);
            g.effect.uniforms.insert("weights".into(),vec![0.2,0.16,0.12,0.07,0.03,0.015,0.004,0.001]);
            g.effect.uniforms.insert(step.into(),vec![1./960.]);f.shader_groups.push(g);
        }
        f.shader_groups.push(test_group("screen_outer",0,2));
        let mut nodes=NodeCache::default();
        for x in [0.,13.,29.] {
            for c in &mut f.commands {c.transform.translation.x=x;}
            EVENTS.with(|v|v.borrow_mut().clear());
            render_range(&f,0,2,3,960,544,&mut [0,0],true,&mut nodes,false);
            EVENTS.with(|v|{let v=v.borrow();assert!(v.contains(&"translated:2:2".into()));
                assert_eq!(v.iter().filter(|e|*e=="filter").count(),1);
                assert_eq!(v.iter().filter(|e|*e=="begin-group").count(),2);
            });
        }
        super::super::external_effects::clear();
    }
    #[test]
    fn five_identity_blur_wrappers_do_not_allocate_targets_during_pan(){
        let mut f=neutral_frame();
        let mut base=f.shader_groups.remove(0);base.end=1;
        base.effect.uniforms.insert("opaque".into(),vec![1.]);
        f.shader_groups.push(base);
        for _ in 0..5 {let mut g=test_group("blur_k",0,1);
            g.effect.uniforms.insert("size".into(),vec![0.00052]);
            g.effect.uniforms.insert("offset".into(),vec![2.5]);f.shader_groups.push(g);}
        let mut outer=test_group(GROUP_COMPOSITE_SHADER,0,2);
        outer.effect.uniforms.insert("opaque".into(),vec![1.]);f.shader_groups.push(outer);
        let mut caches=RetainedGroups::default();
        for x in [0.,-20.,-50.] {
            f.commands[0].transform.matrix2.x_axis.x=2.;
            f.commands[0].transform.translation.x=x;
            assert!(passthrough_group(&f,6,960,544));
            EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
            EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","draw:42","draw:43","end-frame"]));
        }
        // Alpha or a real filter inside the chain must prevent the opaque proof.
        f.shader_groups[0].effect.uniforms.insert("alpha".into(),vec![0.5]);
        assert!(!passthrough_group(&f,6,960,544));
        f.shader_groups[0].effect.uniforms.insert("alpha".into(),vec![1.]);
        f.shader_groups[0].effect.uniforms.insert("grayscale".into(),vec![1.]);
        assert!(!passthrough_group(&f,6,960,544));
    }
    #[test]
    fn identity_blur_fallback_keeps_mask_clip_and_unknown_shader_boundaries(){
        let mut f=neutral_frame();f.shader_groups[0].effect.name="blur_k".into();
        assert!(passthrough_group(&f,0,960,544));
        f.shader_groups[0].mask_range=Some([0,1]);assert!(!passthrough_group(&f,0,960,544));
        f.shader_groups[0].mask_range=None;f.shader_groups[0].effect.mask_texture=Some(TextureId(3));
        assert!(!passthrough_group(&f,0,960,544));
        f.shader_groups[0].effect.mask_texture=None;f.shader_groups[0].clip_bounds=Some([0.,0.,100.,100.]);
        assert!(!passthrough_group(&f,0,960,544));
        f.shader_groups[0].clip_bounds=None;f.shader_groups[0].effect.name="custom_other".into();
        assert!(!passthrough_group(&f,0,960,544));
    }
    #[test]
    fn masked_child_inside_neutral_parent_retains_while_background_moves(){
        let mut f=neutral_frame();
        let mut child=f.shader_groups[0].clone();child.start=1;
        child.effect.mask_texture=Some(TextureId(9));
        child.clip_bounds=Some([0.,262.,331.,278.]);
        let mut parent=f.shader_groups[0].clone();
        parent.effect.uniforms.insert("opaque".into(),vec![1.]);
        f.shader_groups=vec![child,parent];
        assert!(passthrough_group(&f,1,960,544));
        let mut cache=RetainedGroups::default();
        for _ in 0..2 {render_cached(&f,960,544,&mut cache);}
        // The outer background changes, but the masked child remains identical.
        f.commands[0].transform.matrix2.x_axis.x=2.;
        f.commands[0].transform.translation.x=-10.;
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","draw:42","cached","end-frame"]));
        f.commands[1].opacity=0.5;
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert!(!v.borrow().iter().any(|e|e=="cached")));
    }
    #[test]
    fn neutral_cache_scopes_handle_equal_ranges_and_preserve_filtered_ancestors(){
        let mut f=neutral_frame();
        let parent=f.shader_groups[0].clone();
        let mut child=parent.clone();child.effect.mask_texture=Some(TextureId(9));
        f.shader_groups=vec![child,parent.clone(),parent];
        let mut tail=f.commands[0].clone();tail.texture=TextureId(99);f.commands.push(tail);
        let mut cache=RetainedGroups::default();
        for _ in 0..2 {render_cached(&f,960,544,&mut cache);}
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","draw:99","end-frame"]));
        // An outer filter must apply to the combined child result, not be skipped.
        f.shader_groups[2].effect.uniforms.insert("grayscale".into(),vec![1.]);
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","begin-group","begin-group","draw:42","draw:43","end-group:3","node-source-build","draw:99","end-frame"]));
    }
    #[test]
    fn opaque_cover_maps_visible_uvs_for_panning_and_reflection(){
        let f=neutral_frame();let mut c=f.commands[0].clone();c.texture=TextureId(44);
        assert!(!opaque_cover(&c,960,544));
        c.transform.matrix2.x_axis.x=2.;c.transform.matrix2.y_axis.y=2.;
        c.transform.translation.x=-480.;c.transform.translation.y=-272.;
        assert!(opaque_cover(&c,960,544));
        c.transform.matrix2.x_axis.x=-2.;c.transform.translation.x=1440.;
        assert!(opaque_cover(&c,960,544));
        c.opacity=0.5;assert!(!opaque_cover(&c,960,544));c.opacity=1.;
        c.transform.translation.x=100.;assert!(!opaque_cover(&c,960,544));
    }
    #[test]
    fn overlay_yields_when_two_groups_and_three_versions_exceed_three_slots(){
        let mut f=neutral_frame();
        f.shader_groups[0].effect.uniforms.insert("grayscale".into(),vec![1.]);
        let mut second=f.shader_groups[0].clone();second.start=2;second.end=4;
        f.shader_groups.push(second);f.commands.extend(f.commands.clone());
        let tail=f.commands[1].clone();f.commands.extend(vec![tail;40]);
        assert!(overlay_tail(&f,960,544).is_some());
        let mut caches=RetainedGroups::default();
        // Two displayed ticks per animation state permit each bake. A static
        // second group plus three cyclic versions needs all four physical slots.
        for i in 0..90 {
            f.commands[1].texture=TextureId(61+(i/2)%3);
            render_cached(&f,960,544,&mut caches);
        }
        assert!(caches.overlay_blocked);assert!(caches.overlay.group.is_none());
        for i in 0..18 {
            f.commands[1].texture=TextureId(61+i%3);
            EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
            EVENTS.with(|v|{let e=v.borrow();assert_eq!(e.iter().filter(|s|*s=="cached").count(),2);
                assert!(!e.iter().any(|s|s=="bake"||s=="begin-group"||s=="overlay-bake"));});
        }
        f.shader_groups.pop();render_cached(&f,960,544,&mut caches);
        assert!(!caches.overlay_blocked); // Reconsider the text cache on a new layout.
    }

    #[test]
    fn retained_local_mask_is_baked_once_and_invalidated_by_mask_changes() {
        let mut f = neutral_frame();
        f.shader_groups[0].effect.uniforms.insert("opaque".into(), vec![0.]);
        f.shader_groups[0].mask_range = Some([0, 1]);
        let mut mask = f.commands[0].clone();
        mask.texture = TextureId(77);
        f.mask_commands.push(mask);
        let mut caches = RetainedGroups::default();
        render_cached(&f, 960, 544, &mut caches);
        EVENTS.with(|v| v.borrow_mut().clear());
        render_cached(&f, 960, 544, &mut caches);
        EVENTS.with(|v| assert_eq!(*v.borrow(),
            ["frame", "begin-group", "draw:42", "draw:43", "begin-mask", "draw:77", "bake", "end-frame"]));
        EVENTS.with(|v| v.borrow_mut().clear());
        render_cached(&f, 960, 544, &mut caches);
        EVENTS.with(|v| assert_eq!(*v.borrow(), ["frame", "cached", "end-frame"]));
        CHANGED_TEXTURE.with(|v| v.set(77));
        EVENTS.with(|v| v.borrow_mut().clear());
        render_cached(&f, 960, 544, &mut caches);
        EVENTS.with(|v| assert!(v.borrow().iter().any(|e| e == "begin-mask")));
        CHANGED_TEXTURE.with(|v| v.set(0));
        f.mask_commands[0].transform.translation.x += 10.;
        assert!(!caches.slots.iter().any(|c| c.matches(&f, &f.shader_groups[0], (960,544), 1)));
    }

    fn layered_gray_frame()->DrawList {
        let mut f=neutral_frame();
        let pair=f.commands.clone();f.commands.clear();f.shader_groups.clear();
        for i in 0..4 {
            f.commands.extend(pair.iter().cloned().map(|mut c|{c.transform.translation.x+=i as f32*20.;c}));
            let mut child=test_group(GROUP_COMPOSITE_SHADER,i*2,i*2+2);
            child.effect.uniforms.insert("alpha".into(),vec![0.8]);
            f.shader_groups.push(child);
        }
        let mut outer=test_group(GROUP_COMPOSITE_SHADER,0,8);
        outer.effect.uniforms.insert("grayscale".into(),vec![1.]);f.shader_groups.push(outer);f
    }
    #[test]
    fn initial_multi_layer_input_is_reused_without_redrawing_children() {
        let f=layered_gray_frame();let mut cache=RetainedGroups::default();
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|{
            assert_eq!(v.borrow().iter().filter(|e|e.starts_with("draw:")).count(),8);
            assert!(!v.borrow().iter().any(|e|e=="bake"));
        });
        assert!(cache.slots[0].input_cached);assert!(!cache.slots[0].baked);
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","restore-input","bake","end-frame"]));
        assert!(!cache.slots[0].input_cached);assert!(cache.slots[0].baked);
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","end-frame"]));
    }
    fn layered_external_gray_frame()->DrawList {
        let mut f=layered_gray_frame();
        let src=b"float alpha; void vs(float4 position:POSITION){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(float2 texCoord0:TEXCOORD0,float2 texCoord1:TEXCOORD1,out float4 result:COLOR0){result=float4(alpha,0,0,1);}";
        super::super::external_effects::register_source("test_multi_gray",src).unwrap();
        super::super::external_effects::mark_test_builtin_gray("test_multi_gray");
        for g in &mut f.shader_groups[..4] {g.effect.name="test_multi_gray".into();g.effect.uniforms.clear();}
        f.shader_groups[4].effect.uniforms.clear();f
    }
    #[test]
    fn complex_flattened_gray_wrapper_keeps_combined_first_input() {
        let f=layered_external_gray_frame();let mut cache=RetainedGroups::default();
        assert!(passthrough_group_inner(&f,4,960,544,true));
        assert!(!passthrough_group(&f,4,960,544));
        render_cached(&f,960,544,&mut cache);
        assert!(cache.slots[0].input_cached);
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","restore-input","bake","end-frame"]));
    }
    #[test]
    fn continuously_moving_flattened_children_stop_extra_input_capture() {
        let mut f=layered_external_gray_frame();let mut cache=RetainedGroups::default();
        for frame in 0..12 {
            f.commands[1].transform.translation.x=frame as f32;
            render_cached(&f,960,544,&mut cache);
        }
        assert!(cache.slots.iter().all(|c|!c.input_cached && !c.baked));
    }
    #[test]
    fn settled_composite_can_consume_matching_shared_node_input() {
        let f=layered_external_gray_frame();let mut cache=RetainedGroups::default();
        cache.slots[0].store(&f,&f.shader_groups[4],(960,544),1);
        cache.nodes.store(4,&f,4,(960,544),true);
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","restore-input","bake","end-frame"]));
        assert!(cache.nodes.find(&f,4,(960,544)).is_none()); // Ownership was consumed.
        assert!(cache.slots[0].baked);
    }
    #[test]
    fn first_input_never_hides_changed_expression_effect_or_program() {
        for change in 0..8 {
            let mut f=layered_gray_frame();let mut cache=RetainedGroups::default();
            render_cached(&f,960,544,&mut cache);assert!(cache.slots[0].input_cached);
            let mut size=(960,544);
            match change {
                0=>f.commands[1].texture=TextureId(78),
                1=>f.commands[1].transform.translation.x+=1.,
                2=>f.commands[1].opacity=0.3,
                3=>{f.shader_groups[0].effect.uniforms.insert("alpha".into(),vec![0.7]);},
                4=>{f.shader_groups[0].effect.mask_texture=Some(TextureId(71));},
                5=>CHANGED_TEXTURE.with(|v|v.set(43)),
                6=>cache.slots[0].programs=cache.slots[0].programs.wrapping_sub(1),
                _=>size=(1280,720),
            }
            EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,size.0,size.1,&mut cache);
            EVENTS.with(|v|{
                assert!(!v.borrow().iter().any(|e|e=="restore-input"||e=="cached"),"change={change}");
                assert!(v.borrow().iter().any(|e|e.starts_with("draw:")),"change={change}");
            });
            CHANGED_TEXTURE.with(|v|v.set(0));
        }
    }
    #[test]
    fn overwritten_or_unavailable_first_input_falls_back_to_full_composition() {
        for failure in 0..2 {
            let f=layered_gray_frame();let mut cache=RetainedGroups::default();
            render_cached(&f,960,544,&mut cache);assert!(cache.slots[0].input_cached);
            if failure==0 {bump_pool(0);}else{RESTORE_FAIL.with(|v|v.set(true));}
            EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
            EVENTS.with(|v|{
                assert_eq!(v.borrow().iter().filter(|e|e.starts_with("draw:")).count(),8);
                assert!(v.borrow().iter().any(|e|e=="bake"));
            });
            RESTORE_FAIL.with(|v|v.set(false));
            EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
            EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","end-frame"]));
        }
    }

    #[test]
    fn looping_group_versions_reuse_results_and_invalidate_changed_sources(){
        let mut f=neutral_frame();
        f.shader_groups[0].effect.uniforms.insert("grayscale".into(),vec![1.]);
        let mut caches=RetainedGroups::default();
        for id in [61,62,63] {
            f.commands[1].texture=TextureId(id);
            render_cached(&f,960,544,&mut caches);
            render_cached(&f,960,544,&mut caches);
        }
        for i in 0..30 {
            f.commands[1].texture=TextureId(61+i%3);
            EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
            EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","end-frame"]));
        }
        // A source update invalidates the matching old state even when its
        // command list and texture identifier have not changed.
        f.commands[1].texture=TextureId(61);CHANGED_TEXTURE.with(|v|v.set(61));
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
        EVENTS.with(|v|assert!(!v.borrow().iter().any(|e|e=="cached")));
        CHANGED_TEXTURE.with(|v|v.set(0));
        let g=&f.shader_groups[0];
        assert!(caches.select(&f,g,(960,544),1,&[true;4],4).is_none());
        assert!(caches.select(&f,g,(960,544),1,&[true,true,true,false],3).is_none());
        // The newly captured input is reserved for this whole frame.
        assert!(caches.select(&f,g,(960,544),1,&[true,true,true,false],4).is_none());
        caches.nodes.begin();
        assert_eq!(caches.select(&f,g,(960,544),1,&[true,true,true,false],4),Some(3));
    }

    #[test]
    fn retained_result_reuses_only_identical_group_and_keeps_outside_animation_live(){
        let mut f=neutral_frame();
        f.shader_groups[0].effect.uniforms.extend([("opaque".into(),vec![1.]),("grayscale".into(),vec![1.])]);
        let mut icon=f.commands[1].clone();icon.texture=TextureId(99);f.commands.push(icon);
        let mut cache=RetainedGroups::default();
        render_cached(&f,960,544,&mut cache); // Observe the first frame, without a cache pass.
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","restore-input","bake","draw:99","end-frame"]));
        f.commands[2].transform.translation.x=10.;
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","draw:99","end-frame"]));
        let cache=&cache.slots[0];
        let g=&f.shader_groups[0];
        assert!(cache.matches(&f,g,(960,544),2)); // Unrelated upload is harmless.
        CHANGED_TEXTURE.with(|c|c.set(42));
        assert!(!cache.matches(&f,g,(960,544),2));
        CHANGED_TEXTURE.with(|c|c.set(0));
        assert!(!cache.matches(&f,g,(1280,720),1));
        f.commands[0].opacity=0.5;assert!(!cache.matches(&f,&f.shader_groups[0],(960,544),1));
        f.commands[0].opacity=1.;
        f.shader_groups[0].effect.uniforms.insert("negative".into(),vec![1.]);
        assert!(!cache.matches(&f,&f.shader_groups[0],(960,544),1));
    }
    #[test]
    fn retained_result_supports_transparency_clips_and_texture_masks_but_rejects_nonstandard_output_blends(){
        let mut f=neutral_frame();
        f.shader_groups[0].effect.uniforms.extend([("opaque".into(),vec![1.]),("grayscale".into(),vec![1.])]);
        assert!(retainable_group(&f,0,960,544).is_some());
        for (key,value) in [("opaque",0.),("alpha",0.5)]{
            let mut changed=f.clone();changed.shader_groups[0].effect.uniforms.insert(key.into(),vec![value]);
            assert!(retainable_group(&changed,0,960,544).is_some());
        }
        let mut changed=f.clone();changed.shader_groups[0].effect.uniforms.insert("blendMode".into(),vec![1.]);
        assert!(retainable_group(&changed,0,960,544).is_none());
        let mut changed=f.clone();changed.shader_groups[0].clip_bounds=Some([0.,0.,500.,544.]);
        assert!(retainable_group(&changed,0,960,544).is_some());
        let mut cache=RetainedGroup::default();cache.store(&changed,&changed.shader_groups[0],(960,544),1);
        changed.shader_groups[0].clip_bounds=Some([0.,0.,501.,544.]);
        assert!(!cache.matches(&changed,&changed.shader_groups[0],(960,544),1));
        let mut changed=f.clone();changed.shader_groups[0].effect.mask_texture=Some(TextureId(9));
        assert!(retainable_group(&changed,0,960,544).is_some());
        cache.store(&changed,&changed.shader_groups[0],(960,544),1);
        CHANGED_TEXTURE.with(|c|c.set(9));
        assert!(!cache.matches(&changed,&changed.shader_groups[0],(960,544),2));
        CHANGED_TEXTURE.with(|c|c.set(0));
    }
    #[test]
    fn retained_dependencies_ignore_disjoint_effects_but_include_nested_effects_and_masks(){
        let mut f=neutral_frame();
        let outer=f.shader_groups[0].clone();
        let mut child=outer.clone();child.end=1;
        let mut unrelated=outer.clone();unrelated.start=2;unrelated.end=3;
        f.shader_groups=vec![child,outer.clone(),unrelated];
        let mut cache=RetainedGroup::default();cache.store(&f,&outer,(960,544),1);
        f.shader_groups[2].effect.uniforms.insert("alpha".into(),vec![0.5]);
        assert!(cache.matches(&f,&outer,(960,544),1));
        f.shader_groups[0].effect.uniforms.insert("alpha".into(),vec![0.5]);
        assert!(!cache.matches(&f,&outer,(960,544),1));
        cache.store(&f,&outer,(960,544),1);
        f.mask_commands.push(f.commands[0].clone());
        assert!(!cache.matches(&f,&outer,(960,544),1));
    }
    #[test]
    fn two_root_groups_retain_independently_and_preserve_interleaved_draws(){
        let mut f=neutral_frame();f.shader_groups[0].effect.uniforms.insert("grayscale".into(),vec![1.]);
        let mut second=f.shader_groups[0].clone();second.start=2;second.end=4;
        let commands=f.commands.clone();f.commands.extend(commands);f.shader_groups.push(second);
        let mut cache=RetainedGroups::default();render_cached(&f,960,544,&mut cache);
        render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","cached","end-frame"]));
        f.shader_groups[1].effect.uniforms.insert("alpha".into(),vec![0.5]);
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","begin-group","draw:42","draw:43","end-group:3","end-frame"]));
        f.commands[2].opacity=0.5;
        EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","cached","begin-group","draw:42","draw:43","node-source-build","end-frame"]));
        for x in [1.,2.,3.] {
            f.commands[2].transform.translation.x=x;
            EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut cache);
            EVENTS.with(|v|assert!(!v.borrow().iter().any(|e|e=="bake")));
        }
    }
    #[test]
    fn neutral_direct_draw_precedes_fusion_and_does_not_need_retained_cache(){
        let mut f=neutral_frame();f.commands.truncate(1);f.shader_groups[0].end=1;
        f.shader_groups[0].effect.uniforms.insert("opaque".into(),vec![1.]);
        assert!(passthrough_group(&f,0,960,544));
        assert!(fused_group(&f,0,960,544).is_some());
        assert!(retainable_group(&f,0,960,544).is_none());
        let mut cache=RetainedGroups::default();
        DRAW_KINDS.with(|k|k.borrow_mut().clear());
        for _ in 0..3 {render_cached(&f,960,544,&mut cache);}
        DRAW_KINDS.with(|k|assert_eq!(*k.borrow(),[0.,0.,0.]));
    }

    #[test]
    fn fused_group_caches_after_motion_stops_and_invalidates_on_motion(){
        let mut f=neutral_frame();f.commands[0].clip.quad_size=[2.,2.];
        f.shader_groups[0].effect.uniforms.insert("opaque".into(),vec![1.]);
        let mut caches=RetainedGroups::default();
        EVENTS.with(|e|e.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
        EVENTS.with(|e|assert!(!e.borrow().iter().any(|v|v=="begin-group")));
        EVENTS.with(|e|e.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
        EVENTS.with(|e|assert!(e.borrow().iter().any(|v|v=="bake")));
        EVENTS.with(|e|e.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
        EVENTS.with(|e|assert_eq!(*e.borrow(),["frame","cached","end-frame"]));
        f.commands[1].transform.translation.x=-1.;
        EVENTS.with(|e|e.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
        EVENTS.with(|e|assert!(!e.borrow().iter().any(|v|v=="cached"||v=="bake")));
    }

    #[test]
    fn screen_source_caches_without_freezing_the_background_and_invalidates_on_change(){
        let mut f=neutral_frame();
        f.commands[0].clip.quad_size[0]=1000.;
        let outer=f.shader_groups[0].clone();
        let mut light=test_group(GROUP_COMPOSITE_SHADER,1,2);
        light.effect.uniforms.insert("blendMode".into(),vec![2.]);
        f.commands[1].opacity=200./255.;
        f.shader_groups=vec![light,outer];
        assert!(retainable_group(&f,0,960,544).is_some());
        assert_eq!(encode(&group_command(&f.shader_groups[0],960,544),960,544).unwrap().blend,3);
        let mut caches=RetainedGroups::default();
        for tick in 0..5 {
            f.commands[0].transform.translation.x=-(tick as f32);
            EVENTS.with(|e|e.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
            EVENTS.with(|e|{
                let events=e.borrow();assert!(events.iter().any(|v|v=="draw:42"));
                if tick==1 {assert!(events.iter().any(|v|v=="bake"));}
                if tick>=2 {assert!(events.iter().any(|v|v=="cached"));}
            });
        }
        f.commands[1].opacity=0.4;
        EVENTS.with(|e|e.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
        EVENTS.with(|e|assert!(!e.borrow().iter().any(|v|v=="cached")));
        // A blend change is also part of the retained key.
        let cached=caches.slots.iter().find(|c|c.group.is_some()).unwrap();
        f.shader_groups[0].effect.uniforms.insert("blendMode".into(),vec![0.]);
        assert!(!cached.matches(&f,&f.shader_groups[0],(960,544),1));
    }

    #[test]
    fn local_opaque_base_keeps_overlap_and_rejects_effect_distribution(){
        let mut f=neutral_frame();f.commands[0].clip.quad_size=[2.,2.];
        f.shader_groups[0].effect.uniforms.insert("opaque".into(),vec![1.]);
        let draws=local_opaque_group(&f,0,960,544).unwrap();
        assert_eq!((draws[0].texture,draws[1].texture,draws[2].texture),(43,42,43));
        assert_eq!(draws[0].effects.flags[0],4.);
        assert_eq!(draws[2].clip,[0.,0.,2.,2.]);assert_eq!(draws[2].has_clip,1);
        assert_eq!(draws[2].quad,[2.,2.]);
        assert!(retainable_group(&f,0,960,544).is_some());
        f.commands[0].opacity=0.9;assert!(local_opaque_group(&f,0,960,544).is_none());
        f.commands[0].opacity=1.;f.commands[0].texture=TextureId(99);
        assert!(local_opaque_group(&f,0,960,544).is_none());f.commands[0].texture=TextureId(42);
        f.commands[1].clip.quad_size[0]=959.;assert!(local_opaque_group(&f,0,960,544).is_none());
        f.commands[1].clip.quad_size[0]=960.;
        for (key,val) in [("alpha",0.5),("grayscale",1.),("negative",1.),("blendMode",1.)]{
            let mut altered=f.clone();altered.shader_groups[0].effect.uniforms.insert(key.into(),vec![val]);
            assert!(local_opaque_group(&altered,0,960,544).is_none());
        }
    }

    #[test]
    fn single_sprite_fusion_ignores_only_proven_empty_plain_children() {
        let mut f=neutral_frame();
        f.shader_groups[0].effect.uniforms.insert("opaque".into(),vec![1.]);
        f.commands[0].texture=TextureId(4041);
        f.commands[0].clip.quad_size=[2.,2.];
        assert_eq!(fused_group(&f,0,960,544).unwrap().texture,43);
        f.commands.swap(0,1);
        assert_eq!(fused_group(&f,0,960,544).unwrap().texture,43);
        f.commands[1].texture=TextureId(99); // Unknown/nonempty source must survive.
        assert!(fused_group(&f,0,960,544).is_none());
        f.commands[1].texture=TextureId(4041);
        f.commands[1].blend=BlendMode::Multiply;
        assert!(fused_group(&f,0,960,544).is_none());
        f.commands[1].blend=BlendMode::Alpha;
        f.commands[1].shader=Some(f.shader_groups[0].effect.clone());
        assert!(fused_group(&f,0,960,544).is_none());
        f.commands[1].shader=None;
        f.commands[0].texture=TextureId(4041); // All empty still needs the opaque group output.
        assert!(fused_group(&f,0,960,544).is_none());
    }

    #[test]
    fn single_sprite_fusion_preserves_parent_and_child_filters_without_opaque_texture_guess() {
        let mut f=neutral_frame();f.commands.truncate(1);f.shader_groups[0].end=1;
        f.commands[0].texture=TextureId(41);f.commands[0].opacity=0.25;f.commands[0].color.multiply=[0.8,1.,0.6];
        f.shader_groups[0].effect.uniforms.extend([
            ("grayscale".into(),vec![1.]),("alpha".into(),vec![0.65]),("opaque".into(),vec![1.])]);
        let d=fused_group(&f,0,960,544).unwrap();assert_eq!(d.effects.flags[0],4.);
        assert_eq!(&d.effects.corners[..4],&[0.8,1.,0.6,0.25]);assert_eq!(d.tint[3],0.65);
        assert_eq!(d.effects.flags[1],1.);assert_eq!(d.effects.transition[2],1.);
        f.commands[0].clip.quad_size[0]=959.;assert!(fused_group(&f,0,960,544).is_none());
        f.shader_groups[0].effect.uniforms.insert("opaque".into(),vec![0.]);assert!(fused_group(&f,0,960,544).is_some());
        f.commands[0].color.negative=true;assert!(fused_group(&f,0,960,544).is_none());
    }
    #[test]
    fn neutral_group_elision_preserves_source_order_and_does_not_open_target() {
        let f=neutral_frame();assert!(passthrough_group(&f,0,960,544));
        EVENTS.with(|v|v.borrow_mut().clear());render(&f,960,544);
        EVENTS.with(|v|assert_eq!(*v.borrow(),["frame","draw:42","draw:43","end-frame"]));
    }
    #[test]
    fn opaque_prefix_allows_screen_child_without_rebuilding_the_whole_moving_scene() {
        let mut f=neutral_frame();
        let outer=f.shader_groups[0].clone();
        let mut light=test_group(GROUP_COMPOSITE_SHADER,1,2);
        light.effect.uniforms.insert("blendMode".into(),vec![2.]);
        light.effect.uniforms.insert("alpha".into(),vec![0.78]);
        f.shader_groups=vec![light,outer];
        assert!(passthrough_group(&f,1,960,544));
        // The light stays isolated and Screen-blended; only its neutral parent
        // disappears. Motion must never force a whole-scene input capture.
        let mut caches=RetainedGroups::default();
        for tick in 0..12 {
            f.commands[1].transform.translation.x=tick as f32;
            EVENTS.with(|v|v.borrow_mut().clear());render_cached(&f,960,544,&mut caches);
            EVENTS.with(|v|assert!(!v.borrow().iter().any(|e|e=="node-source-build")));
        }
        f.commands[0].opacity=0.5;assert!(!passthrough_group(&f,1,960,544));
        f.commands[0].opacity=1.;f.commands[0].clip.quad_size[0]=959.;
        assert!(!passthrough_group(&f,1,960,544));
        f.commands[0].clip.quad_size[0]=960.;f.commands[0].texture=TextureId(41);
        assert!(!passthrough_group(&f,1,960,544));
        f.commands[0].texture=TextureId(42);f.shader_groups[0].start=0;
        assert!(!passthrough_group(&f,1,960,544)); // cover inside Screen is not a prefix
        f.shader_groups[0].start=1;f.shader_groups[1].effect.uniforms.insert("alpha".into(),vec![0.5]);
        assert!(!passthrough_group(&f,1,960,544));
    }
    #[test]
    fn opacity_filters_masks_clips_and_destination_dependent_blends_keep_isolation() {
        for (name,value) in [("alpha",0.5),("grayscale",1.0),("negative",1.0),("blendMode",1.0)] {
            let mut f=neutral_frame();f.shader_groups[0].effect.uniforms.insert(name.into(),vec![value]);
            assert!(!passthrough_group(&f,0,960,544),"{name}");
        }
        let mut f=neutral_frame();f.shader_groups[0].effect.uniforms.insert("colorMultiply".into(),vec![1.,0.9,1.]);
        assert!(!passthrough_group(&f,0,960,544));
        let mut f=neutral_frame();f.shader_groups[0].mask_range=Some([0,1]);assert!(!passthrough_group(&f,0,960,544));
        let mut f=neutral_frame();f.shader_groups[0].effect.mask_texture=Some(TextureId(9));assert!(!passthrough_group(&f,0,960,544));
        let mut f=neutral_frame();f.shader_groups[0].clip_bounds=Some([0.,0.,100.,100.]);assert!(!passthrough_group(&f,0,960,544));
        for blend in [BlendMode::Add,BlendMode::Multiply,BlendMode::Screen,BlendMode::NativeAdd,BlendMode::PremultipliedAlpha] {
            let mut f=neutral_frame();f.commands[1].blend=blend;assert!(!passthrough_group(&f,0,960,544));
        }
    }
    #[test]
    fn forced_opacity_elision_needs_certified_full_cover_outside_nested_groups() {
        let mut f=neutral_frame();f.shader_groups[0].effect.uniforms.insert("opaque".into(),vec![1.]);
        assert!(passthrough_group(&f,0,960,544));
        f.commands[0].texture=TextureId(41);assert!(!passthrough_group(&f,0,960,544));
        f.commands[0].texture=TextureId(42);f.commands[0].opacity=0.5;assert!(!passthrough_group(&f,0,960,544));
        f.commands[0].opacity=1.;f.commands[0].clip.quad_size[0]=959.;assert!(!passthrough_group(&f,0,960,544));
        f.commands[0].clip.quad_size[0]=960.;f.commands[0].transform=glam::Affine2::from_angle(0.2);assert!(!passthrough_group(&f,0,960,544));
        f.commands[0].transform=glam::Affine2::IDENTITY;
        let mut inner=test_group(GROUP_COMPOSITE_SHADER,0,1);inner.effect.uniforms.insert("alpha".into(),vec![0.5]);
        f.shader_groups.insert(0,inner);assert!(!passthrough_group(&f,1,960,544));
    }
}
