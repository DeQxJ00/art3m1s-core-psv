//! Owned foreign snapshots. Handles are registry IDs, never encoded pointers.
//! C validates generation/owner for use; release queues retirement from any thread.
#[derive(Debug)]
pub struct RecoveredPacket {
    handle: u64,
    owner: std::thread::ThreadId,
}
impl PartialEq for RecoveredPacket {
    fn eq(&self, other: &Self) -> bool { self.handle == other.handle }
}
impl RecoveredPacket {
    #[cfg(feature="emote-recovery")]
    pub(crate) fn from_handle(handle: u64) -> Option<std::sync::Arc<Self>> {
        (handle != 0).then(|| std::sync::Arc::new(Self {handle, owner: std::thread::current().id()}))
    }
    #[cfg(feature="emote-recovery")]
    pub(crate) fn failed() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {handle:0,owner:std::thread::current().id()})
    }
    pub(crate) fn on_owner_thread(&self) -> bool { self.owner == std::thread::current().id() }
    pub(crate) fn handle(&self) -> u64 { self.handle }
    pub(crate) fn source_over(&self)->bool {
        #[cfg(feature="emote-recovery")]
        { self.handle!=0 && self.on_owner_thread() && unsafe{art3m1s_recovery_packet_source_over(self.handle)}==1 }
        #[cfg(not(feature="emote-recovery"))]
        { false }
    }
    /// Must be called before BeginScene. Deferred packets stay owned for retry.
    pub(crate) fn prepare(&self) -> bool {
        #[cfg(feature="emote-recovery")]
        { self.handle != 0 && self.on_owner_thread() && unsafe { art3m1s_recovery_packet_prepare(self.handle) } == 1 }
        #[cfg(not(feature="emote-recovery"))]
        { false }
    }
}
#[cfg(feature="emote-recovery")]
unsafe extern "C" {
    fn art3m1s_recovery_packet_prepare(handle: u64) -> i32;
    fn art3m1s_recovery_packet_source_over(handle: u64) -> i32;
    // Thread-safe queue-only operation; native destruction happens on render owner.
    fn art3m1s_recovery_packet_release(handle: u64);
}
#[cfg(feature="emote-recovery")]
impl Drop for RecoveredPacket {
    fn drop(&mut self) { if self.handle != 0 { unsafe { art3m1s_recovery_packet_release(self.handle) }; } }
}

/// Host scene clips are xywh; native replay requires half-open edge bounds.
pub(crate) fn clip_edges(bounds: Option<[f32;4]>) -> Result<Option<[f32;4]>,()> {
    let Some([x,y,w,h])=bounds else{return Ok(None)};
    let edges=[x,y,x+w,y+h];
    if ![x,y,w,h].iter().chain(edges.iter()).all(|v|v.is_finite()) || w<=0. || h<=0. {return Err(())}
    Ok(Some(edges))
}
/// Packet vertices encode logical pixels using a fixed 960x544 NDC basis.
/// Match the ordinary host bridge when the game's stage has another size.
pub(crate) fn stage_to_vita(mut affine:[f32;6],mut clip:Option<[f32;4]>,width:u32,height:u32)
    -> Result<([f32;6],Option<[f32;4]>),()> {
    if width==0||height==0{return Err(())}
    let sx=960./width as f32;let sy=544./height as f32;
    for i in [0,2,4]{affine[i]*=sx;}
    for i in [1,3,5]{affine[i]*=sy;}
    if let Some(c)=clip.as_mut(){c[0]*=sx;c[2]*=sx;c[1]*=sy;c[3]*=sy;}
    if !affine.iter().chain(clip.iter().flatten()).all(|v|v.is_finite()){return Err(())}
    Ok((affine,clip))
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn logical_stage_maps_geometry_and_clip_together(){
        let a=[1.,0.,0.,1.,120.,90.];let c=Some([120.,90.,420.,290.]);
        assert_eq!(stage_to_vita(a,c,960,544),Ok((a,c)));
        assert_eq!(stage_to_vita(a,c,1920,1088),Ok(([0.5,0.,0.,0.5,60.,45.],Some([60.,45.,210.,145.]))));
        let (mapped,bounds)=stage_to_vita([0.,1.,-1.,0.,1440.,810.],Some([0.,0.,1440.,810.]),1440,810).unwrap();
        assert_eq!(mapped[4],960.);assert_eq!(mapped[5],544.);assert_eq!(bounds,Some([0.,0.,960.,544.]));
        assert!(stage_to_vita(a,c,0,544).is_err());
    }
    #[test] fn non_origin_clip_converts_extent_to_edges(){
        assert_eq!(clip_edges(Some([120.,90.,300.,200.])),Ok(Some([120.,90.,420.,290.])));
        assert_eq!(clip_edges(None),Ok(None));
        assert!(clip_edges(Some([0.,0.,-1.,2.])).is_err());
        assert!(clip_edges(Some([f32::MAX,0.,f32::MAX,2.])).is_err());
        assert!(clip_edges(Some([f32::NAN,0.,3.,2.])).is_err());
    }
}
