//! Bounded PSB source retention. Playback state and GPU textures are never cached here.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use crate::image_cache_budget::SharedCacheBudget;
use crate::resource_ledger::{Charge, Owner};

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
}
impl Source {
    pub fn uncached(bytes: Vec<u8>) -> Arc<Self> {
        let charge=Charge::observed(Owner::Source,bytes.capacity());
        Arc::new(Self { bytes:Arc::new(bytes), _reservation:None, _charge:charge })
    }
}
struct Entry { source:Arc<Source>, order:u64, prefetched:bool }
struct Pending { ticket:u64, active:bool, demanded:bool }
#[derive(Default)]
struct State {
    entries:HashMap<String,Entry>, pending:HashMap<String,Pending>,
    queue:VecDeque<(String,u64)>, plan:Vec<String>, serial:u64,
    hits:u64, misses:u64, prefetch_hits:u64, evictions:u64,
}
pub(super) struct Cache { state:Mutex<State>, wake:Condvar, budget:SharedCacheBudget, limit:usize }
fn key(path:&str)->String { path.replace('\\',"/") }
impl Cache {
    fn new(budget:SharedCacheBudget,limit:usize)->Self { Self{state:Mutex::new(State::default()),wake:Condvar::new(),budget,limit} }
    fn evict_locked(&self,s:&mut State,needed:usize,source_growth:bool) {
        loop {
            let b=self.budget.lock().unwrap();
            let available=b.limit.saturating_sub(b.ready.max(b.ready_goal)+b.idle+b.emote);
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
    fn reserve(&self,size:usize)->Option<Reservation> {
        if size>self.limit {return None;}
        let mut s=self.state.lock().unwrap();self.evict_locked(&mut s,size,true);
        let mut b=self.budget.lock().unwrap();
        if size>b.limit.saturating_sub(b.ready.max(b.ready_goal)+b.idle+b.emote)
            ||size>self.limit.saturating_sub(b.emote) {return None;}
        b.emote+=size;Some(Reservation{budget:self.budget.clone(),bytes:size})
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
                // A queued (not yet reading) job is safely taken over by demand.
                s.pending.remove(&path);s.queue.retain(|(p,_)|p!=&path);
            }
            s.serial+=1;let ticket=s.serial;s.misses+=1;
            s.pending.insert(path.clone(),Pending{ticket,active:true,demanded:true});drop(s);
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
            s.pending.insert(path.clone(),Pending{ticket,active:false,demanded:false});s.queue.push_back((path,ticket));
        }
        self.wake.notify_all();
    }
    fn process_one(&self) {
        let job={let mut s=self.state.lock().unwrap();let mut job=None;
            while let Some((p,t))=s.queue.pop_front(){
                if let Some(v)=s.pending.get_mut(&p){if v.ticket==t&&!v.active{v.active=true;job=Some((p,t));break;}}
            }job};
        let Some((path,ticket))=job else{return;};
        let start=std::time::Instant::now();let result=load_source(self,&path,ticket,true);
        crate::core_info!("[emote-source-cache] prefetch path={} elapsed_us={} ok={}",path,start.elapsed().as_micros(),result.is_ok());
        let _=self.finish(&path,ticket,result,true);
    }
    fn clear(&self) {
        let mut s=self.state.lock().unwrap();s.entries.clear();s.pending.clear();s.queue.clear();s.plan.clear();s.serial+=1;self.wake.notify_all();
    }
}

// File bytes are shared, with reservation and ledger ownership kept alive by
// Source's consumers. On pressure, demand can use an uncached source normally.
fn load_source(cache:&Cache,path:&str,ticket:u64,prefetch:bool)->Result<Arc<Source>,String> {
    let size=crate::ffi::query_asset_size(path).ok_or_else(||format!("model not found: {path}"))? as usize;
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
    Ok(Arc::new(Source{bytes:Arc::new(bytes),_reservation:reservation,_charge:charge}))
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
pub(super) fn invalidate(path:&str){if let Some(c)=SESSION.lock().unwrap().as_ref(){c.state.lock().unwrap().entries.remove(&key(path));}}
pub(super) fn reset(){if let Some(c)=SESSION.lock().unwrap().take(){c.clear();}}
pub(super) fn cancel_plan(){if let Some(c)=SESSION.lock().unwrap().as_ref(){c.plan(&[]);}}
pub(super) fn reclaim(needed:usize){if let Some(c)=SESSION.lock().unwrap().as_ref(){let mut s=c.state.lock().unwrap();c.evict_locked(&mut s,needed,false);}}
pub(super) fn process_one(){let c=SESSION.lock().unwrap().clone();if let Some(c)=c{c.process_one();}}
pub(super) fn pending()->bool{SESSION.lock().unwrap().as_ref().is_some_and(|c|!c.state.lock().unwrap().queue.is_empty())}
#[cfg(all(target_os="vita",feature="gxm-backend"))]
pub(super) fn plan(paths:&[String],comments:super::png_comments::SharedComments){session().plan(paths);super::surface_loader::wake_models(comments);}

/// Independent optional HUD extension: allocated source bytes, retained models,
/// hits, misses, prefetch hits, evictions, planned models, resident planned models.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_emote_source_cache_snapshot(out:*mut u64,count:usize)->i32{
    if out.is_null()||count<8{return 0;}
    let Ok(slot)=SESSION.try_lock()else{return 0;};
    let Some(c)=slot.as_ref()else{unsafe{std::ptr::write_bytes(out,0,8)};return 1;};
    let Ok(s)=c.state.try_lock()else{return 0;};let Ok(b)=c.budget.try_lock()else{return 0;};
    let v=[b.emote as u64,s.entries.len() as u64,s.hits,s.misses,s.prefetch_hits,s.evictions,
        s.plan.len() as u64,s.plan.iter().filter(|p|s.entries.contains_key(*p)).count() as u64];
    unsafe{std::ptr::copy_nonoverlapping(v.as_ptr(),out,8)};1
}

#[cfg(test)] mod tests;
