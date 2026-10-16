//! Glue only: model evaluation stays in the recovered C adapter.
use super::*;
use std::ffi::CString;
use crate::render_pipeline::recovered::RecoveredPacket;
unsafe extern "C" {
 fn art3m1s_recovery_create_prepared(path:*const std::ffi::c_char,model:u64,width:u32,height:u32,lease:*mut std::ffi::c_void,release:extern "C" fn(*mut std::ffi::c_void))->u64;
 fn art3m1s_recovery_destroy(id:u64);
 fn art3m1s_recovery_command(id:u64,op:u32,label:*const std::ffi::c_char,args:*const f32,flags:u32)->i32;
 fn art3m1s_recovery_query(id:u64,op:u32,label:*const std::ffi::c_char,out:*mut f32)->i32;
 fn art3m1s_recovery_advance(id:u64,frames:f32)->i32;
 fn art3m1s_recovery_capture(id:u64)->u64;
 fn art3m1s_recovery_capture_batch(ids:*const u64,out:*mut u64,count:u32);
}
pub(super) struct RecoveryInstance {id:u64,owner:std::thread::ThreadId,bytes:u64,width:u32,height:u32}
impl RecoveryInstance {
 pub(super) fn new(path:&str,source:&Arc<super::super::emote_source_cache::Source>,width:u32,height:u32)->Result<Self,String>{
  crate::core_info!("[recovered-emote] create model={path} surface={width}x{height} bytes={}",source.bytes.len());
  let model=super::super::emote_source_cache::recovered(source,path)?;
  let path=CString::new(path).map_err(|_|"recovery path contains NUL")?;
  let model_id=model.id;
  let lease=Arc::into_raw(model) as *mut std::ffi::c_void;
  // C owns this lease only on success, until deferred owner-thread teardown.
  let id=unsafe{art3m1s_recovery_create_prepared(path.as_ptr(),model_id,width,height,lease,release_model)};
  if id==0{release_model(lease);return Err("recovered E-Mote create failed (owner thread, PSB v2/v3/v4 and valid logical canvas required)".into());}
  Ok(Self{id,owner:std::thread::current().id(),bytes:source.bytes.len() as u64,width,height})
 }
 fn owner(&self)->Result<(),String>{if self.owner==std::thread::current().id(){Ok(())}else{Err("recovered E-Mote operation requires its render owner thread".into())}}
 pub(super) fn source_bytes(&self)->u64{self.bytes}
 pub(super) fn command(&mut self,command:EmoteLayerCommand)->Result<(),String>{
  self.owner()?;
  let (op,label,args,flags)=match command {
   EmoteLayerCommand::SetScale{scale,origin_x,origin_y}=>(1,String::new(),[scale,origin_x,origin_y,0.],0),
   EmoteLayerCommand::SetCoord{x,y,z,angle}=>(2,String::new(),[x,y,z,angle],0),
   EmoteLayerCommand::SetVariable{label,value,frames,easing}=>(3,label,[value,frames,easing as f32,0.],0),
   EmoteLayerCommand::PlayTimeline{label,flags}=>(4,label,[0.;4],flags),
   EmoteLayerCommand::FadeInTimeline{label,frames,easing}=>(5,label,[frames,easing as f32,0.,0.],0),
   EmoteLayerCommand::FadeOutTimeline{label,frames,easing}=>(6,label,[frames,easing as f32,0.,0.],0),
   EmoteLayerCommand::StopTimeline{label}=>(7,label,[0.;4],0),
   EmoteLayerCommand::Pass=>(8,String::new(),[0.;4],0),
   EmoteLayerCommand::SetMeshDivisionRatio{ratio}=>(9,String::new(),[ratio,0.,0.,0.],0),
   EmoteLayerCommand::Step|EmoteLayerCommand::Skip=>return Err("recovery Step/Skip semantics are not yet implemented".into()),
  };
  let label=CString::new(label).map_err(|_|"recovery label contains NUL")?;
  if unsafe{art3m1s_recovery_command(self.id,op,label.as_ptr(),args.as_ptr(),flags)}==1{Ok(())}else{Err(format!("recovery command {op} rejected: label={:?} flags={flags} args={args:?}",label.to_string_lossy()))}
 }
 pub(super) fn query(&self,op:u32,label:&str)->Result<f32,String>{self.owner()?;let label=CString::new(label).map_err(|_|"recovery query label contains NUL")?;let mut value=0.;
  if unsafe{art3m1s_recovery_query(self.id,op,label.as_ptr(),&mut value)}==1{Ok(value)}else{Err("recovery query failed".into())}}
 pub(super) fn advance(&mut self,frames:f32)->bool{
  if self.owner().is_err(){return false;}unsafe{art3m1s_recovery_advance(self.id,frames)==1}
 }
 pub(super) fn id(&self)->u64{self.id}
 pub(super) fn build_commands(&mut self)->Result<Vec<DrawCommand>,String>{
  let handle=if self.owner().is_ok(){unsafe{art3m1s_recovery_capture(self.id)}}else{0};
  self.build_captured_commands(handle)
 }
 pub(super) fn build_captured_commands(&mut self,handle:u64)->Result<Vec<DrawCommand>,String>{
  // Keep failures in the ordered list: frame_begin would erase an error set here.
  let packet=if self.owner().is_ok(){RecoveredPacket::from_handle(handle)}else{None}
   .unwrap_or_else(||{crate::core_warn!("recovered packet capture failed; submitting explicit failed snapshot");RecoveredPacket::failed()});
  let size=TextureInfo{width:self.width,height:self.height};
  Ok(vec![DrawCommand{texture:TextureId(0),size,transform:Affine2::IDENTITY,opacity:1.,blend:BlendMode::Alpha,color:ColorFilter::default(),clip:ClipRect::full(size),clip_bounds:Some([0.,0.,self.width as f32,self.height as f32]),shader:None,mesh:None,stencil:None,native_emote:None,recovered_packet:Some(packet)}])
 }
}
impl Drop for RecoveryInstance {fn drop(&mut self){unsafe{art3m1s_recovery_destroy(self.id)}}}

extern "C" fn release_model(lease:*mut std::ffi::c_void){
 unsafe{drop(Arc::from_raw(lease as *const super::super::emote_source_cache::RecoveredModel));}
}

// Returned handles must all be adopted by RecoveredPacket in this owner call.
pub(super) fn capture_batch(ids:&[u64])->Vec<u64>{
 let mut out=vec![0;ids.len()];
 for (input,output) in ids.chunks(64).zip(out.chunks_mut(64)){unsafe{art3m1s_recovery_capture_batch(input.as_ptr(),output.as_mut_ptr(),input.len() as u32)}}
 out
}
