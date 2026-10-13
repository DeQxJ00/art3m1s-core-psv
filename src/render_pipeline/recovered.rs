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
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn non_origin_clip_converts_extent_to_edges(){
        assert_eq!(clip_edges(Some([120.,90.,300.,200.])),Ok(Some([120.,90.,420.,290.])));
        assert_eq!(clip_edges(None),Ok(None));
        assert!(clip_edges(Some([0.,0.,-1.,2.])).is_err());
        assert!(clip_edges(Some([f32::MAX,0.,f32::MAX,2.])).is_err());
        assert!(clip_edges(Some([f32::NAN,0.,3.,2.])).is_err());
    }
}
