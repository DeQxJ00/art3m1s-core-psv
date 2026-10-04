//! Compressed OGV sources, never decoded frames. Color and optional `_m` mask
//! are admitted/published/evicted together; a host lease pins the whole group.
use crate::image_cache_budget::SharedCacheBudget;
use crate::resource_ledger::{Charge,Owner};
use std::collections::{HashMap,HashSet,VecDeque};
use std::sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}};

const MIB:usize=1024*1024;
const QUANTUM:usize=256*1024;
#[derive(Clone,Copy,Debug)]
struct Policy { enabled:bool,groups:usize,bytes:usize }
impl Default for Policy {fn default()->Self{Self{enabled:true,groups:4,bytes:16*MIB}}}
struct Reservation {budget:SharedCacheBudget,bytes:usize,groups:Arc<AtomicUsize>}
impl Drop for Reservation {fn drop(&mut self){self.budget.lock().unwrap().video-=self.bytes;self.groups.fetch_sub(1,Ordering::Relaxed);}}
struct Group {color:Vec<u8>,mask:Vec<u8>,_reservation:Reservation,_charge:Charge}
struct Entry {data:Arc<Group>,used:u64}
struct Work {data:Group,offset:usize}
struct Job {ticket:u64,active:bool,work:Option<Work>}
#[derive(Default)]
struct State {
    policy:Policy,entries:HashMap<String,Entry>,jobs:HashMap<String,Job>,queue:VecDeque<String>,
    plan:Vec<String>,failed:HashSet<String>,serial:u64,hits:u64,misses:u64,evictions:u64,
}
struct Cache {state:Mutex<State>,budget:SharedCacheBudget,groups:Arc<AtomicUsize>}
fn key(path:&str)->Option<String>{
    let p=path.replace('\\',"/").to_ascii_lowercase();
    (p.len()<500&&p.len()>4&&p.ends_with(".ogv")&&!p.contains('\0')).then_some(p)
}
fn mask_path(path:&str)->String{format!("{}_m{}",&path[..path.len()-4],&path[path.len()-4..])}
impl Cache {
    fn new(budget:SharedCacheBudget)->Self{Self{state:Mutex::new(State::default()),budget,groups:Arc::new(AtomicUsize::new(0))}}
    fn valid(&self,path:&str,ticket:u64)->bool{self.state.lock().unwrap().jobs.get(path).is_some_and(|j|j.ticket==ticket)}
    fn enqueue(s:&mut State,path:String){
        if !s.policy.enabled||s.entries.contains_key(&path)||s.jobs.contains_key(&path)||s.failed.contains(&path){return;}
        s.serial+=1;s.jobs.insert(path.clone(),Job{ticket:s.serial,active:false,work:None});s.queue.push_back(path);
    }
    fn plan(&self,paths:&[String]){
        let mut s=self.state.lock().unwrap();let mut seen=HashSet::new();
        let plan:Vec<_>=paths.iter().filter_map(|p|key(p)).filter(|p|seen.insert(p.clone())).take(16).collect();
        s.jobs.retain(|p,_|plan.contains(p));s.queue.retain(|p|plan.contains(p));
        s.failed.retain(|p|plan.contains(p));s.plan=plan.clone();
        for path in plan{Self::enqueue(&mut s,path);}
        self.budget.lock().unwrap().clear_video_request();
    }
    fn acquire(&self,path:&str)->Option<Arc<Group>>{
        let path=key(path)?;let mut s=self.state.lock().unwrap();if !s.policy.enabled{return None;}
        // Actual playback outranks earlier speculative hints in the same AST
        // block (including videos started dynamically by Lua).
        s.plan.retain(|p|p!=&path);s.plan.insert(0,path.clone());s.plan.truncate(16);
        s.serial+=1;let used=s.serial;
        if let Some(e)=s.entries.get_mut(&path){e.used=used;let data=e.data.clone();s.hits+=1;return Some(data);}
        s.misses+=1;
        // Dynamic Lua paths not visible in the AST still warm future replays.
        // Never wait for an in-flight preload on the playback thread.
        if !s.jobs.contains_key(&path)&&s.jobs.len()>=16{
            if let Some(old)=s.queue.pop_back(){s.jobs.remove(&old);}
        }
        Self::enqueue(&mut s,path.clone());
        if s.queue.iter().any(|p|p==&path){s.queue.retain(|p|p!=&path);s.queue.push_front(path);}
        None
    }
    fn evict_one(s:&mut State,protect:Option<&str>)->bool{
        let path=s.entries.iter().filter(|(p,e)|Some(p.as_str())!=protect&&Arc::strong_count(&e.data)==1)
            .min_by_key(|(p,e)|(s.plan.contains(p),e.used)).map(|(p,_)|p.clone());
        if let Some(p)=path{s.entries.remove(&p);s.evictions+=1;true}else{false}
    }
    fn reserve(&self,path:&str,ticket:u64,bytes:usize)->Result<Option<Reservation>,&'static str>{
        let mut s=self.state.lock().unwrap();
        if !s.policy.enabled||!s.jobs.get(path).is_some_and(|j|j.ticket==ticket){return Err("cancelled");}
        if bytes>s.policy.bytes{return Err("group exceeds byte limit");}
        loop {
            let mut b=self.budget.lock().unwrap();
            let slots=self.groups.load(Ordering::Relaxed)<s.policy.groups;
            let room=bytes<=s.policy.bytes.saturating_sub(b.video);
            let potential=b.limit.saturating_sub(b.ready.saturating_add(b.ready_reserved).max(b.ready_goal)+b.emote+b.emote_scratch+b.video);
            if slots&&room&&bytes<=potential {
                if bytes>potential.saturating_sub(b.idle){let goal=b.video+bytes;b.request_video(goal);return Ok(None);}
                b.video+=bytes;b.clear_video_request();self.groups.fetch_add(1,Ordering::Relaxed);
                return Ok(Some(Reservation{budget:self.budget.clone(),bytes,groups:self.groups.clone()}));
            }
            drop(b);
            // Keep nearer planned groups. Otherwise a too-small byte limit
            // would cycle through the same future clips on every dialogue.
            let rank=s.plan.iter().position(|p|p==path).unwrap_or(0);
            let victim=s.entries.iter().filter(|(p,e)|Arc::strong_count(&e.data)==1&&s.plan.iter().position(|v|v==*p).is_none_or(|i|i>rank))
                .min_by_key(|(_,e)|e.used).map(|(p,_)|p.clone());
            if let Some(p)=victim{s.entries.remove(&p);s.evictions+=1;}else{return Ok(None);}
        }
    }
    // One bounded I/O slice per scheduling turn, allowing images/models/audio
    // to progress. Partial groups consume quota but are never exposed to AVIO.
    fn process_with(&self,size:impl Fn(&str)->Option<usize>,read:impl Fn(&str,&mut [u8],usize)->bool)->bool{
        let Some((path,ticket,mut work))=({
            let mut s=self.state.lock().unwrap();let mut job=None;
            while let Some(p)=s.queue.pop_front(){if let Some(j)=s.jobs.get_mut(&p){if !j.active{j.active=true;job=Some((p,j.ticket,j.work.take()));break;}}}job
        }) else{return false;};
        let started=std::time::Instant::now();
        let result=(||->Result<bool,&'static str>{
            if work.is_none(){
                let color=size(&path).filter(|n|*n>=4).ok_or("missing color")?;
                let mask=size(&mask_path(&path));
                if mask.is_some_and(|n|n<4){return Err("invalid mask size");}
                let mask=mask.unwrap_or(0);let total=color.checked_add(mask).ok_or("size overflow")?;
                let Some(reservation)=self.reserve(&path,ticket,total)? else{return Ok(false);};
                let mut charge=Charge::reserve(Owner::Media,total);
                let mut a=Vec::new();a.try_reserve_exact(color).map_err(|_|"color allocation")?;a.resize(color,0);
                let mut b=Vec::new();b.try_reserve_exact(mask).map_err(|_|"mask allocation")?;b.resize(mask,0);
                // try_reserve_exact is permitted to over-allocate. Do not hide
                // extra capacity from either limit if an allocator does so.
                if a.capacity()+b.capacity()!=total{return Err("allocation capacity exceeds reservation");}
                charge.commit(total);work=Some(Work{data:Group{color:a,mask:b,_reservation:reservation,_charge:charge},offset:0});
            }
            let w=work.as_mut().unwrap();let color=w.data.color.len();let total=color+w.data.mask.len();
            let end=(w.offset+QUANTUM).min(total);
            while w.offset<end {
                if !self.valid(&path,ticket){return Err("cancelled");}
                let (name,buffer,offset)=if w.offset<color{(path.clone(),&mut w.data.color,w.offset)}else{(mask_path(&path),&mut w.data.mask,w.offset-color)};
                let n=(end-w.offset).min(buffer.len()-offset).min(32768);
                if !read(&name,&mut buffer[offset..offset+n],offset){return Err("short read");}w.offset+=n;
            }
            if w.offset<total{return Ok(false);}
            if w.data.color.get(..4)!=Some(b"OggS")||(!w.data.mask.is_empty()&&w.data.mask.get(..4)!=Some(b"OggS")){return Err("invalid Ogg header");}
            Ok(true)
        })();
        let mut s=self.state.lock().unwrap();
        if !s.jobs.get(&path).is_some_and(|j|j.ticket==ticket){return false;}
        match result {
            Ok(true)=>{
                let data=Arc::new(work.take().unwrap().data);s.serial+=1;let used=s.serial;
                crate::core_info!("[ogv-cache] ready path={} color={} mask={} groups={} bytes={} slice_us={}",path,data.color.len(),data.mask.len(),self.groups.load(Ordering::Relaxed),self.budget.lock().unwrap().video,started.elapsed().as_micros());
                s.entries.insert(path.clone(),Entry{data,used});s.jobs.remove(&path);false
            }
            Ok(false)=>{let deferred=work.is_none();let j=s.jobs.get_mut(&path).unwrap();j.active=false;j.work=work;s.queue.push_back(path);deferred}
            Err(reason)=>{s.jobs.remove(&path);s.failed.insert(path.clone());crate::core_info!("[ogv-cache] skip path={} reason={}",path,reason);false}
        }
    }
    fn reclaim(&self,needed:usize){
        let mut s=self.state.lock().unwrap();
        loop{let b=self.budget.lock().unwrap();let fits=needed<=b.limit.saturating_sub(b.ready.max(b.ready_goal)+b.emote+b.emote_scratch+b.video);drop(b);
            if fits||!Self::evict_one(&mut s,None){break;}}
    }
    fn clear(&self){let mut s=self.state.lock().unwrap();s.jobs.clear();s.queue.clear();s.entries.clear();s.plan.clear();s.failed.clear();s.hits=0;s.misses=0;s.evictions=0;self.budget.lock().unwrap().clear_video_request();}
}
static SESSION:Mutex<Option<Arc<Cache>>>=Mutex::new(None);
fn session()->Arc<Cache>{SESSION.lock().unwrap().get_or_insert_with(||Arc::new(Cache::new(crate::image_cache_budget::session_budget()))).clone()}
pub(super) fn reset(){if let Some(c)=SESSION.lock().unwrap().as_ref(){c.clear();}}
pub(super) fn cancel_plan(){if let Some(c)=SESSION.lock().unwrap().as_ref(){let mut s=c.state.lock().unwrap();s.jobs.clear();s.queue.clear();s.plan.clear();s.failed.clear();c.budget.lock().unwrap().clear_video_request();}}
pub(super) fn reclaim(needed:usize){let c=SESSION.lock().unwrap().clone();if let Some(c)=c{c.reclaim(needed);}}
pub(super) fn pending()->bool{SESSION.lock().unwrap().as_ref().is_some_and(|c|!c.state.lock().unwrap().queue.is_empty())}
#[cfg(all(target_os="vita",feature="gxm-backend"))]
pub(super) fn plan(paths:&[String],comments:super::png_comments::SharedComments){let c=session();c.plan(paths);if pending(){super::surface_loader::wake_models(comments);}}
pub(super) fn process_one()->bool{
    let c=SESSION.lock().unwrap().clone();c.is_some_and(|c|c.process_with(|p|crate::ffi::query_asset_size(p).and_then(|n|usize::try_from(n).ok()),read_slice))
}
fn read_slice(path:&str,out:&mut [u8],offset:usize)->bool{
    #[cfg(target_os="vita")]
    {unsafe extern "C"{fn host_stream_open(p:*const std::ffi::c_char,size:*mut i64)->*mut std::ffi::c_void;fn host_stream_read(s:*mut std::ffi::c_void,out:*mut u8,n:i32,offset:i64)->i32;fn host_stream_close(s:*mut std::ffi::c_void);}
        let Ok(p)=std::ffi::CString::new(path)else{return false;};let mut size=0;
        unsafe{let s=host_stream_open(p.as_ptr(),&mut size);if s.is_null(){return false;}let n=host_stream_read(s,out.as_mut_ptr(),out.len() as i32,offset as i64);host_stream_close(s);std::thread::yield_now();n==out.len() as i32}
    }
    #[cfg(not(target_os="vita"))]
    {let Ok(data)=crate::ffi::request_file(path)else{return false;};let Some(data)=data.get(offset..offset+out.len())else{return false;};out.copy_from_slice(data);true}
}
// FFI: the lease owns BOTH pointers until release, even after cache reset.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_ogv_cache_acquire(path:*const std::ffi::c_char,color:*mut *const u8,color_len:*mut usize,mask:*mut *const u8,mask_len:*mut usize)->*mut std::ffi::c_void{
    if path.is_null()||color.is_null()||color_len.is_null()||mask.is_null()||mask_len.is_null(){return std::ptr::null_mut();}
    unsafe{*color=std::ptr::null();*color_len=0;*mask=std::ptr::null();*mask_len=0;}
    let Ok(path)=(unsafe{std::ffi::CStr::from_ptr(path)}).to_str()else{return std::ptr::null_mut();};
    let data=session().acquire(path);
    #[cfg(all(target_os="vita",feature="gxm-backend"))]
    super::surface_loader::wake_media();
    let Some(data)=data else{crate::core_info!("[ogv-cache] miss path={} streaming=1",path);return std::ptr::null_mut();};
    crate::core_info!("[ogv-cache] hit path={} color={} mask={}",path,data.color.len(),data.mask.len());
    unsafe{*color=data.color.as_ptr();*color_len=data.color.len();if !data.mask.is_empty(){*mask=data.mask.as_ptr();*mask_len=data.mask.len();}}
    Arc::into_raw(data) as *mut std::ffi::c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_ogv_cache_release(lease:*mut std::ffi::c_void){if !lease.is_null(){drop(unsafe{Arc::from_raw(lease as *const Group)});}}
#[unsafe(no_mangle)]
pub extern "C" fn art3m1s_ogv_cache_configure(enabled:i32,groups:usize,mib:usize)->i32{
    if !(0..=1).contains(&enabled)||!(1..=16).contains(&groups)||!(4..=64).contains(&mib){return 0;}
    let c=session();c.clear();c.state.lock().unwrap().policy=Policy{enabled:enabled!=0,groups,bytes:mib*MIB};1
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_ogv_cache_snapshot(out:*mut u64,count:usize)->i32{
    if out.is_null()||count<8{return 0;}let Ok(slot)=SESSION.try_lock()else{return 0;};
    let Some(c)=slot.as_ref()else{unsafe{std::ptr::write_bytes(out,0,8)};return 1;};
    let Ok(s)=c.state.try_lock()else{return 0;};let Ok(b)=c.budget.try_lock()else{return 0;};
    let v=[b.video as u64,s.policy.bytes as u64,c.groups.load(Ordering::Relaxed) as u64,s.policy.groups as u64,s.hits,s.misses,s.entries.len() as u64,s.jobs.len() as u64];
    unsafe{std::ptr::copy_nonoverlapping(v.as_ptr(),out,8)};1
}

#[cfg(test)] mod tests;
