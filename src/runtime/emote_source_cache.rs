//! Bounded PSB source and immutable parsed-model retention. Playback state and GPU textures are never cached here.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::sync::atomic::{AtomicUsize,Ordering};
use std::collections::BTreeMap;
use art3m1s_emote::{EmoteModel,PsbDocument,PsbResourceData};
use crate::image_cache_budget::SharedCacheBudget;
use crate::resource_ledger::{Charge, Owner};

const DEFERRED: &str = "model prefetch deferred: cache pressure";
const CANCELLED: &str = "model preparse cancelled";
// Temporary shared-pool headroom, separate from the retained model quota.
struct ParseScratch { budget: SharedCacheBudget, bytes: usize, _charge: Charge }
impl Drop for ParseScratch {
    fn drop(&mut self) { self.budget.lock().unwrap().emote_scratch -= self.bytes; }
}
pub(super) const LIMIT: usize = 32 * 1024 * 1024;
struct Reservation { budget: SharedCacheBudget, bytes: usize }
impl Drop for Reservation {
    fn drop(&mut self) { self.budget.lock().unwrap().emote -= self.bytes; }
}
pub(super) struct Source {
    pub bytes: Arc<Vec<u8>>,
    // Instances hold Source, not just bytes, until their last upload succeeds.
    _reservation: Option<Reservation>,
    _charge: Charge,
    parsed:Mutex<Option<Arc<ParsedModel>>>,
}
impl Source {
    pub fn uncached(bytes: Vec<u8>) -> Arc<Self> {
        let charge=Charge::observed(Owner::Source,bytes.capacity());
        Arc::new(Self { bytes:Arc::new(bytes), _reservation:None, _charge:charge,parsed:Mutex::new(None) })
    }
}
pub(super) struct ParsedModel {
    model:EmoteModel,
    textures:BTreeMap<String,std::ops::Range<usize>>,
    _reservation:Option<Reservation>,
    _charge:Charge,
    bytes:usize,
    live:Arc<AtomicUsize>,
}
impl std::ops::Deref for ParsedModel {type Target=EmoteModel;fn deref(&self)->&EmoteModel{&self.model}}
impl Drop for ParsedModel {fn drop(&mut self){self.live.fetch_sub(self.bytes,Ordering::Relaxed);}}
impl ParsedModel {
    pub fn texture_data(&self,source:&Arc<Source>)->Result<BTreeMap<String,PsbResourceData>,String>{
        self.textures.iter().map(|(id,range)|PsbResourceData::from_shared_range(source.bytes.clone(),range.clone())
            .map(|v|(id.clone(),v)).map_err(|e|e.to_string())).collect()
    }
}
struct Entry { source:Arc<Source>, order:u64, prefetched:bool }
struct Pending { ticket:u64, active:bool, demanded:bool, source:Option<Arc<Source>> }
#[derive(Default)]
struct State {
    entries:HashMap<String,Entry>, pending:HashMap<String,Pending>,
    queue:VecDeque<(String,u64)>, plan:Vec<String>, serial:u64,
    hits:u64, misses:u64, prefetch_hits:u64, evictions:u64,
}
pub(super) struct Cache { parse_gate:Mutex<()>, state:Mutex<State>, wake:Condvar, budget:SharedCacheBudget, limit:usize,
    parsed_live:Arc<AtomicUsize>,parsed_hits:AtomicUsize,parsed_misses:AtomicUsize }
