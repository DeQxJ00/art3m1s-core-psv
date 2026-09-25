//! Script-directed CPU prefetch. GPU ownership never leaves the render thread.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use crate::resource_ledger::{Charge,Owner,Tracked};
use crate::image_proof::TileProof;
use crate::image_cache_budget::{SharedCacheBudget,CacheParts};

const BUDGET: usize = 16 * 1024 * 1024;
// Keep headroom for concurrent image decoding, OGV queues and active surfaces.
// The 96 MiB experiment exhausted the process heap in decoder_to_vec on hardware.
const READY_BUDGET: usize = BUDGET;
const MAX_QUEUE: usize = 64;
const SCRIPT_PREFETCH_BURST: usize = 4;
// One suspended source only, charged to the existing 16 MiB working allowance.
// Never suspend decoded pixels or introduce a second decoder/worker.
const MAX_PAUSED_SOURCE:usize=2*1024*1024;
enum LoadStep { Complete(Option<Payload>), Paused(Tracked<Vec<u8>>) }
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub(super) enum Kind { #[default] Image, Mask, Animation }
impl Kind {
    fn label(self)->&'static str{match self{Self::Image=>"image",Self::Mask=>"mask",Self::Animation=>"animation"}}
    fn cap(self)->usize{match self{Self::Image=>usize::MAX,Self::Mask=>16*1024*1024,Self::Animation=>32*1024*1024}}
}
pub(super) enum Payload { Gray(u32,u32,Tracked<Vec<u8>>,Tracked<Vec<u8>>), Pixels(Tracked<image::RgbaImage>, Tracked<Vec<u8>>,Option<TileProof>), Encoded(Tracked<Vec<u8>>,Option<TileProof>) }
impl Payload {
    #[cfg(test)] fn pixels(p:image::RgbaImage,b:Vec<u8>)->Self{Self::Pixels(p.into(),b.into(),None)}
    #[cfg(test)] fn encoded(b:Vec<u8>)->Self{Self::Encoded(b.into(),None)}
    fn ready(&mut self){match self{
        Self::Gray(_,_,p,b)=>{p.transfer(Owner::Ready);b.transfer(Owner::Ready);},
        Self::Pixels(p,b,proof)=>{p.transfer(Owner::Ready);b.transfer(Owner::Ready);if let Some(p)=proof{p.transfer(Owner::Ready);}},
        Self::Encoded(b,proof)=>{b.transfer(Owner::Ready);if let Some(p)=proof{p.transfer(Owner::Ready);}}
    }}

    fn parts(&self)->CacheParts {match self{
        Self::Gray(_,_,p,b)=>CacheParts{decoded:p.capacity(),encoded:b.capacity(),..Default::default()},
        Self::Pixels(p,b,proof)=>CacheParts{decoded:p.as_raw().capacity(),encoded:b.capacity(),proof:proof.as_ref().map_or(0,TileProof::bytes),gpu:0},
        Self::Encoded(b,proof)=>CacheParts{encoded:b.capacity(),proof:proof.as_ref().map_or(0,TileProof::bytes),..Default::default()},
    }}
    fn bytes(&self) -> usize { self.parts().total() }
}
fn demote(payload: &mut Option<Payload>) -> usize {
    if matches!(payload,Some(Payload::Gray(_,_,_,b)) if !b.is_empty()) {
        let Some(Payload::Gray(_,_,p,b))=payload.take() else{unreachable!()};
        let released=p.capacity();*payload=Some(Payload::Encoded(b,None));return released;
    }
    if !matches!(payload, Some(Payload::Pixels(_, encoded,_)) if !encoded.is_empty()) { return 0; }
    let Some(Payload::Pixels(pixels, encoded,proof)) = payload.take() else { unreachable!() };
    let released = pixels.as_raw().capacity();
    *payload = Some(Payload::Encoded(encoded,proof)); released
}
struct Entry { refs: usize, ticket: u64, bind_order:u64, prepared:bool, story_suppressed:bool, pending: bool, deferred: bool, demanded: bool, speculative:bool, payload: Option<Payload>,kind:Kind,leased:bool,retry_bytes:usize }
#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>, queue: VecDeque<(String, u64)>,
    masks:VecDeque<(String,u64)>,animations:VecDeque<(String,u64)>,urgent:Option<String>,lane:usize,chapter:String,
    script_burst:usize,
    // References to existing jobs only: no extra binding or resource lifetime.
    scene_priority:VecDeque<(String,u64)>,
    story_priority:VecDeque<(String,u64)>,
    story_managed:std::collections::HashSet<String>,
    serial: u64, stop: bool, active:Option<(String,u64)>,
    parked:Option<(String,u64)>,
    demotion_samples:u32,
    reserve_requested:bool,
}
impl State{
    fn queue_len(&self)->usize{self.queue.len()+self.masks.len()+self.animations.len()}
    fn queue_mut(&mut self,kind:Kind)->&mut VecDeque<(String,u64)>{match kind{Kind::Image=>&mut self.queue,Kind::Mask=>&mut self.masks,Kind::Animation=>&mut self.animations}}
    fn remove_queued(&mut self,path:&str)->Option<(String,u64)>{
        for q in [&mut self.queue,&mut self.masks,&mut self.animations]{if let Some(i)=q.iter().position(|(p,_)|p==path){return q.remove(i);}}
        None
    }
    fn pop_priority_job(&mut self)->Option<(String,u64)>{
        if let Some(path)=self.urgent.take(){
            if let Some(job)=self.remove_queued(&path){return Some(job);}
            if self.parked.as_ref().is_some_and(|(p,_)|p==&path){return self.parked.clone();}
        }
        while let Some((path,ticket))=self.scene_priority.pop_front(){
            if !self.entries.get(&path).is_some_and(|e|e.pending&&e.ticket==ticket){continue;}
            // A scene batch can mention the worker's current request. Never
            // enqueue it twice, including when the batch is refreshed mid-load.
            if self.active.as_ref().is_some_and(|(p,t)|p==&path&&*t==ticket){continue;}
            self.remove_queued(&path);
            self.entries.get_mut(&path).unwrap().deferred=false;
            return Some((path,ticket));
        }
        None
    }
    fn should_yield(&self,path:&str,ticket:u64)->bool{
        if self.stop||self.parked.is_some(){return false;}
        let live=|p:&str,t:u64|self.entries.get(p).is_some_and(|e|e.pending&&e.ticket==t);
        if !self.entries.get(path).is_some_and(|e|e.pending&&e.ticket==ticket&&e.speculative&&!e.demanded){return false;}
        // A refreshed batch can promote the active request itself.
        if self.scene_priority.iter().any(|(p,t)|p==path&&*t==ticket){return false;}
        self.urgent.as_ref().is_some_and(|p|p!=path&&self.entries.get(p).is_some_and(|e|e.pending))
            ||self.scene_priority.iter().any(|(p,t)|p!=path&&live(p,*t))
    }
    fn pop_job(&mut self)->Option<(String,u64)>{
        if let Some(job)=self.pop_priority_job(){return Some(job);}
        while self.script_burst<SCRIPT_PREFETCH_BURST && !self.story_priority.is_empty(){
            let (path,ticket)=self.story_priority.pop_front().unwrap();
            if !self.entries.get(&path).is_some_and(|e|e.pending&&e.ticket==ticket){continue;}
            if self.active.as_ref().is_some_and(|(p,t)|p==&path&&*t==ticket){continue;}
            self.remove_queued(&path);
            self.entries.get_mut(&path).unwrap().deferred=false;
            self.script_burst+=1;return Some((path,ticket));
        }
        // Lua bindings carry the game's first-use order, independent of resource
        // kind. Leave one supplemental opportunity after four script requests
        // so an effect omitted by cache.lua still makes forward progress.
        if self.script_burst<SCRIPT_PREFETCH_BURST{
            if let Some(job)=self.pop_prefetch_job(true,None){self.script_burst+=1;return Some(job);}
        }
        let lanes=[Kind::Mask,Kind::Animation,Kind::Image];
        for _ in 0..lanes.len(){
            let kind=lanes[self.lane%lanes.len()];self.lane+=1;
            if let Some(job)=self.pop_prefetch_job(false,Some(kind)){self.script_burst=0;return Some(job);}
        }
        if let Some(job)=self.pop_prefetch_job(true,None){self.script_burst=1;return Some(job);}
        None
    }
    fn pop_prefetch_job(&mut self,bound:bool,kind:Option<Kind>)->Option<(String,u64)>{
        // Include deferred entries: a full supplemental queue must not delay a
        // new Lua binding. This removes one request, never expands the queue.
        let candidate=self.queue.iter().chain(self.masks.iter()).chain(self.animations.iter())
            .map(|(p,t)|(p,*t))
            .chain(self.entries.iter().filter(|(_,e)|e.pending&&e.deferred).map(|(p,e)|(p,e.ticket)))
            .filter(|(p,t)|self.entries.get(*p).is_some_and(|e|e.pending&&e.ticket==*t&&(e.refs>0)==bound&&kind.is_none_or(|k|e.kind==k)))
            .min_by_key(|(p,t)|if bound{self.entries[*p].bind_order}else{*t}).map(|(p,t)|(p.clone(),t));
        let (path,ticket)=candidate?;
        self.remove_queued(&path);
        self.entries.get_mut(&path).unwrap().deferred=false;
        Some((path,ticket))
    }
    fn clear_queues(&mut self){self.queue.clear();self.masks.clear();self.animations.clear();self.urgent=None;self.scene_priority.clear();self.story_priority.clear();self.story_managed.clear();self.script_burst=0;}
    fn kind_parts(&self,kind:Kind)->CacheParts{
        self.entries.values().filter(|e|e.kind==kind).filter_map(|e|e.payload.as_ref()).fold(CacheParts::default(),|mut sum,p|{sum.add(p.parts());sum})
    }
    fn ready_parts(&self)->CacheParts{
        self.entries.values().filter_map(|e|e.payload.as_ref()).fold(CacheParts::default(),|mut sum,p|{sum.add(p.parts());sum})
    }
    fn update_reservation(&mut self,account:&mut crate::image_cache_budget::CacheBudget){
        account.mask_parts=self.kind_parts(Kind::Mask);
        account.animation_parts=self.kind_parts(Kind::Animation);
        account.script_preload=self.script_preload_counts();
        if !self.entries.values().any(|e|e.pending){self.reserve_requested=false;}
        account.request_ready(self.reserve_requested);
    }
    fn script_preload_counts(&self)->crate::image_cache_budget::ScriptPreloadCounts{
        let mut counts=crate::image_cache_budget::ScriptPreloadCounts::default();
        for (_,e) in self.entries.iter().filter(|(p,e)|e.refs>0&&!e.story_suppressed&&!crate::ui_image_lifetime::transient_menu_image(p)){
            counts.planned+=1;
            counts.completed+=usize::from(e.prepared);
            match e.payload{
                Some(Payload::Gray(..)|Payload::Pixels(..))=>counts.pixels+=1,
                Some(Payload::Encoded(..))=>counts.encoded+=1,
                None=>{},
            }
        }
        counts
    }
}
struct Shared { state: Mutex<State>, wake: Condvar, cache:Option<SharedCacheBudget>,budget:usize }
pub(super) struct Loader { shared: Arc<Shared>, worker: Mutex<Option<JoinHandle<()>>> }
impl Loader {
    #[cfg(test)]
    pub fn new(load: impl Fn(&str, &dyn Fn() -> bool) -> Option<Payload> + Send + 'static) -> std::io::Result<Self> {
        Self::with_budget(load, BUDGET)
    }
    #[cfg(test)]
    fn with_budget(load: impl Fn(&str, &dyn Fn() -> bool) -> Option<Payload> + Send + 'static, budget: usize) -> std::io::Result<Self> {
        Self::with_policy(load,budget,None)
    }
    #[cfg(test)]
    fn with_policy(load: impl Fn(&str, &dyn Fn() -> bool) -> Option<Payload> + Send + 'static, budget: usize, cache:Option<SharedCacheBudget>) -> std::io::Result<Self> {
        Self::with_steps(move|p,resume,c,_,_|{assert!(resume.is_none());LoadStep::Complete(load(p,c))},budget,cache)
    }
    fn with_steps(load: impl Fn(&str,Option<Tracked<Vec<u8>>>,&dyn Fn()->bool,&dyn Fn(usize)->bool,usize)->LoadStep + Send + 'static, budget:usize,cache:Option<SharedCacheBudget>)->std::io::Result<Self>{
        let shared = Arc::new(Shared { state: Mutex::new(State::default()), wake: Condvar::new(),cache,budget });
        let s = shared.clone();
        let worker = std::thread::Builder::new().name("surface-loader".into()).stack_size(512 * 1024).spawn(move || {
          #[cfg(target_os = "vita")]
          {
            unsafe extern "C" { fn sceKernelChangeThreadPriority(id: i32, priority: i32) -> i32; }
            let result = unsafe { sceKernelChangeThreadPriority(0, 180) };
            crate::core_info!("[surface-prefetch] thread priority=180 result={}", result);
          }
          crate::ffi::worker_started(c"surface-loader");
          let mut paused:Option<(String,u64,Tracked<Vec<u8>>)>=None;
          loop {
            let mut state = s.state.lock().unwrap();
            if paused.as_ref().is_some_and(|(p,t,_)|!state.entries.get(p).is_some_and(|e|e.pending&&e.ticket==*t)){
                paused=None;state.parked=None;
            }
            while state.queue_len()==0 && paused.is_none() && !state.stop { state = s.wake.wait(state).unwrap(); }
            if state.stop { break; }
            let (job,priority)=if let Some(job)=state.pop_priority_job(){(Some(job),true)}
                else if let Some((p,t,_))=&paused{(Some((p.clone(),*t)),false)}
                else{(state.pop_job(),false)};
            let Some((path,ticket))=job else{continue;};
            if !state.entries.get(&path).is_some_and(|e| e.pending && e.ticket == ticket) { continue; }
            let resume=if paused.as_ref().is_some_and(|(p,t,_)|p==&path&&*t==ticket){
                state.parked=None;state.remove_queued(&path);
                let (_,_,bytes)=paused.take().unwrap();
                crate::core_info!("[surface-prefetch] resume path={} source_bytes={} ticket={}",path,bytes.capacity(),ticket);
                Some(bytes)
            }else{None};
            let work_budget=BUDGET-paused.as_ref().map_or(0,|(_,_,b)|b.capacity());
            // An urgent batch runs to completion before resuming speculation.
            let allow_yield=!priority&&paused.is_none();
            state.active=Some((path.clone(),ticket));
            refill(&mut state);
            drop(state);
            let start = std::time::Instant::now();
            let cancelled = || {
                let state = s.state.lock().unwrap();
                state.stop || !state.entries.get(&path).is_some_and(|e| e.pending && e.ticket == ticket)
            };
            let should_yield=|bytes:usize|allow_yield&&bytes<=MAX_PAUSED_SOURCE&&s.state.lock().unwrap().should_yield(&path,ticket);
            let step=if cancelled(){LoadStep::Complete(None)}else{load(&path,resume,&cancelled,&should_yield,work_budget)};
            let mut state = s.state.lock().unwrap();
            state.active=None;
            let mut payload=match step{
                LoadStep::Complete(p)=>p,
                LoadStep::Paused(bytes)=>{
                    if !state.stop&&state.entries.get(&path).is_some_and(|e|e.pending&&e.ticket==ticket){
                        assert!(paused.is_none()&&bytes.capacity()<=MAX_PAUSED_SOURCE);
                        crate::core_info!("[surface-prefetch] yield path={} source_bytes={} ticket={} elapsed_us={}",path,bytes.capacity(),ticket,start.elapsed().as_micros());
                        state.parked=Some((path.clone(),ticket));paused=Some((path,ticket,bytes));
                    }
                    // Pending stays true: consumers must never see a partial
                    // decode as a missing file or a completed encoded fallback.
                    s.wake.notify_all();continue;
                }
            };
            if state.entries.get(&path).is_some_and(|e| e.pending && e.ticket == ticket) && !state.stop {
                // Lock order: loader state -> retention budget. Provider retain
                // never takes loader state and never invokes a loader callback.
                let mut account=s.cache.as_ref().map(|b|b.lock().unwrap());
                let budget=account.as_ref().map_or(budget,|b|b.ready_limit());
                let kind=state.entries[&path].kind;
                let loaded_bytes=payload.as_ref().map_or(0,Payload::bytes);
                let mut class_used=state.kind_parts(kind).total();
                if kind!=Kind::Image&&state.entries[&path].demanded{
                    let needed=payload.as_ref().map_or(0,Payload::bytes);
                    let mut older:Vec<_>=state.entries.iter().filter(|(p,e)|p.as_str()!=path&&e.kind==kind&&!e.demanded&&e.payload.is_some()).map(|(p,e)|(e.ticket,p.clone())).collect();older.sort_unstable();
                    if needed<=kind.cap(){
                        for (_,p) in &older{if class_used+needed<=kind.cap(){break;}class_used-=demote(&mut state.entries.get_mut(p).unwrap().payload);}
                        for (_,p) in older{if class_used+needed<=kind.cap(){break;}class_used-=state.entries.get_mut(&p).unwrap().payload.take().map_or(0,|p|p.bytes());}
                    }
                }
                let class_free=kind.cap().saturating_sub(class_used);
                if payload.as_ref().is_some_and(|p|p.bytes()>class_free){demote(&mut payload);}
                if payload.as_ref().is_some_and(|p|p.bytes()>class_free){payload=None;}
                // Keep the compressed source when large RGBA buffers are reclaimed.
                if payload.as_ref().is_some_and(|p| p.bytes() > budget) { demote(&mut payload); }
                if payload.as_ref().is_some_and(|p| p.bytes() > budget) { payload = None; }
                let bytes = payload.as_ref().map_or(0, Payload::bytes);
                let used: usize = state.entries.values().filter_map(|e| e.payload.as_ref()).map(Payload::bytes).sum();
                let mut total = used.saturating_add(bytes);
                let mut old: Vec<_> = state.entries.iter().filter(|(_, e)| e.payload.is_some() && !e.demanded).map(|(p,e)| (e.ticket,p.clone())).collect();
                old.sort_unstable();
                // A later speculative request must not destroy completed,
                // still-bound first-use payloads to admit itself. Native surfaces
                // distinguish referenced objects from the reclaimable cache;
                // our hard ready cap instead declines pixel admission here.
                // Foreground demand keeps the existing priority/reclaim path.
                let speculative=state.entries.get(&path).is_some_and(|e|e.speculative&&!e.demanded);
                if speculative&&state.entries[&path].refs>0&&total>budget{
                    // Script-directed first use may borrow pixels from chapter
                    // supplements, never the reverse. Keep their encoded source
                    // and reclaim the most recently queued supplements first.
                    for (_,p) in old.iter().rev(){
                        if total<=budget{break;}
                        if state.entries[p].refs==0{total-=demote(&mut state.entries.get_mut(p).unwrap().payload);}
                    }
                }
                if speculative && total>budget {
                    let released=demote(&mut payload);total-=released;
                    if released>0&&state.demotion_samples<64{
                        state.demotion_samples+=1;
                        crate::core_info!("[surface-prefetch] demote path={} pixels_released={} ready_limit={} idle_reserved={} reason=incoming-speculative",path,released,budget,account.as_ref().map_or(0,|b|b.idle));
                    }
                    // Encoded fallbacks also protect first use and carry alpha
                    // certificates. Do not evict them speculatively, especially
                    // before knowing whether the new source can be admitted.
                    if total>budget{
                        let declined=payload.take().map_or(0,|p|p.bytes());total-=declined;
                        if declined>0&&state.demotion_samples<64{
                            state.demotion_samples+=1;
                            crate::core_info!("[surface-prefetch] admission-declined path={} bytes={} ready_limit={} reason=preserve-bound-payloads",path,declined,budget);
                        }
                    }
                }
                for (_, p) in &old {
                    if total <= budget { break; }
                    let released=demote(&mut state.entries.get_mut(p).unwrap().payload);
                    total -= released;
                    if released>0&&state.demotion_samples<64{
                        state.demotion_samples+=1;
                        crate::core_info!("[surface-prefetch] demote path={} pixels_released={} ready_limit={} idle_reserved={}",p,released,budget,account.as_ref().map_or(0,|b|b.idle));
                    }
                }
                if total > budget { total -= demote(&mut payload); }
                for (_, p) in old {
                    if total <= budget { break; }
                    if let Some(data) = state.entries.get_mut(&p).unwrap().payload.take() { total -= data.bytes(); }
                }
                if total > budget { payload = None; } // Preserve a foreground consumer's ready result.
                let entry = state.entries.get_mut(&path).unwrap();
                entry.pending = false;
                // Admission failure is not a missing file. Remember only the
                // required capacity, never a second pixel/source allocation.
                entry.retry_bytes=if speculative&&payload.is_none(){loaded_bytes}else{0};
                if let Some(p)=payload.as_mut(){p.ready();}
                if payload.is_some(){entry.prepared=true;}
                entry.payload = payload;
                let payload_kind=match &entry.payload {Some(Payload::Gray(..))=>"gray8",Some(Payload::Pixels(..))=>"pixels",Some(Payload::Encoded(..))=>"encoded",None if entry.retry_bytes>0=>"capacity-deferred",None=>"missing"};
                let retained_bytes=entry.payload.as_ref().map_or(0,Payload::bytes);
                let parts=state.ready_parts();let cache_bytes=parts.total();
                if let Some(b)=account.as_mut(){b.set_ready_parts(parts);state.update_reservation(b);}
                crate::core_info!("[surface-prefetch] ready path={} bytes={} elapsed_us={} payload={} retained_bytes={} cache_bytes={} cache_budget={} kind={} kind_bytes={} kind_limit={}",
                    path, bytes, start.elapsed().as_micros(),payload_kind,retained_bytes,cache_bytes,budget,kind.label(),state.kind_parts(kind).total(),kind.cap());
                retry_capacity(&mut state,budget);
                if let Some(b)=account.as_mut(){state.update_reservation(b);}
            }
            s.wake.notify_all();
          }
        })?;
        Ok(Self { shared, worker: Mutex::new(Some(worker)) })
    }
    pub fn bind(&self, path: &str, asynchronous: bool) {
        self.request(path,asynchronous,Kind::Image,false);
    }
    pub fn begin_chapter(&self,chapter:&str){
        let mut state=self.shared.state.lock().unwrap();
        if state.chapter==chapter{return;}
        state.chapter=chapter.into();
        state.story_priority.clear();state.story_managed.clear();
        state.script_burst=0;
        for e in state.entries.values_mut(){e.leased=false;}
        state.entries.retain(|_,e|e.refs>0);
        let live:std::collections::HashSet<_>=state.entries.keys().cloned().collect();
        state.queue.retain(|(p,_)|live.contains(p));
        state.masks.retain(|(p,_)|live.contains(p));
        state.animations.retain(|(p,_)|live.contains(p));
        refill(&mut state);self.update_ready_account(&mut state);self.shared.wake.notify_all();
    }
    pub fn preload(&self,path:&str,kind:Kind){self.request(path,true,kind,true);}
    pub fn prioritize_scene(&self,paths:&[String])->usize{
        let mut state=self.shared.state.lock().unwrap();
        state.scene_priority.clear();
        if state.stop{return 0;}
        let mut seen=std::collections::HashSet::new();
        for path in paths{
            let Some(key)=[path.clone(),format!("{path}.png"),format!("{path}.jpg"),format!("{path}.jpeg")]
                .into_iter().find(|p|state.entries.contains_key(p)) else{continue;};
            let entry=&state.entries[&key];
            if !entry.pending||!seen.insert(key.clone()){continue;}
            let ticket=entry.ticket;
            state.scene_priority.push_back((key,ticket));
        }
        let count=state.scene_priority.len();
        if count>0{self.shared.wake.notify_one();}
        count
    }
    // Retire only direct image bindings previously observed in the standard
    // script timeline. Dynamic/IPT/effect bindings retain their original policy.
    // A suppressed binding still exists for Lua and can be demanded at any time.
    pub fn update_story_plan(&self,future:&[String],current:&[String]) {
        let mut state=self.shared.state.lock().unwrap();
        if state.stop{return;}
        let key=|p:&String|[p.clone(),format!("{p}.png"),format!("{p}.jpg"),format!("{p}.jpeg")]
            .into_iter().find(|p|state.entries.contains_key(p));
        let mut seen=std::collections::HashSet::new();
        let future:Vec<_>=future.iter().filter_map(&key).filter(|p|{
            let e=&state.entries[p];e.refs>0&&e.kind==Kind::Image
                &&!crate::ui_image_lifetime::transient_menu_image(p)&&seen.insert(p.clone())
        }).collect();
        let protected:std::collections::HashSet<_>=current.iter().filter_map(key).collect();
        state.story_managed.extend(future.iter().cloned());
        let stale:Vec<_>=state.story_managed.iter().filter(|p|!seen.contains(*p)&&!protected.contains(*p)
            &&state.entries.get(*p).is_some_and(|e|!e.demanded&&!e.story_suppressed)) .cloned().collect();
        let mut released=0;
        for path in &stale {
            let e=state.entries.get_mut(path).unwrap();
            released+=e.payload.take().map_or(0,|p|p.bytes());
            e.pending=false;e.deferred=false;e.retry_bytes=0;e.prepared=false;e.story_suppressed=true;
            state.remove_queued(path);
        }
        state.story_priority.clear();
        for path in future {
            if state.entries[&path].story_suppressed {
                state.serial+=1;let ticket=state.serial;
                let e=state.entries.get_mut(&path).unwrap();
                e.story_suppressed=false;e.ticket=ticket;e.pending=true;e.deferred=true;e.speculative=true;
            }
            let e=&state.entries[&path];
            if e.pending {let ticket=e.ticket;state.story_priority.push_back((path,ticket));}
        }
        refill(&mut state);self.update_ready_account(&mut state);self.refill_capacity(&mut state);
        self.shared.wake.notify_all();
        crate::core_info!("[surface-prefetch] story-plan future={} pending={} retired={} released_bytes={}",
            seen.len(),state.story_priority.len(),stale.len(),released);
    }
    fn request(&self,path:&str,asynchronous:bool,kind:Kind,lease:bool){
        let mut state = self.shared.state.lock().unwrap();
        if state.stop { return; }
        let already_leased=state.entries.get(path).is_some_and(|e|e.leased);
        let first_binding=!lease&&state.entries.get(path).is_none_or(|e|e.refs==0);
        if lease&&!already_leased&&state.entries.values().filter(|e|e.leased&&e.kind==kind).count()>=128{return;}
        if let Some(e) = state.entries.get_mut(path) { if lease{e.leased=true;}else{e.refs+=1;} }
        else { state.entries.insert(path.into(), Entry { refs:usize::from(!lease),ticket:0,bind_order:0,prepared:false,story_suppressed:false,pending:false,deferred:false,demanded:false,speculative:false,payload:None,kind,leased:lease,retry_bytes:0 }); }
        // A chapter hint can precede its Lua binding. Track script order apart
        // from the cancellation ticket; never invalidate an in-flight load.
        if first_binding{state.serial+=1;let order=state.serial;state.entries.get_mut(path).unwrap().bind_order=order;}
        // Classification is fixed when a result is admitted; don't move a large
        // completed ordinary result into a smaller lane without accounting it.
        if state.entries[path].kind==Kind::Image&&kind!=Kind::Image&&state.entries[path].payload.is_none(){
            state.entries.get_mut(path).unwrap().kind=kind;
            if let Some(job)=state.remove_queued(path){state.queue_mut(kind).push_back(job);}
        }
        state.entries.get_mut(path).unwrap().story_suppressed=false;
        if !asynchronous {state.entries.get_mut(path).unwrap().speculative=false;}
        // System-cache bindings for these menus keep reference bookkeeping,
        // but no background IO or READY allocation while the menu is closed.
        let lazy_ui=asynchronous&&crate::ui_image_lifetime::transient_menu_image(path);
        let needs_job=!lazy_ui&&!(lease&&already_leased)&&state.entries.get(path).is_some_and(|e|!e.pending && e.payload.is_none());
        if needs_job {
            state.serial += 1;let ticket=state.serial;
            let e=state.entries.get_mut(path).unwrap();e.ticket=ticket;e.prepared=false;e.pending=true;e.deferred=true;e.speculative=asynchronous;e.retry_bytes=0;
            refill(&mut state);self.shared.wake.notify_one();
        }
        if asynchronous&&needs_job{state.reserve_requested=true;}
        self.update_ready_account(&mut state);
        drop(state);
        if !asynchronous { self.wait(path); }
    }
    fn wait(&self, path: &str) {
        let start = std::time::Instant::now();
        let mut state = self.shared.state.lock().unwrap();
        // Promote an immediately needed image ahead of speculative requests.
        promote(&mut state,path);
        while state.entries.get(path).is_some_and(|e| e.pending) && !state.stop {
            state = self.shared.wake.wait(state).unwrap();
        }
        drop(state);
        if start.elapsed().as_micros() >= 1000 { crate::core_info!("[surface-prefetch] demand-wait path={} us={}", path, start.elapsed().as_micros()); }
    }
    fn key(&self, path: &str) -> Option<String> {
        let state = self.shared.state.lock().unwrap();
        [path.to_string(), format!("{path}.png"), format!("{path}.jpg"), format!("{path}.jpeg")].into_iter().find(|p| state.entries.contains_key(p))
    }
    pub fn take(&self, path: &str) -> Option<Payload> {
        let key = self.key(path)?;
        let started = std::time::Instant::now();
        let mut state = self.shared.state.lock().unwrap();
        if (crate::ui_image_lifetime::transient_menu_image(&key)||state.entries[&key].story_suppressed)
            && state.entries.get(&key).is_some_and(|e|!e.pending&&e.payload.is_none()) {
            state.serial+=1;
            let ticket=state.serial;
            let e=state.entries.get_mut(&key)?;
            e.ticket=ticket;e.pending=true;e.deferred=true;e.speculative=false;e.retry_bytes=0;e.story_suppressed=false;
            refill(&mut state);self.shared.wake.notify_one();
        }
        state.entries.get_mut(&key)?.demanded = true;
        // If demand arrives before a refill, the normal synchronous fallback
        // owns this attempt. Do not reload that consumed binding later.
        state.entries.get_mut(&key)?.retry_bytes = 0;
        promote(&mut state,&key);
        while state.entries.get(&key).is_some_and(|e| e.pending) && !state.stop {
            state = self.shared.wake.wait(state).unwrap();
        }
        let entry = state.entries.get_mut(&key)?;
        entry.demanded = false;
        let result = entry.payload.take();
        self.update_ready_account(&mut state);
        self.refill_capacity(&mut state);
        drop(state);
        if started.elapsed().as_micros() >= 1000 { crate::core_info!("[surface-prefetch] demand-wait path={} us={}",path,started.elapsed().as_micros()); }
        result
    }
    /// Nonblocking handoff for render-thread staging. Atomically move CPU
    /// ownership READY -> IDLE so the worker cannot spend those bytes again.
    /// Never starts a job, decodes, waits, or consumes masks/menu/encoded data.
    pub fn take_warm_pixels(&self, path: &str, max_bytes: usize) -> Option<Payload> {
        let mut state = self.shared.state.try_lock().ok()?;
        let key = [path.to_owned(), format!("{path}.png"), format!("{path}.jpg"), format!("{path}.jpeg")]
            .into_iter().find(|p| state.entries.contains_key(p))?;
        let entry = state.entries.get(&key)?;
        if state.stop || entry.pending || entry.demanded || entry.story_suppressed
            || entry.refs == 0 || entry.kind != Kind::Image
            || crate::ui_image_lifetime::transient_menu_image(&key) { return None; }
        let Some(Payload::Pixels(p, _, Some(_))) = &entry.payload else { return None; };
        if p.width() == 0 || p.height() == 0 || p.width() > 4096 || p.height() > 4096
            || entry.payload.as_ref()?.bytes() > max_bytes { return None; }
        let cache = self.shared.cache.as_ref()?;
        let mut account = cache.try_lock().ok()?;
        let result = state.entries.get_mut(&key)?.payload.take()?;
        account.set_ready_parts(state.ready_parts());
        let idle = account.idle + result.bytes();
        account.set_idle(idle);
        state.update_reservation(&mut account);
        Some(result)
    }
    pub fn retain_menu_images(&self, paths:&std::collections::HashSet<String>) {
        let mut state=self.shared.state.lock().unwrap();
        let stale:Vec<_>=state.entries.iter().filter(|(p,e)|
            crate::ui_image_lifetime::transient_menu_image(p)&&!e.demanded
            &&!paths.contains(*p)&&!paths.contains(p.strip_suffix(".png").unwrap_or(p))
            &&(e.pending||e.payload.is_some()||e.retry_bytes>0))
            .map(|(p,_)|p.clone()).collect();
        if stale.is_empty(){return;}
        let mut released=0;
        for path in &stale {
            let e=state.entries.get_mut(path).unwrap();
            released+=e.payload.take().map_or(0,|p|p.bytes());
            e.pending=false;e.deferred=false;e.retry_bytes=0;e.prepared=false;
            state.remove_queued(path);
        }
        refill(&mut state);self.update_ready_account(&mut state);self.refill_capacity(&mut state);
        self.shared.wake.notify_all();
        crate::core_info!("[menu-image-release] stage=ready entries={} bytes={}",stale.len(),released);
    }
    pub fn loading(&self, path: Option<&str>) -> bool {
        let key = path.and_then(|p| self.key(p));
        let state = self.shared.state.lock().unwrap();
        if path.is_some() { key.as_ref().and_then(|p| state.entries.get(p)).is_some_and(|e| e.pending) }
        else { state.entries.values().any(|e| e.pending&&e.refs>0) }
    }
    pub fn unbind(&self, path: &str) {
        let mut state = self.shared.state.lock().unwrap();
        if let Some(e) = state.entries.get_mut(path) {
            e.refs = e.refs.saturating_sub(1);
            if e.refs == 0&&!e.leased { state.entries.remove(path); state.remove_queued(path); }
        }
        self.update_ready_account(&mut state);
        refill(&mut state);
        self.refill_capacity(&mut state);
        self.shared.wake.notify_all();
    }
    pub fn cancel(&self) {
        let mut state = self.shared.state.lock().unwrap();
        state.clear_queues();
        let active=state.active.clone();
        let parked=state.parked.clone();
        for (path,e) in state.entries.iter_mut() { if e.leased{e.deferred=e.pending&&!active.as_ref().is_some_and(|(p,t)|p==path&&*t==e.ticket)&&!parked.as_ref().is_some_and(|(p,t)|p==path&&*t==e.ticket);}else{e.pending = false; e.deferred=false;e.retry_bytes=0;} }
        refill(&mut state); // Chapter preloads have a separate lifetime from script bindings.
        self.update_ready_account(&mut state);
        self.shared.wake.notify_all(); // Active request is discarded by ticket/pending check.
    }
    fn shutdown(&self) {
        { let mut state = self.shared.state.lock().unwrap(); state.stop = true; state.clear_queues(); state.entries.clear(); self.update_ready_account(&mut state);self.shared.wake.notify_all(); }
        if let Some(worker) = self.worker.lock().unwrap().take() { let _ = worker.join(); }
    }
    fn refill_capacity(&self,state:&mut State){
        let mut account=self.shared.cache.as_ref().map(|b|b.lock().unwrap());
        let budget=account.as_ref().map_or(self.shared.budget,|b|b.ready_limit());
        if retry_capacity(state,budget){
            if let Some(b)=account.as_mut(){state.update_reservation(b);}
            self.shared.wake.notify_all();
        }
    }
    fn update_ready_account(&self,state:&mut State){
        if let Some(cache)=&self.shared.cache{
            let mut account=cache.lock().unwrap();
            account.set_ready_parts(state.ready_parts());
            state.update_reservation(&mut account);
        }
    }
}
impl Drop for Loader { fn drop(&mut self) { self.shutdown(); } }
// Refill only an idle worker, one known-size declined item at a time. Waiting
// for capacity is not a pending load: wait-all must still terminate when the
// bound chapter exceeds the budget. Consumption/unbind wakes this path; there
// is no timer, spin, extra queue, or eviction of still-bound first-use pixels.
fn retry_capacity(state:&mut State,budget:usize)->bool {
    if state.stop||state.active.is_some()||state.entries.values().any(|e|e.pending){return false;}
    let free=budget.saturating_sub(state.ready_parts().total());
    let mask_free=Kind::Mask.cap().saturating_sub(state.kind_parts(Kind::Mask).total());
    let animation_free=Kind::Animation.cap().saturating_sub(state.kind_parts(Kind::Animation).total());
    let candidate=state.entries.iter().filter(|(_,e)|e.retry_bytes>0&&e.retry_bytes<=free&&e.payload.is_none()
        &&(e.refs>0||e.leased)&&e.retry_bytes<=match e.kind{Kind::Image=>usize::MAX,Kind::Mask=>mask_free,Kind::Animation=>animation_free})
        .min_by_key(|(_,e)|(e.refs==0,if e.refs>0{e.bind_order}else{e.ticket})).map(|(p,_)|p.clone());
    let Some(path)=candidate else{return false;};
    let e=state.entries.get_mut(&path).unwrap();
    crate::core_info!("[surface-prefetch] capacity-refill path={} required={} free={} ticket={}",path,e.retry_bytes,free,e.ticket);
    e.retry_bytes=0;e.pending=true;e.deferred=true;e.speculative=true;
    // This retry already fits free capacity. Do not reserve a new chapter's
    // 5/6 budget and evict warm provider surfaces merely for one retry.
    refill(state);
    true
}
// Deferred requests live in existing binding records, not an unbounded job queue.
fn refill(state:&mut State) {
    if state.stop || state.queue_len() >= MAX_QUEUE { return; }
    let mut deferred:Vec<_>=state.entries.iter().filter(|(_,e)|e.pending&&e.deferred)
        .map(|(p,e)|(e.refs==0,e.ticket,p.clone())).collect();deferred.sort_unstable();
    for (_,ticket,path) in deferred.into_iter().take(MAX_QUEUE-state.queue_len()) {
        let e=state.entries.get_mut(&path).unwrap();e.deferred=false;let kind=e.kind;state.queue_mut(kind).push_back((path,ticket));
    }
}
fn promote(state:&mut State,path:&str) {
    if state.parked.as_ref().is_some_and(|(p,t)|p==path&&state.entries.get(p).is_some_and(|e|e.pending&&e.ticket==*t)){
        state.urgent=Some(path.into());return;
    }
    if let Some(job)=state.remove_queued(path){let kind=state.entries[path].kind;state.queue_mut(kind).push_front(job);state.urgent=Some(path.into());}
    else if state.entries.get(path).is_some_and(|e|e.pending&&e.deferred){
        if state.queue_len()==MAX_QUEUE {
            let (p,_)=state.queue.pop_back().or_else(||state.animations.pop_back()).or_else(||state.masks.pop_back()).unwrap();state.entries.get_mut(&p).unwrap().deferred=true;
        }
        let e=state.entries.get_mut(path).unwrap();e.deferred=false;
        let (kind,ticket)=(e.kind,e.ticket);state.queue_mut(kind).push_front((path.into(),ticket));state.urgent=Some(path.into());
    }
}

