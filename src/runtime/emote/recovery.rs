//! Glue only: model evaluation stays in the recovered C adapter.
use super::*;
use std::ffi::CString;
use crate::render_pipeline::recovered::RecoveredPacket;
unsafe extern "C" {
 fn art3m1s_recovery_create(path:*const std::ffi::c_char,data:*const u8,len:usize,width:u32,height:u32)->u64;
 fn art3m1s_recovery_destroy(id:u64);
 fn art3m1s_recovery_command(id:u64,op:u32,label:*const std::ffi::c_char,args:*const f32,flags:u32)->i32;
 fn art3m1s_recovery_query(id:u64,op:u32,label:*const std::ffi::c_char,out:*mut f32)->i32;
 fn art3m1s_recovery_advance(id:u64,frames:f32)->i32;
 fn art3m1s_recovery_capture(id:u64)->u64;
}
pub(super) struct RecoveryInstance {id:u64,owner:std::thread::ThreadId,bytes:u64}
impl RecoveryInstance {
 pub(super) fn new(path:&str,bytes:&[u8],width:u32,height:u32)->Result<Self,String>{
  let path=CString::new(path).map_err(|_|"recovery path contains NUL")?;
  let id=unsafe{art3m1s_recovery_create(path.as_ptr(),bytes.as_ptr(),bytes.len(),width,height)};
  if id==0{return Err("recovered E-Mote create failed (owner thread, PSB v2/v3/v4 and 960x544 required)".into());}
  Ok(Self{id,owner:std::thread::current().id(),bytes:bytes.len() as u64})
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
  if unsafe{art3m1s_recovery_command(self.id,op,label.as_ptr(),args.as_ptr(),flags)}==1{Ok(())}else{Err(format!("recovery command {op} rejected (unsupported values or missing label)"))}
 }
 pub(super) fn query(&self,op:u32,label:&str)->Result<f32,String>{self.owner()?;let label=CString::new(label).map_err(|_|"recovery query label contains NUL")?;let mut value=0.;
  if unsafe{art3m1s_recovery_query(self.id,op,label.as_ptr(),&mut value)}==1{Ok(value)}else{Err("recovery query failed".into())}}
 pub(super) fn advance(&mut self,frames:f32)->bool{
  if self.owner().is_err(){return false;}unsafe{art3m1s_recovery_advance(self.id,frames)==1}
 }
 pub(super) fn build_commands(&mut self)->Result<Vec<DrawCommand>,String>{
  // Keep failures in the ordered list: frame_begin would erase an error set here.
  let packet=if self.owner().is_ok(){RecoveredPacket::from_handle(unsafe{art3m1s_recovery_capture(self.id)})}else{None}
   .unwrap_or_else(||{crate::core_warn!("recovered packet capture failed; submitting explicit failed snapshot");RecoveredPacket::failed()});
  let size=TextureInfo{width:960,height:544};
  Ok(vec![DrawCommand{texture:TextureId(0),size,transform:Affine2::IDENTITY,opacity:1.,blend:BlendMode::Alpha,color:ColorFilter::default(),clip:ClipRect::full(size),clip_bounds:None,shader:None,mesh:None,stencil:None,native_emote:None,recovered_packet:Some(packet)}])
 }
}
impl Drop for RecoveryInstance {fn drop(&mut self){unsafe{art3m1s_recovery_destroy(self.id)}}}