fn key(path:&str)->String { path.replace('\\',"/") }
impl Cache {
    fn new(budget:SharedCacheBudget,limit:usize)->Self { Self{parse_gate:Mutex::new(()),state:Mutex::new(State::default()),wake:Condvar::new(),budget,limit,
        parsed_live:Arc::new(AtomicUsize::new(0)),parsed_hits:AtomicUsize::new(0),parsed_misses:AtomicUsize::new(0)} }
    fn model(&self,source:&Arc<Source>,path:&str)->Result<Arc<ParsedModel>,String>{
        self.model_with(source,path,None)
    }
    fn model_with(&self,source:&Arc<Source>,path:&str,prefetch:Option<u64>)->Result<Arc<ParsedModel>,String>{
        let start=std::time::Instant::now();
        // Hits must not wait for a different model's background parse.
        if let Some(model)=source.parsed.lock().unwrap().as_ref().cloned(){
            self.parsed_hits.fetch_add(1,Ordering::Relaxed);
            crate::core_info!("[emote-model-cache] hit path={} estimate_bytes={} elapsed_us={}",path,model.bytes,start.elapsed().as_micros());
            return Ok(model);
        }
        // Serialize scratch allocations across background and foreground parses.
        let _parse=if prefetch.is_some(){self.parse_gate.try_lock().map_err(|_|DEFERRED.to_owned())?}
            else{self.parse_gate.lock().unwrap()};
        let valid=||prefetch.is_none_or(|ticket|self.valid(path,ticket));
        if !valid(){return Err(CANCELLED.into());}
        // One parse per immutable source. The cached result owns descriptors,
        // never texture views or a Source Arc, so no source/cache cycle exists.
        let mut slot=source.parsed.lock().unwrap();
        if let Some(model)=slot.as_ref(){
            self.parsed_hits.fetch_add(1,Ordering::Relaxed);
            crate::core_info!("[emote-model-cache] hit path={} estimate_bytes={} elapsed_us={}",path,model.bytes,start.elapsed().as_micros());
            return Ok(model.clone());
        }
        let mut scratch=if prefetch.is_some(){
            let size=PsbDocument::parse_scratch_estimate(&source.bytes).map_err(|e|e.to_string())?;
            // Oversized/unknown models keep the normal on-demand path.
            if size>32*1024*1024{return Err("model preparse scratch estimate exceeds limit".into());}
            Some(self.reserve_scratch(size).ok_or_else(||DEFERRED.to_owned())?)
        }else{None};
        self.parsed_misses.fetch_add(1,Ordering::Relaxed);
        let document=PsbDocument::from_shared_bytes(source.bytes.clone()).map_err(|e|e.to_string())?;
        if !valid(){return Err(CANCELLED.into());}
        let mut model=EmoteModel::from_document(document).map_err(|e|e.to_string())?;
        if !valid(){return Err(CANCELLED.into());}
        let (_,views)=model.take_texture_data().map_err(|e|e.to_string())?;
        let textures:BTreeMap<_,_>=views.iter().map(|(id,v)|(id.clone(),v.source_range())).collect();
        drop(views);
        let descriptors=textures.len()*(16*std::mem::size_of::<usize>()+11*std::mem::size_of::<(String,std::ops::Range<usize>)>())
            +textures.keys().map(String::capacity).sum::<usize>();
        let bytes=model.retained_bytes_estimate()+descriptors+std::mem::size_of::<ParsedModel>();
        let reservation=if source._reservation.is_some(){self.reserve_with_credit(bytes,&mut scratch)}else{None};
        drop(scratch);
        let retained=reservation.is_some();
        self.parsed_live.fetch_add(bytes,Ordering::Relaxed);
        let parsed=Arc::new(ParsedModel{model,textures,_reservation:reservation,_charge:Charge::observed(Owner::Source,bytes),bytes,live:self.parsed_live.clone()});
        if retained{*slot=Some(parsed.clone());}
        crate::core_info!("[emote-model-cache] miss path={} estimate_bytes={} retained={} elapsed_us={} preparse={}",path,bytes,retained,start.elapsed().as_micros(),prefetch.is_some());
        Ok(parsed)
    }
    fn evict_locked(&self,s:&mut State,needed:usize,source_growth:bool) {
        self.evict_with_credit(s,needed,source_growth,0);
    }
    fn evict_with_credit(&self,s:&mut State,needed:usize,source_growth:bool,credit:usize) {
        loop {
            let b=self.budget.lock().unwrap();
            // IDLE is lower priority: its occupied bytes may delay admission,
            // but must not cause eviction of a higher-priority PSB.
            if !source_growth && b.idle>0 {break;}
            let available=b.limit.saturating_sub(b.ready.max(b.ready_goal)+b.video+b.emote+b.emote_scratch.saturating_sub(credit));
            let fits=needed<=available && (!source_growth || needed<=self.limit.saturating_sub(b.emote));
            drop(b);
            if fits { break; }
            // Leased source bytes cannot be reclaimed yet; never pretend that
            // removing a map entry released an allocation still used by a model.
            let oldest=s.entries.iter().filter(|(_,e)|Arc::strong_count(&e.source)==1)
                .min_by_key(|(p,e)|(s.plan.contains(p),e.order)).map(|(p,_)|p.clone());
            let Some(path)=oldest else { break; };
            s.entries.remove(&path);s.evictions+=1;
        }
    }
    fn reserve_scratch(&self,size:usize)->Option<ParseScratch>{
        super::ogv_cache::reclaim(size);
        let mut s=self.state.lock().unwrap();self.evict_locked(&mut s,size,false);
        let mut b=self.budget.lock().unwrap();
        if size>b.limit.saturating_sub(b.ready.max(b.ready_goal)+b.idle+b.video+b.emote+b.emote_scratch){
            let goal=b.emote+b.emote_scratch+size;b.request_emote(goal);return None;
        }
        b.emote_scratch+=size;b.clear_emote_request();
        Some(ParseScratch{budget:self.budget.clone(),bytes:size,_charge:Charge::reserve(Owner::Temporary,size)})
    }
    fn reserve(&self,size:usize)->Option<Reservation> {self.reserve_with_credit(size,&mut None)}
    fn reserve_with_credit(&self,size:usize,scratch:&mut Option<ParseScratch>)->Option<Reservation> {
        super::ogv_cache::reclaim(size);
        if size>self.limit {return None;}
        let credit=scratch.as_ref().map_or(0,|s|s.bytes);
        let mut s=self.state.lock().unwrap();self.evict_with_credit(&mut s,size,true,credit);
        let mut b=self.budget.lock().unwrap();
        if size>self.limit.saturating_sub(b.emote) {return None;}
        if size>b.limit.saturating_sub(b.ready.max(b.ready_goal)+b.idle+b.video+b.emote+b.emote_scratch.saturating_sub(credit)) {
            let goal=b.emote.saturating_add(b.emote_scratch).saturating_add(size);b.request_emote(goal);
            return None;
        }
        b.emote_scratch-=credit;if let Some(s)=scratch.as_mut(){s.bytes=0;}
        b.emote+=size;b.clear_emote_request();Some(Reservation{budget:self.budget.clone(),bytes:size})
    }
    fn finish(&self,path:&str,ticket:u64,result:Result<Arc<Source>,String>,prefetched:bool)->Result<Arc<Source>,String> {
        let mut s=self.state.lock().unwrap();
        if s.pending.get(path).is_some_and(|p|p.ticket==ticket) {
            s.pending.remove(path);
            if let Ok(source)=&result {
                if source._reservation.is_some() {
                    s.serial+=1;let order=s.serial;
                    s.entries.insert(path.into(),Entry{source:source.clone(),order,prefetched});
                }
            }
        }
        self.wake.notify_all();result
    }
    fn read_with(&self,path:&str,load:impl FnOnce(&Self,&str,u64)->Result<Arc<Source>,String>)->Result<Arc<Source>,String> {
        let path=key(path);let started=std::time::Instant::now();
        let mut s=self.state.lock().unwrap();
        loop {
            s.serial+=1;let order=s.serial;
            if let Some(e)=s.entries.get_mut(&path) {
                e.order=order;let prefetched=e.prefetched;e.prefetched=false;let source=e.source.clone();
                s.hits+=1;if prefetched{s.prefetch_hits+=1;}
                crate::core_info!("[emote-source-cache] hit path={} prefetch={} bytes={} wait_us={} read_bytes=0",path,prefetched,source.bytes.len(),started.elapsed().as_micros());
                return Ok(source);
            }
            if let Some(p)=s.pending.get_mut(&path) {
                if p.active { p.demanded=true;s=self.wake.wait(s).unwrap();continue; }
                // Reuse bytes when demand takes over a job waiting for scratch.
                if let Some(source)=p.source.take(){
                    let ticket=p.ticket;p.active=true;p.demanded=true;
                    s.queue.retain(|(p,_)|p!=&path);drop(s);
                    return self.finish(&path,ticket,Ok(source),true);
                }
                // A queued (not yet reading) job is safely taken over by demand.
                s.pending.remove(&path);s.queue.retain(|(p,_)|p!=&path);
            }
            s.serial+=1;let ticket=s.serial;s.misses+=1;
            s.pending.insert(path.clone(),Pending{ticket,active:true,demanded:true,source:None});drop(s);
            let result=load(self,&path,ticket);
            crate::core_info!("[emote-source-cache] miss path={} elapsed_us={} ok={}",path,started.elapsed().as_micros(),result.is_ok());
            return self.finish(&path,ticket,result,false);
        }
    }
    fn valid(&self,path:&str,ticket:u64)->bool {
        self.state.lock().unwrap().pending.get(path).is_some_and(|p|p.ticket==ticket)
    }
    fn plan(&self,paths:&[String]) {
        let mut s=self.state.lock().unwrap();
        let mut seen=HashSet::new();
        let plan:Vec<_>=paths.iter().map(|p|key(p)).filter(|p|seen.insert(p.clone())).take(2).collect();
        s.pending.retain(|p,v|v.demanded||plan.contains(p));
        s.queue.retain(|(p,_)|plan.contains(p));
        s.plan=plan.clone();
        for path in plan {
            if s.entries.contains_key(&path)||s.pending.contains_key(&path){continue;}
            s.serial+=1;let ticket=s.serial;
            s.pending.insert(path.clone(),Pending{ticket,active:false,demanded:false,source:None});s.queue.push_back((path,ticket));
        }
        self.wake.notify_all();
    }
    fn process_one(&self)->bool {
        self.process_one_with(|c,path,ticket|load_source(c,path,ticket,true))
    }
    fn process_one_with(&self,load:impl FnOnce(&Self,&str,u64)->Result<Arc<Source>,String>)->bool {
        let job={let mut s=self.state.lock().unwrap();let mut job=None;
            while let Some((p,t))=s.queue.pop_front(){
                if let Some(v)=s.pending.get_mut(&p){if v.ticket==t&&!v.active{v.active=true;job=Some((p,t,v.source.take()));break;}}
            }job};
        let Some((path,ticket,reused))=job else{return false;};
        let start=std::time::Instant::now();let result=match reused{Some(source)=>Ok(source),None=>load(self,&path,ticket)};
        let mut defer=result.as_ref().err().is_some_and(|e|e==DEFERRED);
        if let Ok(source)=&result {
            if self.valid(&path,ticket){
                match self.model_with(source,&path,Some(ticket)){
                    Ok(_)=>{},
                    Err(e) if e==DEFERRED=>{defer=true;},
                    Err(e)=>{crate::core_info!("[emote-model-cache] preparse skipped path={} reason={}",path,e);},
                }
            }
        }
        if defer {
            let mut s=self.state.lock().unwrap();
            if let Some(p)=s.pending.get_mut(&path).filter(|p|p.ticket==ticket){
                p.active=false;p.source=result.ok();s.queue.push_back((path,ticket));
            }
            self.wake.notify_all();return true;
        }
        crate::core_info!("[emote-source-cache] prefetch path={} elapsed_us={} ok={}",path,start.elapsed().as_micros(),result.is_ok());
        let _=self.finish(&path,ticket,result,true);false
    }
    fn clear(&self) {
        let mut s=self.state.lock().unwrap();s.entries.clear();s.pending.clear();s.queue.clear();s.plan.clear();s.serial+=1;self.budget.lock().unwrap().clear_emote_request();self.wake.notify_all();
    }
}

