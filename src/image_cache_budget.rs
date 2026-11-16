//! Shared retention budget, not a cap on process memory or active GPU surfaces.
//! Loader ready pixels/sources can borrow unused idle texture space. Publication
//! and provider retain serialize their counters; decoder scratch and GPU-retired
//! storage remain covered separately by the resource ledger/lifetime guards.
use std::sync::{Arc,Mutex,OnceLock};
pub(crate) const SESSION_RETENTION_BYTES:usize=192*1024*1024;
pub(crate) type SharedCacheBudget=Arc<Mutex<CacheBudget>>;
// Retained allocation capacities, not file lengths or whole-process memory.
#[derive(Clone,Copy,Default,Debug,PartialEq,Eq)]
pub(crate) struct CacheParts { pub decoded:usize,pub gpu:usize,pub encoded:usize,pub proof:usize }
impl CacheParts {
    pub fn total(&self)->usize{self.decoded+self.gpu+self.encoded+self.proof}
    pub fn add(&mut self,p:Self){self.decoded+=p.decoded;self.gpu+=p.gpu;self.encoded+=p.encoded;self.proof+=p.proof;}
}
// Distinct paths with a live script binding. Completed is a load-progress
// counter, not residency: successful payload handoff/reclamation preserves it.
#[derive(Clone,Copy,Default,Debug,PartialEq,Eq)]
pub(crate) struct ScriptPreloadCounts { pub bound:usize,pub pending:usize,pub missing:usize,pub planned:usize,pub completed:usize,pub pixels:usize,pub encoded:usize }
pub(crate) struct CacheBudget { pub limit:usize,pub ready_max:usize,pub idle_max:usize,pub lua_load_max:usize,pub ready:usize,pub idle:usize,pub emote:usize,pub emote_scratch:usize,pub video:usize,video_request:Option<(usize,std::time::Instant)>,emote_request:Option<(usize,std::time::Instant)>,pub ready_goal:usize,pub ready_reserved:usize,pub ready_parts:CacheParts,pub mask_parts:CacheParts,pub animation_parts:CacheParts,pub script_preload:ScriptPreloadCounts }
impl CacheBudget {
    pub fn new(limit:usize)->SharedCacheBudget{Arc::new(Mutex::new(Self{limit,ready_max:limit,idle_max:limit,lua_load_max:usize::MAX,ready:0,idle:0,emote:0,emote_scratch:0,video:0,video_request:None,emote_request:None,ready_goal:0,ready_reserved:0,ready_parts:CacheParts::default(),mask_parts:CacheParts::default(),animation_parts:CacheParts::default(),script_preload:ScriptPreloadCounts::default()}))}
    pub fn ready_limit(&self)->usize{
        let physical=self.limit.saturating_sub(self.idle+self.emote+self.emote_scratch+self.video);
        physical.min(self.ready_max).min(self.ready.max(self.limit.saturating_sub(self.idle+self.emote_goal()+self.video)))
    }
    fn emote_goal(&self)->usize{
        (self.emote+self.emote_scratch).max(self.emote_request.filter(|(_,until)|*until>std::time::Instant::now()).map_or(0,|(n,_)|n))
    }
    pub fn request_emote(&mut self,goal:usize){
        // Promise headroom, never spend bytes still owned by IDLE. Renewed by
        // a deferred worker; abandoned foreground requests expire promptly.
        self.emote_request=Some((goal.min(self.limit),std::time::Instant::now()+std::time::Duration::from_secs(1)));
    }
    pub fn clear_emote_request(&mut self){self.emote_request=None;}
    pub fn request_video(&mut self,goal:usize){self.video_request=Some((goal.min(self.limit),std::time::Instant::now()+std::time::Duration::from_secs(1)));}
    pub fn clear_video_request(&mut self){self.video_request=None;}
    fn video_goal(&self)->usize{self.video.max(self.video_request.filter(|(_,until)|*until>std::time::Instant::now()).map_or(0,|(n,_)|n))}
    pub fn idle_limit(&self,maximum:usize)->usize{maximum.min(self.idle_max).min(self.limit.saturating_sub(self.ready.saturating_add(self.ready_reserved).max(self.ready_goal)+self.emote_goal()+self.video_goal()))}
    // Potential capacity after render-thread reclamation, never permission to
    // publish into occupied IDLE. Model requests keep their higher priority.
    pub fn ready_reclaim_limit(&self)->usize{self.ready_max.min(self.limit.saturating_sub(self.emote_goal()+self.video))}
    // Request one bounded growth window beyond actual ready allocations, not
    // the entire chapter allowance as soon as one async job is queued. The
    // former 5/6 reservation evicted warm backgrounds with >100 MiB still free.
    // Publication refreshes this window; reclaim and admission remain serialized.
    // This is not permission to spend occupied bytes. Keep the 1/6 idle floor.
    pub fn request_ready(&mut self,pending_async:bool){
        self.ready_goal=if pending_async{
            self.ready.saturating_add(16*1024*1024).min(self.limit-self.limit/6).min(self.ready_max)
        }else{0};
    }
    pub fn set_ready_parts(&mut self,parts:CacheParts){let bytes=parts.total();debug_assert!(bytes<=self.ready_limit());self.ready=bytes;self.ready_parts=parts;}
    #[cfg(test)]
    pub fn set_ready(&mut self,bytes:usize){self.set_ready_parts(CacheParts{decoded:bytes,..Default::default()});}
    pub fn set_idle(&mut self,bytes:usize){debug_assert!(bytes<=self.limit.saturating_sub(self.ready+self.emote+self.emote_scratch+self.video));self.idle=bytes;}
}
// Like the existing loader, one game session owns these accounts. Shutdown and
// provider Drop release their respective counters, including a project reload.
pub(crate) fn session_budget()->SharedCacheBudget{
    static SHARED:OnceLock<SharedCacheBudget>=OnceLock::new();
    SHARED.get_or_init(||{
        let b=CacheBudget::new(SESSION_RETENTION_BYTES);
        {let mut a=b.lock().unwrap();a.lua_load_max=128;a.ready_max=64*1024*1024;a.idle_max=96*1024*1024;}
        b
    }).clone()
}

