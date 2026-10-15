//! CPU-only recovered scene templates, retained under the source-cache quota.
use super::*;
unsafe extern "C" {
 fn art3m1s_recovery_model_prepare_shared(data:*const u8,len:usize,limit:usize,bytes:*mut usize)->u64;
 fn art3m1s_recovery_model_release(id:u64);
}
pub(crate) struct RecoveredModel {
 pub id:u64,
 _backing:Arc<Vec<u8>>,
 _source_reservation:Option<Arc<Reservation>>,
 _source_charge:Option<Arc<Charge>>,
 _reservation:Option<Reservation>,
 _charge:Charge,
 bytes:usize,
 live:Arc<AtomicUsize>,
}
impl Drop for RecoveredModel {
 fn drop(&mut self){unsafe{art3m1s_recovery_model_release(self.id)};self.live.fetch_sub(self.bytes,Ordering::Relaxed);}
}
// Also releases a successfully prepared handle on cancellation/admission errors.
struct Prepared(u64);
impl Drop for Prepared {fn drop(&mut self){if self.0!=0{unsafe{art3m1s_recovery_model_release(self.0)}}}}
impl Cache {
 pub(super) fn recovered_model(&self,source:&Arc<Source>,path:&str,prefetch:Option<u64>)->Result<Arc<RecoveredModel>,String>{
  let start=std::time::Instant::now();
  if let Some(model)=source.recovered.lock().unwrap().as_ref().cloned(){
   self.parsed_hits.fetch_add(1,Ordering::Relaxed);
   crate::core_info!("[recovered-model-cache] hit path={} bytes={} elapsed_us={}",path,model.bytes,start.elapsed().as_micros());return Ok(model);
  }
  let _parse=if prefetch.is_some(){self.parse_gate.try_lock().map_err(|_|DEFERRED.to_owned())?}else{self.parse_gate.lock().unwrap()};
  let valid=||prefetch.is_none_or(|ticket|self.valid(path,ticket));
  if !valid(){return Err(CANCELLED.into());}
  let mut slot=source.recovered.lock().unwrap();
  if let Some(model)=slot.as_ref(){self.parsed_hits.fetch_add(1,Ordering::Relaxed);return Ok(model.clone());}
  // A hard C allocation cap, not an assumed scene-size estimate. Normalization
  // can copy the input once; its temporary copy is covered separately.
  let cap:usize=if prefetch.is_some(){8*1024*1024}else{0};
  let mut scratch=if prefetch.is_some(){Some(self.reserve_scratch_for(cap.saturating_add(source.bytes.len()),Some(path)).ok_or_else(||DEFERRED.to_owned())?)}else{None};
  self.parsed_misses.fetch_add(1,Ordering::Relaxed);
  let normalized=art3m1s_emote::normalize_psb_header(&source.bytes).map_err(|e|e.to_string())?;
  let (backing,source_reservation,source_charge,extra)=match normalized {
   std::borrow::Cow::Borrowed(_)=>(source.bytes.clone(),source._reservation.clone(),Some(source._charge.clone()),0),
   std::borrow::Cow::Owned(bytes)=>{let extra=bytes.capacity();(Arc::new(bytes),None,None,extra)},
  };
  let mut size=0;
  let mut prepared=Prepared(unsafe{art3m1s_recovery_model_prepare_shared(backing.as_ptr(),backing.len(),cap,&mut size)});
  if prepared.0==0{return Err("recovered scene preparation failed or exceeded background allocation cap".into());}
  if !valid(){return Err(CANCELLED.into());}
  let bytes=size+extra+std::mem::size_of::<RecoveredModel>()+2*std::mem::size_of::<usize>();
  let reservation=if source._reservation.is_some(){self.reserve_with_credit_for(bytes,&mut scratch,prefetch.map(|_|path))}else{None};
  drop(scratch);
  let retained=reservation.is_some();
  self.parsed_live.fetch_add(bytes,Ordering::Relaxed);
  let model=Arc::new(RecoveredModel{id:prepared.0,_backing:backing,_source_reservation:source_reservation,_source_charge:source_charge,_reservation:reservation,_charge:Charge::observed(Owner::Source,bytes),bytes,live:self.parsed_live.clone()});prepared.0=0;
  if retained{*slot=Some(model.clone());}
  crate::core_info!("[recovered-model-cache] miss path={} bytes={} retained={} elapsed_us={} preparse={}",path,bytes,retained,start.elapsed().as_micros(),prefetch.is_some());
  Ok(model)
 }
}