// File bytes are shared, with reservation and ledger ownership kept alive by
// Source's consumers. On pressure, demand can use an uncached source normally.
fn load_source(cache:&Cache,path:&str,ticket:u64,prefetch:bool)->Result<Arc<Source>,String> {
    let size=crate::ffi::query_asset_size(path).ok_or_else(||format!("model not found: {path}"))? as usize;
    if prefetch&&size>cache.limit{return Err("model exceeds prefetch quota".into());}
    let mut reservation=cache.reserve(size);
    if prefetch&&reservation.is_none(){return Err("model prefetch deferred: cache pressure".into());}
    let mut charge=Charge::reserve(Owner::Source,size);
    let started=std::time::Instant::now();
    let bytes=read_bytes(path,size,||{
        if !cache.valid(path,ticket){return false;}
        #[cfg(all(target_os="vita",feature="gxm-backend"))]
        if prefetch&&super::surface_loader::model_should_yield(){
            return cache.state.lock().unwrap().pending.get(path).is_some_and(|p|p.demanded);
        }
        true
    })?;
    crate::core_info!("[emote-source-cache] read path={} bytes={} read_us={} prefetch={}",path,bytes.len(),started.elapsed().as_micros(),prefetch);
    charge.commit(bytes.capacity());
    if bytes.capacity()!=size {drop(reservation.take());reservation=cache.reserve(bytes.capacity());}
    if bytes.get(..4)!=Some(b"PSB\0"){return Err(format!("invalid PSB signature: {path}"));}
    Ok(Arc::new(Source{bytes:Arc::new(bytes),_reservation:reservation,_charge:charge,parsed:Mutex::new(None)}))
}
fn read_bytes(path:&str,size:usize,valid:impl Fn()->bool)->Result<Vec<u8>,String> {
    let mut bytes=Vec::new();bytes.try_reserve_exact(size).map_err(|e|e.to_string())?;bytes.resize(size,0);
    #[cfg(target_os="vita")]
    {
        unsafe extern "C" {
            fn host_stream_open(p:*const std::ffi::c_char,size:*mut i64)->*mut std::ffi::c_void;
            fn host_stream_read(s:*mut std::ffi::c_void,out:*mut u8,n:i32,offset:i64)->i32;
            fn host_stream_close(s:*mut std::ffi::c_void);
        }
        struct Stream(*mut std::ffi::c_void);
        impl Drop for Stream {fn drop(&mut self){if !self.0.is_null(){unsafe{host_stream_close(self.0)}}}}
        let p=std::ffi::CString::new(path).map_err(|e|e.to_string())?;let mut n=0;
        let stream=Stream(unsafe{host_stream_open(p.as_ptr(),&mut n)});
        if stream.0.is_null()||n!=size as i64{return Err(format!("model source changed or missing: {path}"));}
        for (i,chunk) in bytes.chunks_mut(32768).enumerate(){
            if !valid(){return Err("model read cancelled".into());}
            let n=unsafe{host_stream_read(stream.0,chunk.as_mut_ptr(),chunk.len() as i32,(i*32768) as i64)};
            if n!=chunk.len() as i32{return Err(format!("short model read: {path}"));}
            std::thread::sleep(std::time::Duration::from_micros(1));
        }
    }
    #[cfg(not(target_os="vita"))]
    { if !valid(){return Err("model read cancelled".into());}bytes=crate::ffi::request_file(path)?; }
    Ok(bytes)
}
static SESSION:Mutex<Option<Arc<Cache>>>=Mutex::new(None);
fn session()->Arc<Cache> {SESSION.lock().unwrap().get_or_insert_with(||Arc::new(Cache::new(crate::image_cache_budget::session_budget(),LIMIT))).clone()}
pub(super) fn read(path:&str)->Result<Arc<Source>,String>{session().read_with(path,|c,p,t|load_source(c,p,t,false))}
pub(super) fn parsed(source:&Arc<Source>,path:&str)->Result<Arc<ParsedModel>,String>{session().model(source,path)}
pub(super) fn invalidate(path:&str){if let Some(c)=SESSION.lock().unwrap().as_ref(){c.state.lock().unwrap().entries.remove(&key(path));}}
pub(super) fn reset(){if let Some(c)=SESSION.lock().unwrap().take(){c.clear();}}
pub(super) fn cancel_plan(){if let Some(c)=SESSION.lock().unwrap().as_ref(){c.plan(&[]);}}
pub(super) fn reclaim(needed:usize){if let Some(c)=SESSION.lock().unwrap().as_ref(){let mut s=c.state.lock().unwrap();c.evict_locked(&mut s,needed,false);}}
pub(super) fn process_one()->bool{let c=SESSION.lock().unwrap().clone();c.is_some_and(|c|c.process_one())}
pub(super) fn pending()->bool{SESSION.lock().unwrap().as_ref().is_some_and(|c|!c.state.lock().unwrap().queue.is_empty())}
#[cfg(all(target_os="vita",feature="gxm-backend"))]
pub(super) fn plan(paths:&[String],comments:super::png_comments::SharedComments){session().plan(paths);super::surface_loader::wake_models(comments);}

/// Independent optional HUD extension: allocated source bytes, retained models,
/// hits, misses, prefetch hits, evictions, planned models, resident planned models.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_emote_source_cache_snapshot(out:*mut u64,count:usize)->i32{
    if out.is_null()||count<8{return 0;}
    let Ok(slot)=SESSION.try_lock()else{return 0;};
    let n=if count>=11{11}else{8};
    let Some(c)=slot.as_ref()else{unsafe{std::ptr::write_bytes(out,0,n)};return 1;};
    let Ok(s)=c.state.try_lock()else{return 0;};let Ok(b)=c.budget.try_lock()else{return 0;};
    let v=[b.emote as u64,s.entries.len() as u64,s.hits,s.misses,s.prefetch_hits,s.evictions,
        s.plan.len() as u64,s.plan.iter().filter(|p|s.entries.contains_key(*p)).count() as u64,
        c.parsed_live.load(Ordering::Relaxed) as u64,c.parsed_hits.load(Ordering::Relaxed) as u64,c.parsed_misses.load(Ordering::Relaxed) as u64];
    unsafe{std::ptr::copy_nonoverlapping(v.as_ptr(),out,n)};1
}

#[cfg(test)] mod tests;