struct CancelReader<'a> { cursor: std::io::Cursor<&'a [u8]>, cancelled: &'a dyn Fn()->bool }
impl std::io::Read for CancelReader<'_> {
    fn read(&mut self, out:&mut [u8])->std::io::Result<usize>{
        // Interrupted is retried by read_exact; use a terminal error instead.
        if (self.cancelled)(){return Err(std::io::Error::other("surface prefetch cancelled"));}
        std::io::Read::read(&mut self.cursor,out)
    }
}
impl std::io::Seek for CancelReader<'_> {
    fn seek(&mut self, pos:std::io::SeekFrom)->std::io::Result<u64>{
        if (self.cancelled)(){return Err(std::io::Error::other("surface prefetch cancelled"));}
        std::io::Seek::seek(&mut self.cursor,pos)
    }
}
// Shared with foreground resolution so encoded fallback uses the same allocation protection.
fn decode_rgba(decoder: impl image::ImageDecoder, reserve: impl FnOnce(&mut Vec<u8>, usize)->bool) -> Option<image::RgbaImage> {
    crate::image_decode::rgba_with_reserve(decoder,BUDGET,reserve).ok()
}
const DECODE_READ_BUFFER: usize = 16384;
#[cfg(test)]
fn decoder_allowance(width:u32,height:u32,source_capacity:usize,pixel_storage:usize)->Option<usize> {
    decoder_allowance_with_budget(width,height,source_capacity,pixel_storage,BUDGET)
}
fn decoder_allowance_with_budget(width:u32,height:u32,source_capacity:usize,pixel_storage:usize,budget:usize)->Option<usize> {
    if width==0 || height==0 {return None;}
    let output=(width as usize).checked_mul(height as usize)?.checked_mul(pixel_storage)?;
    budget.checked_sub(source_capacity)?.checked_sub(output)?.checked_sub(DECODE_READ_BUFFER)
        .filter(|remaining|*remaining>0)
}
fn load(path:&str,resume:Option<Tracked<Vec<u8>>>,cancelled:&dyn Fn()->bool,should_yield:&dyn Fn(usize)->bool,budget:usize,comments:&super::png_comments::SharedComments)->LoadStep{
    if let Some(bytes)=resume{return decode_source(bytes,cancelled,should_yield,budget);}
    let bytes=(||{
    for suffix in [".png", "", ".jpg", ".jpeg"] {
        if cancelled(){return None;}
        let candidate = format!("{path}{suffix}");
        let metadata_epoch=super::png_comments::prepare_epoch(comments,&candidate);
        let Some(size) = crate::ffi::query_asset_size(&candidate) else { continue; };
        if cancelled(){return None;}
        if size > budget as u64 { return None; } // Synchronous source remains the fallback.
        // Size is already known: avoid a second archive/directory size lookup.
        let mut charge=Charge::reserve(Owner::Source,size as usize);
        let Some(bytes) = crate::ffi::request_asset_range(&candidate, 0, size as usize) else { continue; };
        if cancelled(){return None;}
        if bytes.len() != size as usize { continue; }
        charge.commit(bytes.capacity());
        let bytes=Tracked{data:bytes,charge};
        if let Some(epoch)=metadata_epoch{
            super::png_comments::prepare_loaded(comments,&candidate,bytes.as_slice(),epoch,cancelled);
        }
        if cancelled(){return None;}
        return Some(bytes);
    }
    None
    })();
    match bytes{Some(b)=>decode_source(b,cancelled,should_yield,budget),None=>LoadStep::Complete(None)}
}
fn decode_source(bytes:Tracked<Vec<u8>>,cancelled:&dyn Fn()->bool,should_yield:&dyn Fn(usize)->bool,budget:usize)->LoadStep{
        // Sticky interruption avoids restarting within the same decoder if the
        // scene changes again between its reads. Only compressed data survives.
        let yielded=std::cell::Cell::new(false);
        let interrupted=||{
            if cancelled(){return true;}
            if yielded.get(){return true;}
            if should_yield(bytes.capacity()){yielded.set(true);return true;}
            false
        };
        let decoded=(||{
        if interrupted(){return None;}
        // Only gray8 PNG is a candidate. Decoder color_type also checks tRNS;
        // avoid opening a second decoder for ordinary RGB/palette resources.
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n")&&bytes.get(24..26)==Some(&[8,0]) {
            let w=u32::from_be_bytes(bytes[16..20].try_into().unwrap());
            let h=u32::from_be_bytes(bytes[20..24].try_into().unwrap());
            let gray=(||{
                let allowance=decoder_allowance_with_budget(w,h,bytes.capacity(),1,budget)?;
                let source=CancelReader{cursor:std::io::Cursor::new(bytes.as_slice()),cancelled:&interrupted};
                let mut reader=image::ImageReader::new(std::io::BufReader::with_capacity(DECODE_READ_BUFFER,source)).with_guessed_format().ok()?;
                let mut limits=image::Limits::default();limits.max_alloc=Some(allowance as u64);reader.limits(limits);
                let decoder=reader.into_decoder().ok()?;
                crate::resource_ledger::decode_luma(decoder,budget-bytes.capacity()-DECODE_READ_BUFFER)
            })();
            if let Some(pixels)=gray{
                return Some(DecodedSource::Gray(w,h,pixels));
            }
        }
        if interrupted(){return None;}
        let dimensions = image::ImageReader::new(std::io::Cursor::new(bytes.as_slice())).with_guessed_format().ok().and_then(|mut r| {
            let mut limits=image::Limits::default();limits.max_alloc=Some(budget as u64);r.limits(limits);
            r.into_dimensions().ok()
        });
        // A 16-bit PNG may need eight bytes/pixel before in-place reduction to RGBA8.
        let pixel_storage=if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(24)==Some(&16) {8}else{4};
        if interrupted(){return None;}
        if let Some(allowance)=dimensions.and_then(|(w,h)|decoder_allowance_with_budget(w,h,bytes.capacity(),pixel_storage,budget)) {
            let source=CancelReader{cursor:std::io::Cursor::new(bytes.as_slice()),cancelled:&interrupted};
            if let Ok(mut reader) = image::ImageReader::new(std::io::BufReader::with_capacity(DECODE_READ_BUFFER,source)).with_guessed_format() {
                // PNG copies this limit at construction; setting it afterwards is insufficient.
                let mut limits = image::Limits::default(); limits.max_alloc = Some(allowance as u64);
                reader.limits(limits);
                let decoded=reader.into_decoder().ok().and_then(|decoder|
                    crate::resource_ledger::decode_rgba(decoder,budget).ok());
                if let Some(image) = decoded {
                    // A complete decode is already useful; do not throw it
                    // away merely because priority changed after its last read.
                    if cancelled(){return None;}
                    let proof=TileProof::for_prepared_upload(&image);
                    if cancelled(){return None;}
                    return Some(DecodedSource::Pixels(image,proof));
                }
            }
        }
        None
        })();
        if cancelled(){return LoadStep::Complete(None);}
        if let Some(decoded)=decoded{return LoadStep::Complete(Some(match decoded{
            DecodedSource::Gray(w,h,p)=>Payload::Gray(w,h,p,bytes),
            DecodedSource::Pixels(p,proof)=>Payload::Pixels(p,bytes,proof),
        }));}
        if yielded.get(){LoadStep::Paused(bytes)}else{LoadStep::Complete(Some(Payload::Encoded(bytes,None)))}
}
enum DecodedSource { Gray(u32,u32,Tracked<Vec<u8>>), Pixels(Tracked<image::RgbaImage>,Option<TileProof>) }