/// Apply a per-game policy between sessions, before the first script binding.
/// Reject live retention rather than invalidating allocations owned by a game.
#[unsafe(no_mangle)]
pub extern "C" fn art3m1s_image_cache_configure(lua_count:u32,ready_mib:u32,idle_mib:u32)->i32{
    if !(16..=4096).contains(&lua_count)||!(8..=192).contains(&ready_mib)||!(8..=192).contains(&idle_mib){return 0;}
    let shared=session_budget();let mut b=shared.lock().unwrap();
    if b.ready!=0||b.idle!=0||b.ready_reserved!=0{return 0;}
    b.lua_load_max=lua_count as usize;b.ready_max=ready_mib as usize*1024*1024;b.idle_max=idle_mib as usize*1024*1024;
    b.ready_goal=0;
    1
}

#[cfg(test)] mod tests{
    use super::*;
    #[test] fn per_game_limits_bound_retention_without_allocating_memory(){
        let shared=CacheBudget::new(192);let mut b=shared.lock().unwrap();
        b.ready_max=64;b.idle_max=96;
        assert_eq!(b.ready_limit(),64);assert_eq!(b.ready_reclaim_limit(),64);
        assert_eq!(b.idle_limit(192),96);assert_eq!((b.ready,b.idle),(0,0));
        b.set_ready(48);b.request_ready(true);assert_eq!(b.ready_goal,64);
        b.set_idle(96);b.video=32;assert_eq!(b.ready_limit(),64);
        b.video=64;assert_eq!(b.ready_limit(),32);
        b.video=0;b.request_emote(64);assert_eq!(b.idle_limit(192),64);
    }
    #[test] fn model_claim_preserves_published_ready_and_expires_without_spending_idle(){
        let shared=CacheBudget::new(100);let mut b=shared.lock().unwrap();
        b.set_ready(50);b.set_idle(40);b.request_emote(24);
        assert_eq!(b.ready_limit(),50);assert_eq!(b.idle_limit(100),26);
        assert_eq!(b.idle,40);assert_eq!(b.emote,0);
        b.emote_request=Some((24,std::time::Instant::now()-std::time::Duration::from_secs(1)));
        assert_eq!(b.idle_limit(100),50);assert_eq!(b.ready_limit(),60);
    }
    #[test] fn pending_prefetch_grows_reservation_without_purging_warm_images(){
        const M:usize=1024*1024;
        let shared=CacheBudget::new(192*M);let mut b=shared.lock().unwrap();
        b.set_ready(48*M);b.set_idle(60*M);b.request_ready(true);
        assert_eq!(b.ready_goal,64*M);assert_eq!(b.idle_limit(64*M),64*M);
        assert_eq!(b.idle,60*M); // A request never changes ownership.
        b.set_ready(128*M);b.request_ready(true);
        assert_eq!(b.ready_goal,144*M);assert_eq!(b.idle_limit(64*M),48*M);
        assert_eq!(b.ready_limit(),132*M); // Cannot publish into promised space yet.
        b.set_idle(48*M);b.set_ready(144*M);b.request_ready(true);
        assert_eq!(b.ready_goal,160*M);assert_eq!(b.idle_limit(64*M),32*M);
        b.set_idle(32*M);b.set_ready(160*M);b.request_ready(true);
        assert_eq!(b.ready_goal,160*M);
        b.request_ready(false);assert_eq!(b.ready_goal,0);
        b.set_ready(48*M);assert_eq!(b.idle_limit(64*M),64*M);
    }
    #[test] fn concurrent_retention_never_spends_the_same_headroom_twice(){
        let budget=CacheBudget::new(1024);
        let handles:Vec<_>=(0..2).map(|owner|{let b=budget.clone();std::thread::spawn(move||{
            for i in 0..2000{let mut s=b.lock().unwrap();
                if owner==0{let n=(i*79)%1500;let cap=s.ready_limit();s.set_ready(n.min(cap));}
                else{let cap=s.idle_limit(768);s.set_idle(((i*31)%1000).min(cap));}
                assert!(s.ready+s.idle<=s.limit);
            }
        })}).collect();
        for h in handles{h.join().unwrap();}
    }
}