static LOADER: Mutex<Option<Arc<Loader>>> = Mutex::new(None);
fn handle()->Option<Arc<Loader>> { LOADER.lock().unwrap().clone() }
fn worker(comments:super::png_comments::SharedComments)->Option<Arc<Loader>> {
    let worker={
        let mut loader=LOADER.lock().unwrap();
        if loader.is_none(){match Loader::with_steps(move|p,r,c,y,b|load(p,r,c,y,b,&comments),READY_BUDGET,Some(crate::image_cache_budget::session_budget())){
            Ok(worker)=>{*loader=Some(Arc::new(worker));crate::core_info!("[surface-prefetch] worker started budget={} shared_budget={} decode_limit={} queue={} deferred=1 priority=lua-first-v1 script_burst={} protect_bound_pixels=1 cooperative_decode=1 paused_source_limit={}",READY_BUDGET,crate::image_cache_budget::SESSION_RETENTION_BYTES,BUDGET,MAX_QUEUE,SCRIPT_PREFETCH_BURST,MAX_PAUSED_SOURCE);}
            Err(e)=>{crate::core_warn!("[surface-prefetch] worker unavailable: {}",e);return None;}
        }}
        loader.as_ref().unwrap().clone()
    };
    Some(worker)
}
pub(super) fn bind(path:&str,asynchronous:bool,comments:super::png_comments::SharedComments){if let Some(w)=worker(comments){w.bind(path,asynchronous);}}
pub(super) fn preload(paths:&[String],kind:Kind,chapter:Option<&str>,comments:super::png_comments::SharedComments){
    if let Some(w)=worker(comments){
        if let Some(chapter)=chapter{w.begin_chapter(chapter);}
        for p in paths{w.preload(p,kind);}
        crate::core_info!("[surface-prefetch] hints kind={} files={} chapter={} cap={}",kind.label(),paths.len(),chapter.unwrap_or("current"),kind.cap());
    }
}
pub(super) fn take(path:&str)->Option<Payload>{handle()?.take(path)}
pub(super) fn take_warm_pixels(path:&str,max:usize)->Option<Payload>{handle()?.take_warm_pixels(path,max)}
pub(super) fn update_story_plan(future:&[String],current:&[String]){if let Some(l)=handle(){l.update_story_plan(future,current);}}
pub(super) fn retain_menu_images(paths:&std::collections::HashSet<String>){if let Some(l)=handle(){l.retain_menu_images(paths);}}
pub(super) fn prioritize_scene(paths:&[String]){
    if let Some(loader)=handle(){
        let count=loader.prioritize_scene(paths);
        if count>0{crate::core_info!("[surface-prefetch] scene-priority pending={} resources={}",count,paths.len());}
    }
}
pub(super) fn loading(path:Option<&str>)->bool{handle().is_some_and(|l|l.loading(path))}
pub(super) fn unbind(path:&str){if let Some(l)=handle(){l.unbind(path);}}
pub(super) fn cancel(){if let Some(l)=handle(){l.cancel();}}
pub(super) fn shutdown(){let loader=LOADER.lock().unwrap().take();if let Some(l)=loader{l.shutdown();}}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn warm_handoff_is_nonblocking_filtered_and_transfers_budget_once() {
        let budget=crate::image_cache_budget::CacheBudget::new(4096);
        let loader=Loader::with_policy(|name,_| {
            if name=="compressed" {return Some(Payload::encoded(vec![7;16]));}
            let image=image::RgbaImage::from_pixel(4,4,image::Rgba([8,9,10,128]));
            let proof=TileProof::for_prepared_upload(&image);
            Some(Payload::Pixels(image.into(),vec![1;8].into(),proof))
        },4096,Some(budget.clone())).unwrap();
        for path in ["future.png","compressed","pc/en/save/button.png"] {loader.bind(path,false);}
        let before=budget.lock().unwrap().ready;
        assert!(loader.take_warm_pixels("missing",4096).is_none());
        assert!(loader.take_warm_pixels("compressed",4096).is_none());
        assert!(loader.take_warm_pixels("pc/en/save/button.png",4096).is_none());
        assert!(loader.take_warm_pixels("future",1).is_none());
        {let _held=loader.shared.state.lock().unwrap();assert!(loader.take_warm_pixels("future",4096).is_none());}
        {let _held=budget.lock().unwrap();assert!(loader.take_warm_pixels("future",4096).is_none());}
        let warm=loader.take_warm_pixels("future",4096).unwrap();let bytes=warm.bytes();
        assert!(loader.take_warm_pixels("future",4096).is_none());
        {let b=budget.lock().unwrap();assert_eq!(b.ready,before-bytes);assert_eq!(b.idle,bytes);}
        assert!(loader.take("future").is_none());
        loader.shutdown();let mut b=budget.lock().unwrap();assert_eq!(b.ready,0);b.set_idle(0);
    }
    #[test]
    fn menu_bindings_are_lazy_and_reopen_without_accumulating_references() {
        use std::sync::atomic::{AtomicUsize,Ordering};
        let reads=Arc::new(AtomicUsize::new(0));let count=reads.clone();
        let budget=crate::image_cache_budget::CacheBudget::new(1024);
        let loader=Loader::with_policy(move|_,_|{count.fetch_add(1,Ordering::SeqCst);Some(Payload::encoded(vec![3;32]))},1024,Some(budget.clone())).unwrap();
        for path in ["pc/cn/conf/bg.png","pc/cn/save/bg_save.png","pc/cn/save/bg_load.png"] {
        let before=reads.load(Ordering::SeqCst);
        loader.bind(path,true);
        assert!(!loader.loading(None));assert_eq!(reads.load(Ordering::SeqCst),before);
        assert_eq!(budget.lock().unwrap().ready,0);
        assert_eq!(budget.lock().unwrap().script_preload.planned,0);
        for _ in 0..3 {
            assert_eq!(loader.take(path).unwrap().bytes(),32);
            loader.retain_menu_images(&Default::default());
            assert_eq!(loader.shared.state.lock().unwrap().entries[path].refs,1);
            assert_eq!(budget.lock().unwrap().ready,0);
        }
        assert_eq!(reads.load(Ordering::SeqCst),before+3);
        loader.unbind(path);assert!(loader.key(path).is_none());
        }
    }
    #[test]
    fn menu_release_preserves_scene_references_and_other_preloads() {
        let budget=crate::image_cache_budget::CacheBudget::new(1024);
        let loader=Loader::with_policy(|_,_|Some(Payload::encoded(vec![7;32])),1024,Some(budget.clone())).unwrap();
        let path="vita/ja/blog/bg.png";
        loader.bind(path,false);loader.bind("image/bg/room",false);
        loader.retain_menu_images(&std::collections::HashSet::from(["vita/ja/blog/bg".into()]));
        assert_eq!(budget.lock().unwrap().ready,64);
        loader.retain_menu_images(&Default::default());
        assert_eq!(budget.lock().unwrap().ready,32);
        assert!(!loader.loading(None));
        assert!(loader.take("image/bg/room").is_some());
        assert!(loader.take(path).is_some());
    }
    #[test]
    fn closing_menu_cancels_inflight_warmup_without_republishing_old_pixels() {
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let loader=Arc::new(Loader::with_policy(move|_,cancelled|{
            started_tx.send(()).unwrap();go_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
            assert!(cancelled());Some(Payload::encoded(vec![4;32]))
        },1024,None).unwrap());
        let path="pc/cn/blog/bg.png";
        let other=loader.clone();let waiter=std::thread::spawn(move||other.bind(path,false));
        started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.retain_menu_images(&Default::default());
        waiter.join().unwrap();assert!(!loader.loading(None));
        go_tx.send(()).unwrap();
        loader.shutdown();
        assert!(loader.shared.state.lock().unwrap().entries.is_empty());
    }
    #[test]
    fn paused_source_stays_pending_and_resumes_after_urgent_batch_without_reread(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let (body_tx,body_rx)=std::sync::mpsc::channel();
        let (body_go_tx,body_go_rx)=std::sync::mpsc::channel();
        let (done_tx,done_rx)=std::sync::mpsc::channel();
        let source_address=Arc::new(std::sync::atomic::AtomicUsize::new(0));let ptr=source_address.clone();
        let loader=Loader::with_steps(move|p,resume,c,y,budget|{
            assert!(!c());
            if p=="background"&&resume.is_none(){
                let source=Tracked::bytes(vec![7;1024],Owner::Source);
                ptr.store(source.as_ptr() as usize,std::sync::atomic::Ordering::Relaxed);
                started_tx.send(()).unwrap();go_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
                assert!(!y(MAX_PAUSED_SOURCE+1));assert!(y(source.capacity()));
                return LoadStep::Paused(source);
            }
            if p=="body"||p=="face"{
                assert_eq!(budget,BUDGET-1024);assert!(!y(1024));assert!(resume.is_none());
            }
            if p=="body"{body_tx.send(()).unwrap();body_go_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();}
            let source=if p=="background"{
                let source=resume.unwrap();assert_eq!(budget,BUDGET);
                assert_eq!(source.as_ptr() as usize,ptr.load(std::sync::atomic::Ordering::Relaxed));source
            }else{vec![1].into()};
            done_tx.send(p.to_string()).unwrap();LoadStep::Complete(Some(Payload::Encoded(source,None)))
        },4096,None).unwrap();
        loader.bind("background",true);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.bind("later",true);loader.bind("body",true);loader.bind("face",true);
        loader.prioritize_scene(&["body".into(),"face".into()]);go_tx.send(()).unwrap();
        body_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        {let state=loader.shared.state.lock().unwrap();
            assert!(state.entries["background"].pending);assert!(!state.entries["background"].prepared);
            assert!(state.entries["background"].payload.is_none());assert!(state.parked.is_some());
            assert_eq!(state.script_preload_counts().completed,0);
        }
        body_go_tx.send(()).unwrap();
        for expected in ["body","face","background","later"]{assert_eq!(done_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),expected);}
        assert!(loader.take("background").is_some());assert!(loader.shared.state.lock().unwrap().parked.is_none());
    }

    #[test]
    fn paused_sources_obey_cancel_rebind_and_chapter_lifetimes(){
        // Script cancellation drops suspended bindings, chapter changes drop
        // old leases, and leased work survives script queue cancellation.
        for mode in 0..4{
            let (started_tx,started_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
            let (urgent_tx,urgent_rx)=std::sync::mpsc::channel();let (finish_tx,finish_rx)=std::sync::mpsc::channel();
            let reads=Arc::new(std::sync::atomic::AtomicUsize::new(0));let n=reads.clone();
            let resumes=Arc::new(std::sync::atomic::AtomicUsize::new(0));let r=resumes.clone();
            let loader=Loader::with_steps(move|p,resume,_,y,_|{
                if p=="background"{
                    if let Some(bytes)=resume{r.fetch_add(1,std::sync::atomic::Ordering::Relaxed);return LoadStep::Complete(Some(Payload::Encoded(bytes,None)));}
                    if n.fetch_add(1,std::sync::atomic::Ordering::Relaxed)==0{
                        started_tx.send(()).unwrap();go_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
                        assert!(y(8));return LoadStep::Paused(vec![8;8].into());
                    }
                }
                if p=="urgent"{urgent_tx.send(()).unwrap();finish_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();}
                LoadStep::Complete(Some(Payload::encoded(vec![9])))
            },1024,None).unwrap();
            if mode>=2{loader.begin_chapter("old");loader.preload("background",Kind::Image);}else{loader.bind("background",true);}
            started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            loader.bind("urgent",true);loader.prioritize_scene(&["urgent".into()]);go_tx.send(()).unwrap();
            urgent_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            match mode{0=>loader.cancel(),1=>{loader.unbind("background");loader.bind("background",true);},2=>loader.begin_chapter("next"),_=>loader.cancel()}
            loader.bind("barrier",true);finish_tx.send(()).unwrap();loader.wait("barrier");
            let result=loader.take("background");
            assert_eq!(result.is_some(),mode==1||mode==3);
            assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed),if mode==1{2}else{1});
            assert_eq!(resumes.load(std::sync::atomic::Ordering::Relaxed),usize::from(mode==3));
            assert!(loader.shared.state.lock().unwrap().parked.is_none());
        }
    }

    #[test]
    fn real_png_decode_can_yield_and_resume_same_source_with_exact_pixels(){
        use image::ImageEncoder;
        for channels in [1usize,4]{
            let raw:Vec<u8>=(0..256*256*channels).map(|i|((i.wrapping_mul(37)^(i/257).wrapping_mul(19))%251)as u8).collect();
            let mut png=Vec::new();
            image::codecs::png::PngEncoder::new(&mut png).write_image(&raw,256,256,
                if channels==1{image::ExtendedColorType::L8}else{image::ExtendedColorType::Rgba8}).unwrap();
            let address=png.as_ptr();let checks=std::cell::Cell::new(0);
            let step=decode_source(png.into(),&||false,&|_|{let n=checks.get()+1;checks.set(n);n>=6},BUDGET);
            let LoadStep::Paused(source)=step else{panic!("decoder did not yield");};
            assert_eq!(source.as_ptr(),address);assert!(checks.get()>=6);
            match decode_source(source,&||false,&|_|false,BUDGET){
                LoadStep::Complete(Some(Payload::Gray(256,256,p,b)))=>{assert_eq!(channels,1);assert_eq!(p.as_slice(),raw);assert_eq!(b.as_ptr(),address);},
                LoadStep::Complete(Some(Payload::Pixels(p,b,_)))=>{assert_eq!(channels,4);assert_eq!(p.as_raw(),&raw);assert_eq!(b.as_ptr(),address);},
                _=>panic!("resume did not produce pixels"),
            }
        }
        assert!(matches!(decode_source(vec![1,2,3].into(),&||true,&|_|true,BUDGET),LoadStep::Complete(None)));
        assert!(matches!(decode_source(vec![1,2,3].into(),&||false,&|_|false,BUDGET),LoadStep::Complete(Some(Payload::Encoded(..)))));
    }
    #[test]
    fn parked_demand_preempts_scene_batch_and_shutdown_discards_parked_work(){
        for shutdown in [false,true]{
            let (start_tx,start_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
            let (urgent_tx,urgent_rx)=std::sync::mpsc::channel();let (finish_tx,finish_rx)=std::sync::mpsc::channel();
            let (done_tx,done_rx)=std::sync::mpsc::channel();
            let loader=Arc::new(Loader::with_steps(move|p,resume,c,y,_|{
                if p=="background"&&resume.is_none(){
                    start_tx.send(()).unwrap();go_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
                    assert!(y(8));return LoadStep::Paused(vec![7;8].into());
                }
                if p=="urgent"{urgent_tx.send(()).unwrap();finish_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();assert_eq!(c(),shutdown);}
                done_tx.send(p.to_string()).unwrap();LoadStep::Complete(Some(Payload::encoded(vec![1])))
            },1024,None).unwrap());
            loader.bind("background",true);start_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            loader.bind("urgent",true);loader.bind("face",true);loader.prioritize_scene(&["urgent".into(),"face".into()]);
            go_tx.send(()).unwrap();urgent_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            let other=loader.clone();
            if shutdown{
                let joined=std::thread::spawn(move||other.shutdown());
                let deadline=std::time::Instant::now()+std::time::Duration::from_secs(2);
                while !loader.shared.state.lock().unwrap().stop{assert!(std::time::Instant::now()<deadline);std::thread::yield_now();}
                assert!(loader.take("background").is_none());finish_tx.send(()).unwrap();joined.join().unwrap();
                assert_eq!(done_rx.try_iter().collect::<Vec<_>>(),["urgent"]);
            }else{
                let consumer=std::thread::spawn(move||other.take("background").is_some());
                let deadline=std::time::Instant::now()+std::time::Duration::from_secs(2);
                while !loader.shared.state.lock().unwrap().entries["background"].demanded{assert!(std::time::Instant::now()<deadline);std::thread::yield_now();}
                finish_tx.send(()).unwrap();assert!(consumer.join().unwrap());loader.wait("face");
                assert_eq!(done_rx.try_iter().collect::<Vec<_>>(),["urgent","background","face"]);
            }
        }
    }
    #[test]
    fn scene_batch_finishes_body_face_and_mask_before_unrelated_prefetch(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let (loaded_tx,loaded_rx)=std::sync::mpsc::channel();
        let loader=Loader::new(move|path,_|{
            if path=="active"{
                started_tx.send(()).unwrap();
                go_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
            }
            loaded_tx.send(path.to_string()).unwrap();
            Some(Payload::encoded(vec![1]))
        }).unwrap();
        loader.bind("active",true);
        started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.bind("later-background",true);
        loader.bind("body.png",true);
        loader.bind("face",true);
        loader.preload("mask.png",Kind::Mask);
        let paths=["active","body","face","mask","body.png","generated"].map(String::from);
        assert_eq!(loader.prioritize_scene(&paths),4);
        {let state=loader.shared.state.lock().unwrap();
            assert_eq!(state.entries.len(),5);
            assert_eq!(state.entries["body.png"].refs,1);
            assert_eq!(state.entries["mask.png"].refs,0);
        }
        go_tx.send(()).unwrap();
        // No take() here: the batch alone must keep the face/mask ahead of
        // speculative reads during the body's eventual foreground upload.
        for expected in ["active","body.png","face","mask.png","later-background"]{
            assert_eq!(loaded_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),expected);
        }
        assert!(loader.take("body").is_some());
        assert!(loader.take("face").is_some());
        assert!(loader.take("mask").is_some());
        loader.unbind("body.png");
        assert!(loader.key("body").is_none());
    }

    #[test]
    fn timeline_to_loader_keeps_decoded_pixels_during_deferred_scene_commit(){
        use std::sync::atomic::{AtomicUsize,Ordering};
        let reads=Arc::new(AtomicUsize::new(0));let calls=reads.clone();
        let loader=Loader::new(move|_,_|{calls.fetch_add(1,Ordering::SeqCst);Some(Payload::encoded(vec![1;8]))}).unwrap();
        let it=asb_interpreter::Interpreter::new(Default::default());
        it.lua().load(r#"
            scr={ip={file='chapter',block='a',count=1}};
            game={fgext='.png'};init={fgid={face=1}};
            cachebuff={img={background=true,['body.png']=true,['face.png']=true,later=true}};
            ast={a={{'bg',path='',file='background'},{'fg',path='',file='body',face='face'},
                    {'text'},linknext='b'},b={{'bg',path='',file='later'},{'text'}}};
        "#).exec().unwrap();
        // Use real, nonempty virtual directories as required by the protocol.
        it.lua().load(r#"
            for _,block in pairs(ast) do for _,tag in ipairs(block) do
                if tag.path then tag.path='images/' end
            end end
            cachebuff.img={['images/background']=true,['images/body.png']=true,['images/face.png']=true,['images/later']=true}
        "#).exec().unwrap();
        let names=["images/background","images/body.png","images/face.png","images/later"];
        for name in names{loader.bind(name,false);}
        let mut cursor=asb_interpreter::SurfaceTimelineCursor::default();
        let plan=it.query_surface_timeline(&mut cursor).unwrap();loader.update_story_plan(&plan,&[]);
        // The interpreter has advanced, but image_loop/extrans has not yet
        // published any new scene files. This used to discard all three images.
        it.lua().load("scr.ip.count=3").exec().unwrap();
        if let Some(plan)=it.query_surface_timeline(&mut cursor){loader.update_story_plan(&plan,&[]);}
        for name in &names[..3]{assert!(loader.take(name).is_some());}
        assert_eq!(reads.load(Ordering::SeqCst),4,"all visible images must consume the original READY data");
        it.lua().load("scr.ip.block='b';scr.ip.count=2").exec().unwrap();
        let plan=it.query_surface_timeline(&mut cursor).unwrap();
        loader.update_story_plan(&plan,&names[..3].iter().map(|s|s.to_string()).collect::<Vec<_>>());
        assert!(loader.take(names[3]).is_some());
        assert_eq!(reads.load(Ordering::SeqCst),4);
    }

    #[test]
    fn story_plan_cancels_passed_jobs_keeps_reuse_and_reloads_on_backtrack(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let (loaded_tx,loaded_rx)=std::sync::mpsc::channel();
        let loader=Loader::new(move|p,_|{
            if p=="old"{started_tx.send(()).unwrap();go_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();}
            loaded_tx.send(p.to_string()).unwrap();Some(Payload::encoded(vec![1]))
        }).unwrap();
        loader.bind("old",true);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        for p in ["skipped","far","body.png","face","reused"]{loader.bind(p,true);}
        loader.update_story_plan(&["old","skipped","far","body","face","reused"].map(String::from),&[]);
        loader.update_story_plan(&["body","face","reused","far"].map(String::from),&[]);
        {let state=loader.shared.state.lock().unwrap();
            assert!(!state.entries["old"].pending);assert!(state.entries["skipped"].story_suppressed);
            assert_eq!(state.entries["old"].refs,1);
        }
        go_tx.send(()).unwrap();
        for expected in ["old","body.png","face","reused","far"]{
            assert_eq!(loaded_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),expected);
        }
        loader.wait("far");
        {let state=loader.shared.state.lock().unwrap();assert!(state.entries["old"].payload.is_none());}
        // Reverse navigation revives the same binding, without growing refs.
        loader.update_story_plan(&["skipped".into()],&["body".into()]);
        loader.wait("skipped");
        {let state=loader.shared.state.lock().unwrap();
            assert_eq!(state.entries["skipped"].refs,1);assert!(state.entries["skipped"].payload.is_some());
            assert!(state.entries["body.png"].payload.is_some());
            assert!(state.entries["face"].payload.is_none());
        }
        // Dynamic demand outside the inferred path also revives a binding.
        assert!(loader.take("face").is_some());
        assert_eq!(loader.shared.state.lock().unwrap().entries["face"].refs,1);
    }

    #[test]
    fn story_plan_does_not_retire_unknown_or_effect_bindings(){
        let loader=Loader::new(|_,_|Some(Payload::encoded(vec![1;8]))).unwrap();
        for p in ["current.png","previous","dynamic","future"]{loader.bind(p,false);}
        loader.preload("mask",Kind::Mask);loader.wait("mask");
        loader.update_story_plan(&["current","previous","future","mask"].map(String::from),&[]);
        loader.update_story_plan(&["future".into()],&["current".into()]);
        let state=loader.shared.state.lock().unwrap();
        assert!(state.entries["current.png"].payload.is_some());
        assert!(state.entries["dynamic"].payload.is_some());
        assert!(state.entries["mask"].payload.is_some());
        assert!(state.entries["future"].payload.is_some());
        assert!(state.entries["previous"].payload.is_none());
        assert_eq!(state.script_preload_counts().planned,3);
    }

    #[test]
    fn scene_priority_preserves_bounded_queues_and_rejects_stale_tickets(){
        let mut state=State::default();
        for i in 0..100{
            state.entries.insert(format!("q{i}"),Entry{refs:1,ticket:i,bind_order:i,prepared:false,story_suppressed:false,pending:true,deferred:true,demanded:false,speculative:true,payload:None,kind:Kind::Image,leased:false,retry_bytes:0});
        }
        refill(&mut state);
        state.scene_priority=VecDeque::from([("q99".into(),99),("q98".into(),98),("q97".into(),96)]);
        promote(&mut state,"q0");
        assert_eq!(state.pop_job().unwrap().0,"q0");
        let mut seen=std::collections::HashSet::from(["q0".to_owned()]);
        for expected in ["q99","q98","q1"]{
            let (path,_)=state.pop_job().unwrap();assert_eq!(path,expected);assert!(seen.insert(path));
            refill(&mut state);assert!(state.queue_len()<=MAX_QUEUE);
        }
        while let Some((path,_))=state.pop_job(){assert!(seen.insert(path));refill(&mut state);assert!(state.queue_len()<=MAX_QUEUE);}
        assert_eq!(seen.len(),100);
        state.scene_priority.push_back(("q1".into(),1));state.clear_queues();
        assert!(state.scene_priority.is_empty());
    }

    #[test]
    fn scene_priority_replacement_does_not_keep_obsolete_scene_jobs_urgent(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let (loaded_tx,loaded_rx)=std::sync::mpsc::channel();
        let loader=Loader::new(move|p,_|{
            if p=="active"{started_tx.send(()).unwrap();go_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();}
            loaded_tx.send(p.to_string()).unwrap();Some(Payload::encoded(vec![1]))
        }).unwrap();
        loader.bind("active",true);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.bind("ordinary",true);loader.bind("old",true);loader.bind("new",true);
        loader.prioritize_scene(&["old".into()]);
        loader.prioritize_scene(&["new".into()]);
        go_tx.send(()).unwrap();
        for expected in ["active","new","ordinary","old"]{
            assert_eq!(loaded_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),expected);
        }
    }

    #[test] fn script_hud_counts_unique_live_bindings_and_preserves_completed_handoff(){
        use crate::image_cache_budget::ScriptPreloadCounts as Counts;
        let budget=crate::image_cache_budget::CacheBudget::new(1024);
        let loader=Loader::with_policy(priority_test_payload,1024,Some(budget.clone())).unwrap();
        loader.preload("chapter-mask",Kind::Mask);loader.wait("chapter-mask");
        assert_eq!(budget.lock().unwrap().script_preload,Counts::default());
        loader.bind("body",true);loader.bind("body",true);loader.wait("body");
        assert_eq!(budget.lock().unwrap().script_preload,Counts{planned:1,completed:1,pixels:1,encoded:0});
        assert!(loader.take("body").is_some());
        assert_eq!(budget.lock().unwrap().script_preload,Counts{planned:1,completed:1,pixels:0,encoded:0});
        loader.unbind("body");assert_eq!(budget.lock().unwrap().script_preload.completed,1);
        loader.unbind("body");assert_eq!(budget.lock().unwrap().script_preload,Counts::default());
        loader.bind("chapter-mask",true);
        assert_eq!(budget.lock().unwrap().script_preload,Counts{planned:1,completed:1,pixels:1,encoded:0});
        loader.begin_chapter("next"); // Script binding survives chapter hint release.
        assert_eq!(budget.lock().unwrap().script_preload.planned,1);
        loader.shutdown();assert_eq!(budget.lock().unwrap().script_preload,Counts::default());
    }
    #[test] fn script_hud_counts_pending_cancelled_and_missing_as_not_completed(){
        use crate::image_cache_budget::ScriptPreloadCounts as Counts;
        let budget=crate::image_cache_budget::CacheBudget::new(1024);
        let (started_tx,started_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
        let loader=Loader::with_policy(move|p,c|{
            if p=="pending"{started_tx.send(()).unwrap();go_rx.recv().unwrap();priority_test_payload(p,c)}else{None}
        },1024,Some(budget.clone())).unwrap();
        loader.bind("pending",true);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(budget.lock().unwrap().script_preload,Counts{planned:1,..Default::default()});
        loader.cancel();go_tx.send(()).unwrap();loader.bind("missing",false);
        assert_eq!(budget.lock().unwrap().script_preload,Counts{planned:2,..Default::default()});
        loader.unbind("pending");loader.unbind("missing");
        assert_eq!(budget.lock().unwrap().script_preload,Counts::default());
    }
    #[test] fn script_hud_distinguishes_pixel_encoded_and_capacity_declined_results(){
        use crate::image_cache_budget::ScriptPreloadCounts as Counts;
        let budget=crate::image_cache_budget::CacheBudget::new(80);
        let loader=Loader::with_policy(priority_test_payload,80,Some(budget.clone())).unwrap();
        for path in ["pixels","encoded","no-room"]{loader.bind(path,true);loader.wait(path);}
        assert_eq!(budget.lock().unwrap().script_preload,Counts{planned:3,completed:2,pixels:1,encoded:1});
        assert!(!loader.loading(None)); // A declined item is not successful completion.
    }
    #[test] fn gray8_ready_account_and_demotion_preserve_compressed_backup(){
        let budget=crate::image_cache_budget::CacheBudget::new(200);
        let loader=Loader::with_policy(|_,_|Some(Payload::Gray(17,9,vec![73;153].into(),vec![1;20].into())),200,Some(budget.clone())).unwrap();
        loader.preload("gray-mask",Kind::Mask);loader.wait("gray-mask");
        {let b=budget.lock().unwrap();assert_eq!((b.ready_parts.decoded,b.ready_parts.encoded,b.mask_parts.decoded),(153,20,153));}
        let mut p=loader.take("gray-mask");assert!(matches!(p,Some(Payload::Gray(17,9,..))));
        assert_eq!(budget.lock().unwrap().ready,0);assert_eq!(demote(&mut p),153);
        assert!(matches!(p,Some(Payload::Encoded(..))));assert_eq!(p.unwrap().parts().encoded,20);
    }
    #[test] fn capacity_declined_prefetch_refills_after_consumption_without_rebinding(){
        let calls=Arc::new(Mutex::new(Vec::new()));let seen=calls.clone();
        let loader=Loader::with_budget(move|p,c|{seen.lock().unwrap().push(p.to_owned());priority_test_payload(p,c)},72).unwrap();
        loader.bind("first",true);loader.wait("first");
        loader.bind("later",true);loader.wait("later");
        assert!(!loader.loading(None)); // No deadlock in script wait-all when full.
        assert_eq!(loader.shared.state.lock().unwrap().entries["later"].retry_bytes,72);
        assert_eq!(*calls.lock().unwrap(),["first","later"]);
        assert!(matches!(loader.take("first"),Some(Payload::Pixels(..))));
        loader.wait("later");
        assert!(matches!(loader.take("later"),Some(Payload::Pixels(..))));
        assert_eq!(*calls.lock().unwrap(),["first","later","later"]);
        let s=loader.shared.state.lock().unwrap();
        assert_eq!((s.entries["first"].refs,s.entries["later"].refs),(1,1));
        assert!(s.entries.values().all(|e|e.retry_bytes==0&&!e.pending));
    }
    #[test] fn refill_skips_missing_oversized_cancelled_and_unbound_entries(){
        for mode in ["missing","oversized","cancel","unbind","demand"]{
            let calls=Arc::new(Mutex::new(Vec::new()));let seen=calls.clone();
            let loader=Loader::with_budget(move|p,c|{
                seen.lock().unwrap().push(p.to_owned());
                match p{"missing"=>None,"oversized"=>Some(Payload::encoded(vec![0;73])),_=>priority_test_payload(p,c)}
            },72).unwrap();
            loader.bind("first",true);loader.wait("first");
            loader.bind(mode,true);loader.wait(mode);
            match mode{"cancel"=>loader.cancel(),"unbind"=>loader.unbind(mode),"demand"=>{assert!(loader.take(mode).is_none());},_=>{}}
            assert!(loader.take("first").is_some());
            assert!(!loader.loading(None));
            assert_eq!(*calls.lock().unwrap(),["first",mode]);
        }
    }
    #[test] fn refilled_inflight_job_obeys_cancel_and_does_not_publish_late(){
        let calls=Arc::new(Mutex::new(Vec::new()));let seen=calls.clone();
        let (tx,rx)=std::sync::mpsc::channel();let (go,gate)=std::sync::mpsc::channel();
        let loader=Loader::with_budget(move|p,c|{
            let retry={let mut seen=seen.lock().unwrap();let retry=seen.iter().any(|s|s==p);seen.push(p.to_owned());retry};
            if retry{tx.send(()).unwrap();gate.recv().unwrap();assert!(c());}
            priority_test_payload(p,c)
        },72).unwrap();
        loader.bind("first",true);loader.wait("first");loader.bind("later",true);loader.wait("later");
        loader.take("first");rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.cancel();assert!(!loader.loading(None));go.send(()).unwrap();
        loader.shutdown();assert_eq!(*calls.lock().unwrap(),["first","later","later"]);
    }
    #[test] fn lane_caps_count_pixels_encoded_and_proof_and_release_on_take(){
        let budget=crate::image_cache_budget::CacheBudget::new(64*1024*1024);
        let loader=Loader::with_policy(|_,_|Some(Payload::pixels(image::RgbaImage::new(2048,1024),vec![1;8])),BUDGET,Some(budget.clone())).unwrap();
        loader.preload("mask-a",Kind::Mask);loader.wait("mask-a");
        loader.preload("mask-b",Kind::Mask);loader.wait("mask-b");
        loader.preload("anim-a",Kind::Animation);loader.wait("anim-a");
        {let b=budget.lock().unwrap();assert_eq!(b.mask_parts.decoded,8*1024*1024);assert_eq!(b.mask_parts.encoded,16);assert!(b.mask_parts.total()<=Kind::Mask.cap());assert_eq!(b.animation_parts.decoded,8*1024*1024);assert_eq!(b.ready,b.mask_parts.total()+b.animation_parts.total());}
        assert!(matches!(loader.take("mask-b"),Some(Payload::Encoded(..))));
        assert!(matches!(loader.take("anim-a"),Some(Payload::Pixels(..))));
        assert_eq!(budget.lock().unwrap().animation_parts.total(),0);
        loader.shutdown();let b=budget.lock().unwrap();assert_eq!((b.ready,b.mask_parts.total()),(0,0));
    }
    #[test] fn leases_survive_script_cancel_without_duplicate_active_read_and_expire_with_chapter(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
        let calls=Arc::new(Mutex::new(Vec::new()));let seen=calls.clone();
        let loader=Loader::new(move|p,_|{seen.lock().unwrap().push(p.to_owned());if p=="mask"{started_tx.send(()).unwrap();go_rx.recv().unwrap();}Some(Payload::encoded(vec![1]))}).unwrap();
        loader.begin_chapter("one");loader.preload("mask",Kind::Mask);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.bind("mask",true);loader.preload("mask",Kind::Mask);loader.cancel();loader.unbind("mask");
        assert!(!loader.loading(None));assert!(loader.loading(Some("mask")));
        go_tx.send(()).unwrap();assert!(loader.take("mask").is_some());
        loader.preload("mask",Kind::Mask);loader.bind("barrier",false);
        assert_eq!(*calls.lock().unwrap(),["mask","barrier"]);
        loader.begin_chapter("two");assert!(loader.key("mask").is_none());assert!(loader.key("barrier").is_some());
    }
    #[test] fn immediate_mask_demand_can_replace_old_lane_pixels_without_exceeding_cap(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
        let loader=Loader::with_budget(move|p,_|{if p=="next"{started_tx.send(()).unwrap();go_rx.recv().unwrap();}Some(Payload::pixels(image::RgbaImage::new(2048,1024),vec![1;8]))},64*1024*1024).unwrap();
        loader.preload("old",Kind::Mask);loader.wait("old");
        loader.preload("next",Kind::Mask);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.shared.state.lock().unwrap().entries.get_mut("next").unwrap().demanded=true;
        go_tx.send(()).unwrap();loader.wait("next");
        assert!(loader.shared.state.lock().unwrap().kind_parts(Kind::Mask).total()<=Kind::Mask.cap());
        assert!(matches!(loader.take("next"),Some(Payload::Pixels(..))));assert!(matches!(loader.take("old"),Some(Payload::Encoded(..))));
    }
    #[test] fn chapter_change_discards_old_inflight_result_and_shared_charge(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
        let count=Arc::new(std::sync::atomic::AtomicUsize::new(0));let c=count.clone();
        let loader=Loader::new(move|_,cancelled|{let n=c.fetch_add(1,std::sync::atomic::Ordering::Relaxed);if n==0{started_tx.send(()).unwrap();go_rx.recv().unwrap();assert!(cancelled());}Some(Payload::encoded(vec![n as u8]))}).unwrap();
        loader.begin_chapter("one");loader.preload("same",Kind::Animation);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.begin_chapter("two");loader.preload("same",Kind::Animation);go_tx.send(()).unwrap();
        assert!(matches!(loader.take("same"),Some(Payload::Encoded(v,_)) if v.data==[1]));
    }
    #[test] fn three_lanes_share_queue_limit_and_urgent_demand_beats_rotation(){
        let mut s=State::default();
        for i in 0..100{let kind=[Kind::Image,Kind::Mask,Kind::Animation][i%3];s.entries.insert(format!("p{i}"),Entry{refs:1,ticket:i as u64,bind_order:i as u64,prepared:false,story_suppressed:false,pending:true,deferred:true,demanded:false,speculative:true,payload:None,kind,leased:false,retry_bytes:0});}
        refill(&mut s);assert_eq!(s.queue_len(),MAX_QUEUE);
        assert_eq!(s.pop_job().unwrap().0,"p0");assert_eq!(s.pop_job().unwrap().0,"p1");assert_eq!(s.pop_job().unwrap().0,"p2");assert_eq!(s.pop_job().unwrap().0,"p3");
        refill(&mut s);promote(&mut s,"p99");assert_eq!(s.queue_len(),MAX_QUEUE);assert_eq!(s.pop_job().unwrap().0,"p99");
    }
    #[test] fn lua_jobs_bypass_full_supplement_queue_but_effects_are_not_starved(){
        let mut s=State::default();
        for i in 0..80{
            s.entries.insert(format!("effect{i}"),Entry{refs:0,ticket:i,bind_order:0,prepared:false,story_suppressed:false,pending:true,deferred:true,demanded:false,speculative:true,payload:None,kind:Kind::Mask,leased:true,retry_bytes:0});
        }
        refill(&mut s);assert_eq!(s.queue_len(),MAX_QUEUE);
        for i in 0..9{
            s.entries.insert(format!("lua{i}"),Entry{refs:1,ticket:100+i,bind_order:100+i,prepared:false,story_suppressed:false,pending:true,deferred:true,demanded:false,speculative:true,payload:None,kind:Kind::Image,leased:false,retry_bytes:0});
        }
        refill(&mut s);assert_eq!(s.queue_len(),MAX_QUEUE);
        for expected in ["lua0","lua1","lua2","lua3","effect0","lua4","lua5","lua6","lua7","effect1","lua8"]{
            assert_eq!(s.pop_job().unwrap().0,expected);assert!(s.queue_len()<=MAX_QUEUE);refill(&mut s);
        }
        promote(&mut s,"effect79");assert_eq!(s.pop_job().unwrap().0,"effect79");
    }
    #[test] fn script_binding_upgrades_a_queued_chapter_lease_without_duplicate_load(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
        let (done_tx,done_rx)=std::sync::mpsc::channel();
        let calls=Arc::new(Mutex::new(Vec::new()));let seen=calls.clone();
        let loader=Loader::new(move|p,c|{
            seen.lock().unwrap().push(p.to_owned());
            if p=="barrier"{started_tx.send(()).unwrap();go_rx.recv().unwrap();}
            done_tx.send(()).unwrap();
            priority_test_payload(p,c)
        }).unwrap();
        loader.preload("barrier",Kind::Mask);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.preload("later-effect",Kind::Mask);loader.preload("script-effect",Kind::Animation);
        loader.bind("body",true);loader.bind("script-effect",true);go_tx.send(()).unwrap();
        for _ in 0..4{done_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();}
        loader.wait("later-effect");loader.wait("script-effect");
        assert_eq!(*calls.lock().unwrap(),["barrier","body","script-effect","later-effect"]);
    }
    #[test] fn supplemental_effects_cannot_demote_bound_first_use_pixels(){
        for kind in [Kind::Mask,Kind::Animation]{
            let loader=Loader::with_budget(priority_test_payload,80).unwrap();
            loader.bind("body",true);loader.wait("body");
            loader.preload("effect",kind);loader.wait("effect");
            // Inspect before take(), which may wake capacity refill.
            {let s=loader.shared.state.lock().unwrap();
                assert!(matches!(s.entries["body"].payload,Some(Payload::Pixels(..))));
                assert!(matches!(s.entries["effect"].payload,Some(Payload::Encoded(..))));
                assert_eq!(s.ready_parts().total(),80);
            }
            assert!(!loader.loading(None));
        }
    }
    #[test] fn lua_first_use_borrows_supplement_pixels_but_keeps_their_source(){
        for bound_effect in [false,true]{
            let loader=Loader::with_budget(priority_test_payload,80).unwrap();
            loader.preload("effect",Kind::Mask);loader.wait("effect");
            if bound_effect{loader.bind("effect",true);}
            loader.bind("body",true);loader.wait("body");
            let s=loader.shared.state.lock().unwrap();
            assert_eq!(s.ready_parts().total(),80);
            if bound_effect{
                assert!(matches!(s.entries["effect"].payload,Some(Payload::Pixels(..))));
                assert!(matches!(s.entries["body"].payload,Some(Payload::Encoded(..))));
            }else{
                assert!(matches!(s.entries["body"].payload,Some(Payload::Pixels(..))));
                assert!(matches!(s.entries["effect"].payload,Some(Payload::Encoded(..))));
            }
        }
    }
    #[test] fn capacity_refill_prefers_lua_order_over_earlier_chapter_tickets(){
        let mut s=State::default();
        for (path,refs,ticket,bind_order) in [("effect",0,1,0),("second",1,2,20),("first",1,3,10)]{
            s.entries.insert(path.into(),Entry{refs,ticket,bind_order,prepared:false,story_suppressed:false,pending:false,deferred:false,demanded:false,speculative:true,payload:None,kind:Kind::Mask,leased:true,retry_bytes:72});
        }
        assert!(retry_capacity(&mut s,72));
        assert_eq!(s.pop_job().unwrap().0,"first");
        assert_eq!(s.entries["effect"].retry_bytes,72);
        assert_eq!(s.entries["second"].retry_bytes,72);
    }
    #[test] fn split_account_survives_demotion_consumption_and_shutdown(){
        let budget=crate::image_cache_budget::CacheBudget::new(160);
        let loader=Loader::with_policy(priority_test_payload,160,Some(budget.clone())).unwrap();
        for name in ["first","second","overflow"]{loader.bind(name,true);loader.wait(name);}
        {let b=budget.lock().unwrap();assert_eq!(b.ready_parts,CacheParts{decoded:128,encoded:24,..Default::default()});assert_eq!(b.ready,152);}
        loader.take("first");
        {let b=budget.lock().unwrap();assert_eq!((b.ready_parts.decoded,b.ready_parts.encoded,b.ready),(64,16,80));}
        loader.unbind("second");
        {let b=budget.lock().unwrap();assert_eq!((b.ready_parts.decoded,b.ready_parts.encoded,b.ready),(0,8,8));}
        loader.shutdown();assert_eq!(budget.lock().unwrap().ready_parts,CacheParts::default());
    }
    #[test] fn split_payload_keeps_capacity_and_proof_separate_from_compressed_backup(){
        let image=image::RgbaImage::new(960,540);let decoded=image.as_raw().capacity();
        let proof=TileProof::from_pixels(&image).unwrap();let proof_bytes=proof.bytes();
        let mut source=Vec::with_capacity(1024);source.extend_from_slice(&[1,2,3]);let encoded=source.capacity();
        let mut p=Some(Payload::Pixels(image.into(),source.into(),Some(proof)));
        let parts=p.as_ref().unwrap().parts();assert_eq!((parts.decoded,parts.encoded,parts.proof),(decoded,encoded,proof_bytes));
        assert_eq!(demote(&mut p),decoded);
        let parts=p.as_ref().unwrap().parts();assert_eq!((parts.decoded,parts.encoded,parts.proof,parts.total()),(0,encoded,proof_bytes,encoded+proof_bytes));
    }
    fn priority_test_payload(_: &str,_:&dyn Fn()->bool)->Option<Payload>{
        Some(Payload::pixels(image::RgbaImage::from_pixel(4,4,image::Rgba([11,22,33,255])),vec![7;8]))
    }
    #[test] fn later_async_pixels_do_not_displace_bound_first_use_pixels(){
        let budget=crate::image_cache_budget::CacheBudget::new(160);
        let loader=Loader::with_policy(priority_test_payload,160,Some(budget.clone())).unwrap();
        for name in ["background","portrait","later-cg"]{loader.bind(name,true);loader.wait(name);}
        assert_eq!(budget.lock().unwrap().ready,152);
        assert!(matches!(loader.take("background"),Some(Payload::Pixels(..))));
        assert!(matches!(loader.take("portrait"),Some(Payload::Pixels(..))));
        assert!(matches!(loader.take("later-cg"),Some(Payload::Encoded(..))));
        assert_eq!(budget.lock().unwrap().ready,0);
        loader.bind("next-cg",true);loader.wait("next-cg");
        assert!(matches!(loader.take("next-cg"),Some(Payload::Pixels(..))));
    }
    #[test] fn speculative_fallback_preserves_earlier_encoded_and_binding_counts(){
        let budget=crate::image_cache_budget::CacheBudget::new(80);
        let loader=Loader::with_policy(priority_test_payload,80,Some(budget.clone())).unwrap();
        for name in ["background","later1","later2"]{loader.bind(name,true);loader.wait(name);}
        assert_eq!(budget.lock().unwrap().ready,80);
        assert!(matches!(loader.take("background"),Some(Payload::Pixels(..))));
        assert!(matches!(loader.take("later1"),Some(Payload::Encoded(..))));
        assert!(loader.key("later1").is_some()); // Keep script binding/refs.
        assert!(matches!(loader.take("later2"),Some(Payload::Pixels(..))));
        loader.unbind("later1");assert!(loader.key("later1").is_none());
    }
    #[test] fn full_bound_pixels_decline_speculative_admission_without_waiting_or_redecoding(){
        let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));let c=calls.clone();
        let budget=crate::image_cache_budget::CacheBudget::new(72);
        let loader=Loader::with_policy(move|p,x|{c.fetch_add(1,std::sync::atomic::Ordering::Relaxed);priority_test_payload(p,x)},72,Some(budget.clone())).unwrap();
        for name in ["background","later"]{loader.bind(name,true);loader.wait(name);}
        assert!(!loader.loading(None));assert_eq!(budget.lock().unwrap().ready,72);
        assert!(loader.take("later").is_none());
        assert!(matches!(loader.take("background"),Some(Payload::Pixels(..))));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed),2);
    }
    #[test] fn rejected_large_speculation_preserves_earlier_source_and_alpha_certificate(){
        let image=image::RgbaImage::from_pixel(960,540,image::Rgba([11,22,33,255]));
        let proof=TileProof::from_pixels(&image).unwrap();
        let source_bytes=8+proof.bytes();
        let limit=72+source_bytes;
        let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));let c=calls.clone();
        let budget=crate::image_cache_budget::CacheBudget::new(limit);
        let loader=Loader::with_policy(move|p,x|{
            c.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
            match p{
                "early"=>Some(Payload::Pixels(image.clone().into(),vec![7;8].into(),TileProof::from_pixels(&image))),
                "too-large"=>Some(Payload::encoded(vec![9;source_bytes+1])),
                _=>priority_test_payload(p,x),
            }
        },limit,Some(budget.clone())).unwrap();
        for p in ["background","early","too-large"]{loader.bind(p,true);loader.wait(p);}
        assert_eq!(budget.lock().unwrap().ready,limit);
        assert!(!loader.loading(None));
        assert!(loader.take("too-large").is_none());
        let Some(Payload::Encoded(source,Some(proof)))=loader.take("early") else{panic!("lost earlier source/certificate")};
        assert_eq!(source.as_slice(),&[7;8]);
        assert_eq!(proof.opaque_for_size(960,540),Some(true));
        assert_eq!(budget.lock().unwrap().ready,72);
        assert!(matches!(loader.take("background"),Some(Payload::Pixels(..))));
        assert_eq!(budget.lock().unwrap().ready,0);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed),3);
    }
    #[test] fn synchronous_demand_still_displaces_speculative_pixels_under_pressure(){
        let loader=Loader::with_budget(priority_test_payload,80).unwrap();
        loader.bind("background",true);loader.wait("background");
        loader.bind("needed-now",false);
        assert!(matches!(loader.take("needed-now"),Some(Payload::Pixels(..))));
        assert!(matches!(loader.take("background"),Some(Payload::Encoded(..))));
    }
    #[test] fn in_flight_async_result_becomes_priority_when_a_consumer_demands_it(){
        let (start_tx,start_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
        let loader=Loader::with_budget(move|p,x|{if p=="needed-now"{start_tx.send(()).unwrap();go_rx.recv().unwrap();}priority_test_payload(p,x)},80).unwrap();
        loader.bind("background",true);loader.wait("background");
        loader.bind("needed-now",true);start_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        // Same flag set by take before releasing the lock to await publication.
        loader.shared.state.lock().unwrap().entries.get_mut("needed-now").unwrap().demanded=true;
        go_tx.send(()).unwrap();loader.wait("needed-now");
        assert!(matches!(loader.take("needed-now"),Some(Payload::Pixels(..))));
        assert!(matches!(loader.take("background"),Some(Payload::Encoded(..))));
    }
    #[test] fn async_reservation_is_a_request_not_permission_to_overcommit_and_cancel_releases_it(){
        let budget=crate::image_cache_budget::CacheBudget::new(480);
        budget.lock().unwrap().set_idle(320);
        let (start_tx,start_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
        let loader=Loader::with_policy(move|_,_|{start_tx.send(()).unwrap();go_rx.recv().unwrap();Some(Payload::encoded(vec![1;8]))},160,Some(budget.clone())).unwrap();
        loader.bind("a",true);start_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        {let b=budget.lock().unwrap();assert_eq!(b.ready_goal,400);assert_eq!(b.ready_limit(),160);assert_eq!(b.idle_limit(320),80);}
        loader.cancel();assert_eq!(budget.lock().unwrap().ready_goal,0);
        go_tx.send(()).unwrap();loader.shutdown();assert_eq!(budget.lock().unwrap().ready,0);
    }
    #[test] fn completed_async_batch_releases_request_but_keeps_its_ready_pixels(){
        let budget=crate::image_cache_budget::CacheBudget::new(480);
        let (start_tx,start_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
        let loader=Loader::with_policy(move|_,_|{start_tx.send(()).unwrap();go_rx.recv().unwrap();Some(Payload::pixels(image::RgbaImage::new(4,4),vec![1;8]))},160,Some(budget.clone())).unwrap();
        loader.bind("a",true);start_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(budget.lock().unwrap().ready_goal,400);go_tx.send(()).unwrap();loader.wait("a");
        {let b=budget.lock().unwrap();assert_eq!((b.ready_goal,b.ready),(0,72));}
        assert!(matches!(loader.take("a"),Some(Payload::Pixels(..))));
    }
    #[test] fn ready_pixels_borrow_idle_headroom_and_release_on_take_unbind_shutdown(){
        let budget=crate::image_cache_budget::CacheBudget::new(400);
        budget.lock().unwrap().set_idle(64);
        let loader=Loader::with_policy(|_,_|Some(Payload::pixels(image::RgbaImage::from_pixel(4,4,image::Rgba([1,2,3,128])),vec![7;8])),144,Some(budget.clone())).unwrap();
        for p in ["first","second","third"]{loader.bind(p,false);}
        assert_eq!(budget.lock().unwrap().ready,216); // Previously first lost its 64 pixel bytes at the 144 cap.
        assert!(matches!(loader.take("first"),Some(Payload::Pixels(..))));
        assert_eq!(budget.lock().unwrap().ready,144);
        loader.unbind("second");assert_eq!(budget.lock().unwrap().ready,72);
        loader.cancel();assert_eq!(budget.lock().unwrap().ready,72); // Completed results survive cancel as before.
        loader.shutdown();let b=budget.lock().unwrap();assert_eq!((b.ready,b.idle),(0,64));
    }
    #[test] fn ready_pool_cannot_spend_occupied_idle_space_and_retains_compressed_fallback(){
        let budget=crate::image_cache_budget::CacheBudget::new(200);
        budget.lock().unwrap().set_idle(100);
        let loader=Loader::with_policy(|_,_|Some(Payload::pixels(image::RgbaImage::new(4,4),vec![7;8])),144,Some(budget.clone())).unwrap();
        loader.bind("first",false);loader.bind("second",false);
        assert_eq!(budget.lock().unwrap().ready,80);
        assert!(matches!(loader.take("first"),Some(Payload::Encoded(..))));
        budget.lock().unwrap().set_idle(0);
        loader.bind("third",false);
        assert!(matches!(loader.take("second"),Some(Payload::Pixels(..))));
        assert!(matches!(loader.take("third"),Some(Payload::Pixels(..))));
        assert_eq!(budget.lock().unwrap().ready,0);
    }
    #[test] fn cancelled_late_publication_does_not_recharge_shared_ready_account(){
        let budget=crate::image_cache_budget::CacheBudget::new(256);
        let (start_tx,start_rx)=std::sync::mpsc::channel();let (go_tx,go_rx)=std::sync::mpsc::channel();
        let loader=Loader::with_policy(move|_,_|{start_tx.send(()).unwrap();go_rx.recv().unwrap();Some(Payload::pixels(image::RgbaImage::new(4,4),vec![1;8]))},128,Some(budget.clone())).unwrap();
        loader.bind("old",true);start_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.cancel();assert!(!loader.loading(None));go_tx.send(()).unwrap();loader.shutdown();
        assert_eq!(budget.lock().unwrap().ready,0);
    }
    #[test] fn pixel_demotion_preserves_source_and_its_small_proof(){
        let image=image::RgbaImage::from_pixel(960,540,image::Rgba([11,23,31,255]));
        let proof=TileProof::from_pixels(&image).unwrap();let proof_bytes=proof.bytes();
        let bytes=vec![1,2,3,4];let pointer=bytes.as_ptr();
        let mut payload=Some(Payload::Pixels(image.into(),bytes.into(),Some(proof)));
        payload.as_mut().unwrap().ready();
        let before=payload.as_ref().unwrap().bytes();
        assert_eq!(demote(&mut payload),960*540*4);
        assert_eq!(payload.as_ref().unwrap().bytes(),before-960*540*4);
        let Some(Payload::Encoded(source,Some(proof)))=payload else{panic!("demotion lost proof")};
        assert_eq!(source.as_ptr(),pointer);assert_eq!(proof.bytes(),proof_bytes);
        assert!(proof.for_size(960,540).unwrap().iter().all(|&v|v==1));
    }
    #[test]
    fn decoder_allowance_charges_source_output_and_reader_without_overflow() {
        assert_eq!(decoder_allowance(1920,1080,1024*1024,4),Some(BUDGET-1920*1080*4-1024*1024-DECODE_READ_BUFFER));
        assert_eq!(decoder_allowance(2048,2048,0,4),None);
        assert_eq!(decoder_allowance(1024,1024,1024,8),Some(BUDGET-1024*1024*8-1024-DECODE_READ_BUFFER));
        assert_eq!(decoder_allowance(1,1,BUDGET-DECODE_READ_BUFFER-4,4),None);
        assert_eq!(decoder_allowance(1,1,BUDGET-DECODE_READ_BUFFER-5,4),Some(1));
        assert_eq!(decoder_allowance(u32::MAX,u32::MAX,0,4),None);
        assert_eq!(decoder_allowance(1,1,usize::MAX,4),None);
        assert_eq!(decoder_allowance(0,1080,0,4),None);
    }
    #[test]
    fn fallible_decode_matches_image_library_and_rejects_allocation_failure() {
        use image::{ColorType,ImageEncoder};
        for color in [ColorType::L8,ColorType::La8,ColorType::Rgb8,ColorType::Rgba8] {
            let raw:Vec<u8>=(0..7*3*color.bytes_per_pixel() as usize).map(|i|(i*37) as u8).collect();
            let mut png=Vec::new();
            image::codecs::png::PngEncoder::new(&mut png).write_image(&raw,7,3,color.into()).unwrap();
            let expected=image::load_from_memory(&png).unwrap().into_rgba8();
            let decoder=||image::ImageReader::new(std::io::Cursor::new(&png)).with_guessed_format().unwrap().into_decoder().unwrap();
            let actual=decode_rgba(decoder(),|data,size|data.try_reserve_exact(size).is_ok()).unwrap();
            assert_eq!(actual,expected,"{color:?}");
            assert!(decode_rgba(decoder(),|_,_|false).is_none());
            assert_eq!(image::load_from_memory(&png).unwrap().into_rgba8(),expected);
        }
    }
    #[test]
    fn ready_budget_preserves_first_use_pixels_without_changing_worker_protocol() {
        fn load(_: &str, _: &dyn Fn()->bool)->Option<Payload> {
            Some(Payload::pixels(image::RgbaImage::from_pixel(4,4,image::Rgba([1,2,3,255])),vec![7;8]))
        }
        for (budget,keep_pixels) in [(160,false),(256,true)] {
            let loader=Loader::with_budget(load,budget).unwrap();
            for name in ["first","second","third"] {loader.bind(name,false);}
            assert!(!loader.loading(None));
            assert!(loader.shared.state.lock().unwrap().entries.values()
                .filter_map(|e|e.payload.as_ref()).map(Payload::bytes).sum::<usize>()<=budget);
            let result=loader.take("first").unwrap();
            assert_eq!(matches!(result,Payload::Pixels(..)),keep_pixels);
            assert!(loader.take("first").is_none());
            loader.unbind("first");assert!(loader.key("first").is_none());
        }
    }
    #[test]
    fn worker_returns_pixels_once_and_duplicate_binds_share_one_job() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let c = calls.clone();
        let loader = Loader::new(move |_, _| { c.fetch_add(1, std::sync::atomic::Ordering::Relaxed); Some(Payload::pixels(image::RgbaImage::from_pixel(2,2,image::Rgba([1,2,3,128])), vec![7;4])) }).unwrap();
        loader.bind("bg.png",true); loader.bind("bg.png",true);
        let Some(Payload::Pixels(p, _,_)) = loader.take("bg") else { panic!("pixels missing") };
        assert_eq!(p.get_pixel(0,0).0,[1,2,3,128]); assert!(!loader.loading(None));
        assert!(loader.take("bg").is_none()); assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed),1);
        loader.unbind("bg.png"); assert!(loader.key("bg").is_some());
        loader.unbind("bg.png"); assert!(loader.key("bg").is_none());
    }
    #[test]
    fn cancellation_discards_active_result_and_unbind_rebind_cannot_publish_old_job() {
        let (started_tx,started_rx) = std::sync::mpsc::channel();
        let (release_tx,release_rx) = std::sync::mpsc::channel();
        let loader = Loader::new(move |_, _| { started_tx.send(()).unwrap(); release_rx.recv().unwrap(); Some(Payload::encoded(vec![1])) }).unwrap();
        loader.bind("a",true); started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(loader.loading(Some("a"))); assert!(!loader.loading(Some("b")));
        loader.cancel(); assert!(!loader.loading(None)); assert!(loader.take("a").is_none());
        loader.unbind("a"); loader.bind("a",true); release_tx.send(()).unwrap();
        started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(loader.loading(Some("a"))); release_tx.send(()).unwrap();
        assert!(matches!(loader.take("a"), Some(Payload::Encoded(..))));
    }
    #[test]
    fn missing_and_oversize_results_finish_without_poisoning_state() {
        let loader = Loader::new(|p, _| if p == "large" { Some(Payload::encoded(vec![0;BUDGET+1])) } else { None }).unwrap();
        loader.bind("missing",false); assert!(!loader.loading(None)); assert!(loader.take("missing").is_none());
        loader.bind("large",false); assert!(loader.take("large").is_none());
    }
    #[test]
    fn backlog_is_bounded_and_cancel_finishes_all_path_queries() {
        let (start_tx,start_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let loader=Loader::new(move |_, _| { start_tx.send(()).unwrap();go_rx.recv().unwrap();None }).unwrap();
        loader.bind("active",true);start_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        for i in 0..100 { loader.bind(&format!("queued{i}"),true); }
        assert_eq!(loader.shared.state.lock().unwrap().queue.len(),MAX_QUEUE);
        assert!(loader.loading(Some("queued99")));
        loader.cancel();assert!(!loader.loading(None));go_tx.send(()).unwrap();
    }
    #[test]
    fn completed_payloads_share_budget_without_losing_binding_counts() {
        let loader=Loader::new(|_, _|Some(Payload::encoded(vec![0;BUDGET/2+1]))).unwrap();
        loader.bind("a",false);loader.bind("b",false);
        assert!(loader.take("a").is_none());assert!(loader.take("b").is_some());
        assert!(loader.key("a").is_some());assert!(!loader.loading(None));
    }
    #[test]
    fn decoded_eviction_preserves_encoded_source_under_the_same_budget() {
        let loader=Loader::new(|_, _|Some(Payload::pixels(image::RgbaImage::new(1920,1080),vec![7;1024*1024]))).unwrap();
        loader.bind("a",false);loader.bind("b",false);
        let state=loader.shared.state.lock().unwrap();
        assert!(state.entries.values().filter_map(|e|e.payload.as_ref()).map(Payload::bytes).sum::<usize>()<=BUDGET);
        drop(state);
        assert!(matches!(loader.take("a"),Some(Payload::Encoded(b,_)) if b.len()==1024*1024));
        assert!(matches!(loader.take("b"),Some(Payload::Pixels(_, _,_))));
    }
    #[test]
    fn rejected_oversize_result_does_not_flush_useful_cache() {
        let loader=Loader::new(|p, _|Some(Payload::encoded(vec![0;if p=="big" {BUDGET+1} else {1024}]))).unwrap();
        loader.bind("a",false);loader.bind("big",false);
        assert!(loader.take("a").is_some());assert!(loader.take("big").is_none());
    }
    #[test]
    fn ready_foreground_result_survives_new_prefetch_pressure() {
        let loader=Loader::new(|_, _|Some(Payload::encoded(vec![0;BUDGET]))).unwrap();
        loader.bind("foreground",false);
        loader.shared.state.lock().unwrap().entries.get_mut("foreground").unwrap().demanded=true;
        loader.bind("speculative",false);
        assert!(loader.take("foreground").is_some());assert!(loader.take("speculative").is_none());
    }
    #[test]
    fn deferred_requests_drain_and_demand_promotes_without_dropping_jobs(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let calls=Arc::new(Mutex::new(Vec::new()));let c=calls.clone();
        let loader=Arc::new(Loader::new(move |p,_|{
            c.lock().unwrap().push(p.to_string());
            if p=="active"{started_tx.send(()).unwrap();go_rx.recv().unwrap();}
            Some(Payload::encoded(vec![1]))
        }).unwrap());
        loader.bind("active",true);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        for i in 0..100{loader.bind(&format!("q{i}"),true);}
        {let mut state=loader.shared.state.lock().unwrap();promote(&mut state,"q99");assert_eq!(state.queue.front().unwrap().0,"q99");assert_eq!(state.queue.len(),MAX_QUEUE);}
        go_tx.send(()).unwrap();
        // Consume the promoted demand before issuing any other demand; each
        // take legitimately reprioritizes its own key.
        assert!(loader.take("q99").is_some());
        for i in 0..99{assert!(loader.take(&format!("q{i}")).is_some());}
        assert!(!loader.loading(None));
        let c=calls.lock().unwrap();assert_eq!(c.len(),101);assert_eq!(c[1],"q99");
    }
    #[test]
    fn failure_consumption_and_cancelled_binding_can_be_retried(){
        let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));let c=calls.clone();
        let loader=Loader::new(move |_,_|{
            let n=c.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
            if n==0{None}else{Some(Payload::encoded(vec![n as u8]))}
        }).unwrap();
        loader.bind("a",false);assert!(loader.take("a").is_none());
        loader.bind("a",false);assert!(loader.take("a").is_some());
        loader.bind("a",false);assert!(loader.take("a").is_some());
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed),3);
        for _ in 0..3{loader.unbind("a");}assert!(loader.key("a").is_none());
    }
    #[test]
    fn cancelled_inflight_rebind_uses_new_ticket_without_unbind(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let mut_call=Arc::new(std::sync::atomic::AtomicUsize::new(0));let c=mut_call.clone();
        let loader=Loader::new(move |_,cancelled|{
            let n=c.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
            if n==0{started_tx.send(()).unwrap();go_rx.recv().unwrap();assert!(cancelled());}
            Some(Payload::encoded(vec![n as u8]))
        }).unwrap();
        loader.bind("a",true);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.cancel();loader.bind("a",true);assert!(loader.loading(Some("a")));go_tx.send(()).unwrap();
        assert!(matches!(loader.take("a"),Some(Payload::Encoded(v,_)) if v.data==[1]));
    }
    #[test]
    fn cancellation_releases_waiting_consumer_and_decoder_reader_returns_terminal_error(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let loader=Arc::new(Loader::new(move |_,_|{started_tx.send(()).unwrap();go_rx.recv().unwrap();None}).unwrap());
        loader.bind("a",true);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let c=loader.clone();let (done_tx,done_rx)=std::sync::mpsc::channel();
        let consumer=std::thread::spawn(move ||done_tx.send(c.take("a").is_none()).unwrap());
        loader.cancel();assert!(done_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap());
        go_tx.send(()).unwrap();consumer.join().unwrap();
        let mut reader=CancelReader{cursor:std::io::Cursor::new(&[1,2][..]),cancelled:&||true};
        assert_eq!(std::io::Read::read(&mut reader,&mut [0;1]).unwrap_err().kind(),std::io::ErrorKind::Other);
    }
    #[test]
    fn cancel_keeps_ready_cache_and_never_executes_cancelled_queue(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let seen=Arc::new(Mutex::new(Vec::new()));let calls=seen.clone();
        let loader=Loader::new(move |path,cancelled|{
            calls.lock().unwrap().push(path.to_string());
            if path=="active"{started_tx.send(()).unwrap();go_rx.recv().unwrap();assert!(cancelled());}
            Some(Payload::encoded(path.as_bytes().to_vec()))
        }).unwrap();
        loader.bind("ready",false);
        loader.bind("active",true);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        loader.bind("queued",true);loader.cancel();
        assert!(!loader.loading(None));
        assert!(matches!(loader.take("ready"),Some(Payload::Encoded(v,_)) if v.data==b"ready"));
        assert!(loader.take("queued").is_none());
        go_tx.send(()).unwrap();
        // Barrier request proves that the active result was handled, without sleeps.
        loader.bind("barrier",false);
        assert!(loader.take("active").is_none());
        assert_eq!(*seen.lock().unwrap(),["ready","active","barrier"]);
    }
    #[test]
    fn shutdown_wakes_consumers_and_joins_active_worker_before_returning(){
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (go_tx,go_rx)=std::sync::mpsc::channel();
        let loader=Arc::new(Loader::new(move |_,cancelled|{
            started_tx.send(()).unwrap();go_rx.recv().unwrap();assert!(cancelled());
            Some(Payload::encoded(vec![1]))
        }).unwrap());
        loader.bind("active",true);started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let c=loader.clone();let (consumer_tx,consumer_rx)=std::sync::mpsc::channel();
        let consumer=std::thread::spawn(move ||consumer_tx.send(c.take("active").is_none()).unwrap());
        let c=loader.clone();let (stop_tx,stop_rx)=std::sync::mpsc::channel();
        let stopper=std::thread::spawn(move ||{c.shutdown();stop_tx.send(()).unwrap();});
        assert!(consumer_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap());
        assert!(matches!(stop_rx.try_recv(),Err(std::sync::mpsc::TryRecvError::Empty)));
        go_tx.send(()).unwrap();stop_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        consumer.join().unwrap();stopper.join().unwrap();
        loader.bind("after-stop",true);assert!(!loader.loading(None));assert!(loader.key("after-stop").is_none());
        loader.shutdown(); // Idempotent, including the eventual Drop path.
    }
}
