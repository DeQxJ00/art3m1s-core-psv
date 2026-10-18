//! External AGX1 packages generated from the game's fixed DX9 pixel wrapper.
//! This backend owns one host GXM context/render thread. Registrations live only
//! for the active renderer and are released before returning to the launcher.
use std::{cell::RefCell, collections::BTreeMap, ffi::CString};
use serde::{Deserialize,Serialize};
use crate::render_pipeline::draw::ShaderEffect;
const MAX_VALUES:usize=128;
#[derive(Deserialize,Serialize,Clone)]
struct Uniform {name:String,offset:usize,count:usize}
impl Uniform {
 fn bind(&self,dst:&mut [f32],effect:&ShaderEffect,alpha:f32,color:[f32;3]) {
  // The shared descriptor uses float32 slots. Script values override draw
  // defaults; absent components keep those defaults, never another program's values.
  if self.name=="alpha"{dst[0]=alpha;}
  else if self.name=="colorMultiply"{for(a,b)in dst.iter_mut().zip(color){*a=b;}}
  if let Some(values)=effect.uniforms.get(&self.name){for(a,b)in dst.iter_mut().zip(values){if b.is_finite(){*a=*b;}}}
 }
}
#[derive(Deserialize)]
struct Metadata {abi:u32,source_hash:String,uniforms:Vec<Uniform>}
#[derive(Deserialize,Serialize)]
struct Converted {abi:u32,source_hash:String,cg_hash:String,cg:String,uniforms:Vec<Uniform>}
struct Program {handle:u32,uniforms:Vec<Uniform>,builtin_gray:bool,builtin_cmul:bool,builtin_sepia:bool,builtin_mosaic:bool,builtin_blur:bool,builtin_kawase:bool,translation_kind:u32}
thread_local!{static PROGRAMS:RefCell<BTreeMap<String,Program>>=RefCell::new(BTreeMap::new());}
thread_local!{static REVISION:std::cell::Cell<u64>=const{std::cell::Cell::new(0)};}
fn changed(){REVISION.with(|r|r.set(r.get().saturating_add(1)));}
pub(super) fn revision()->u64{REVISION.with(|r|r.get())}
#[repr(C)]
#[derive(Clone,Copy)]
pub(super) struct CustomDraw {pub program:u32,pub values:[f32;MAX_VALUES],pub user_texture:u64}
impl Default for CustomDraw {fn default()->Self{Self{program:0,values:[0.;MAX_VALUES],user_texture:0}}}
unsafe extern "C" {
 fn art3m1s_gxm_shader_stage(stage:i32);
 fn art3m1s_gxm_external_cache_path(file:*const std::ffi::c_char,out:*mut std::ffi::c_char,size:usize)->i32;
 fn art3m1s_gxm_external_shared_cache_path(file:*const std::ffi::c_char,out:*mut std::ffi::c_char,size:usize)->i32;
 fn art3m1s_gxm_external_load_cached(id:*const std::ffi::c_char,key:*const std::ffi::c_char,cg:*const std::ffi::c_char)->u32;
 fn art3m1s_gxm_external_compile(id:*const std::ffi::c_char,key:*const std::ffi::c_char,cg:*const std::ffi::c_char)->u32;
 fn art3m1s_gxm_external_conversion_enabled()->i32;
 fn art3m1s_gxm_external_compiler_end();
 fn art3m1s_gxm_external_register(data:*const u8,len:usize)->u32;
 fn art3m1s_gxm_external_uniform(handle:u32,name:*const std::ffi::c_char,offset:u32,count:u32)->i32;
 fn art3m1s_gxm_external_release(handle:u32);
}
fn hash(data:&[u8])->String {format!("{:016x}",data.iter().fold(0xcbf29ce484222325u64,|n,b|(n^u64::from(*b)).wrapping_mul(0x100000001b3)))}
fn parse<'a>(source:&[u8],package:&'a[u8])->Result<(Metadata,&'a[u8]),String>{
 parse_hashed(&hash(source),package)
}
fn parse_hashed<'a>(source_hash:&str,package:&'a[u8])->Result<(Metadata,&'a[u8]),String>{
 if package.len()<12||&package[..4]!=b"AGX1"{return Err("not an AGX1 external shader package".into());}
 let m=u32::from_le_bytes(package[4..8].try_into().unwrap()) as usize;
 let b=u32::from_le_bytes(package[8..12].try_into().unwrap()) as usize;
 if m>32768||!(156..=1024*1024).contains(&b)||12+m+b!=package.len(){return Err("external shader length/limit mismatch".into());}
 let meta:Metadata=serde_json::from_slice(&package[12..12+m]).map_err(|e|e.to_string())?;
 if meta.abi!=1||meta.source_hash!=source_hash{return Err("external shader ABI/source hash mismatch; recompile companion".into());}
 let mut occupied=[false;MAX_VALUES];let mut names=std::collections::HashSet::new();
 for u in &meta.uniforms{
  if u.name.is_empty()||u.name.len()>63||u.name.starts_with("art_")||!u.name.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_')
    || !names.insert(&u.name)||u.count==0||u.count>MAX_VALUES||u.offset>MAX_VALUES-u.count {return Err("external uniform bounds/name".into());}
  for i in u.offset..u.offset+u.count{if occupied[i]{return Err("external uniform overlap".into());}occupied[i]=true;}
 }
 let gxp=&package[12+m..];
 let declared=u32::from_le_bytes(gxp[8..12].try_into().unwrap()) as usize;
 if &gxp[..4]!=b"GXP\0"||declared<156||declared>b||(b!=declared&&b!=((declared+3)&!3)){return Err("GXP size/magic mismatch".into());}
 Ok((meta,gxp))
}
pub(super) fn register(id:&str,source:&[u8],package:&[u8])->Result<(),String>{
 register_hashed(id,&hash(source),package)
}
fn register_hashed(id:&str,source_hash:&str,package:&[u8])->Result<(),String>{
 if id.is_empty()||id.len()>128||matches!(id,"group-composite"|"alpha-mask"|"rule-trans"|"sprite"){return Err("reserved/invalid shader id".into());}
 let(meta,gxp)=parse_hashed(source_hash,package)?;
 let handle=unsafe{art3m1s_gxm_external_register(gxp.as_ptr(),gxp.len())};
 if handle==0{return Err("host rejected GXP program".into());}
 for u in &meta.uniforms{
  let name=CString::new(u.name.as_str()).unwrap();
  if unsafe{art3m1s_gxm_external_uniform(handle,name.as_ptr(),u.offset as u32,u.count as u32)}==0{
   unsafe{art3m1s_gxm_external_release(handle)};return Err(format!("GXP uniform mismatch: {}",u.name));
  }
 }
 changed();PROGRAMS.with(|p|{if let Some(old)=p.borrow_mut().insert(id.into(),Program{handle,uniforms:meta.uniforms,builtin_gray:false,builtin_cmul:false,builtin_sepia:false,builtin_mosaic:false,builtin_blur:false,builtin_kawase:false,translation_kind:0}){unsafe{art3m1s_gxm_external_release(old.handle)}}});
 Ok(())
}
#[cfg(test)]
pub(super) fn register_test_source(id:&str,source:&[u8])->Result<(),String>{
 // Scene tests install synthetic programs directly without a device compiler.
 let (_,layout)=crate::render_pipeline::hlsl::translate_cg_effect(source)?;
 let uniforms:Vec<_>=layout.into_iter().map(|u|serde_json::json!({"name":u.name,"offset":u.offset,"count":u.count})).collect();
 register(id,source,&tests::fixture(source,serde_json::json!(uniforms)))
}
pub(super) fn register_source(id:&str,source:&[u8])->Result<(),String>{
 register_source_at(id,id,source)
}
// Only source-hash-selected implementations enter here. Mobile gray/sepia have
// additive RGB terms and deliberately do not inherit PC premultiplication proofs.
fn builtin_contract(name:&str)->&str {
 let name=super::bundled_effects::contract(name);
 match name {
  "glsl_mb_blur_h"|"glsl_sw_blur_h"=>"blur_h",
  "glsl_mb_blur_v"|"glsl_sw_blur_v"=>"blur_v",
  "glsl_mb_cmul"|"glsl_sw_cmul"=>"cmul",
  _=>name,
 }
}
fn register_builtin(id:&str,name:&str,canonical_hash:&str,package:&[u8],fallback:bool)->Result<(),String>{
 unsafe{art3m1s_gxm_shader_stage(3)};
 register_hashed(id,canonical_hash,package)?;
  let implementation=name;
  let name=builtin_contract(name);
  PROGRAMS.with(|p|{if let Some(program)=p.borrow_mut().get_mut(id){program.builtin_gray=name=="gray";program.builtin_cmul=name=="cmul";program.builtin_sepia=name=="sepia";program.builtin_mosaic=name=="mosaic";program.builtin_blur=matches!(name,"blur_h"|"blur_v");program.builtin_kawase=matches!(name,"blur_k"|"blur_kx"|"blur_ky");program.translation_kind=match name{"blur_h"=>1,"blur_v"=>2,"blur_k"=>3,"blur_kx"=>4,"blur_ky"=>5,"gray"=>6,_=>0};}});
  crate::core_info!("[shader-builtin] id={} implementation={} bytes={} fallback={}",id,implementation,package.len(),fallback);
 Ok(())
}
pub(super) fn register_source_at(id:&str,file:&str,source:&[u8])->Result<(),String>{
 if id.is_empty()||id.len()>128||matches!(id,"group-composite"|"alpha-mask"|"rule-trans"|"sprite"){return Err("reserved/invalid shader id".into());}
 if let Some((name,canonical_hash,package))=super::bundled_effects::lookup(&hash(source)){
  match register_builtin(id,name,canonical_hash,package,false){
   Ok(())=>return Ok(()),
   Err(e)=>{crate::core_warn!("[shader-builtin] unavailable id={}: {}; trying cache",id,e);},
  }
 }
 let mut cache_error=match register_cached_source_at(id,file,source,false){
  Ok(())=>return Ok(()),Err(e)=>e,
 };
 if let Some((name,canonical_hash,package))=super::bundled_effects::equivalent(&hash(source)){
  match register_builtin(id,name,canonical_hash,package,false){
   Ok(())=>return Ok(()),Err(e)=>cache_error=e,
  }
 }
 if unsafe{art3m1s_gxm_external_conversion_enabled()}!=0 && !source.is_empty(){
  match register_cached_source_at(id,file,source,true){
   Ok(())=>return Ok(()),Err(e)=>cache_error=format!("{cache_error}; generation: {e}"),
  }
 }
 if let Some((name,canonical_hash,package))=super::bundled_effects::fallback(file){
  crate::core_warn!("[shader-fallback] id={} file={} implementation={}; cache unavailable: {}; same-function PSV approximation",id,file,name,cache_error);
  return register_builtin(id,name,canonical_hash,package,true);
 }
 Err(format!("no built-in match, valid shader cache or mapped PSV fallback: {cache_error}"))
}
fn register_cached_source_at(id:&str,file:&str,source:&[u8],allow_generate:bool)->Result<(),String>{
 if source.is_empty(){return Err("source unavailable; cannot validate shader cache".into());}
 let id_c=CString::new(file).map_err(|_|"invalid shader path")?;
 let mut base=[0u8;1024];
 let cache=if unsafe{art3m1s_gxm_external_cache_path(id_c.as_ptr(),base.as_mut_ptr().cast(),base.len())}!=0{
  Some(String::from_utf8_lossy(&base[..base.iter().position(|b|*b==0).unwrap_or(base.len())]).to_string())
 }else{None};
 let mut shared_base=[0u8;1024];
 let shared=if unsafe{art3m1s_gxm_external_shared_cache_path(id_c.as_ptr(),shared_base.as_mut_ptr().cast(),shared_base.len())}!=0{
  Some(String::from_utf8_lossy(&shared_base[..shared_base.iter().position(|b|*b==0).unwrap_or(shared_base.len())]).to_string())
 }else{None};
 unsafe{art3m1s_gxm_shader_stage(5)};
 let converted=prepare_conversion_shared(source,cache.as_deref(),shared.as_deref(),allow_generate)?;
 let key=CString::new(format!("{}-{}",hash(source),hash(converted.cg.as_bytes()))).unwrap();
 let source_c=CString::new(converted.cg).map_err(|_|"Cg contains NUL")?;
 // The host separately enforces the compile setting and checks GXP cache first.
 let handle=unsafe{if allow_generate{art3m1s_gxm_external_compile(id_c.as_ptr(),key.as_ptr(),source_c.as_ptr())}
  else{art3m1s_gxm_external_load_cached(id_c.as_ptr(),key.as_ptr(),source_c.as_ptr())}};
 if handle==0{return Err("GXP cache missing, stale or rejected; see host.log".into());}
 let uniforms=converted.uniforms;
 for u in &uniforms{let name=CString::new(u.name.as_str()).unwrap();
  if unsafe{art3m1s_gxm_external_uniform(handle,name.as_ptr(),u.offset as u32,u.count as u32)}==0{
   unsafe{art3m1s_gxm_external_release(handle)};return Err(format!("GXP uniform mismatch: {}",u.name));
  }
 }
 changed();PROGRAMS.with(|p|{if let Some(old)=p.borrow_mut().insert(id.into(),Program{handle,uniforms,builtin_gray:false,builtin_cmul:false,builtin_sepia:false,builtin_mosaic:false,builtin_blur:false,builtin_kawase:false,translation_kind:0}){unsafe{art3m1s_gxm_external_release(old.handle)}}});Ok(())
}
fn conversion_valid(c:&Converted,source:&[u8])->bool{
 if c.abi!=2||c.source_hash!=hash(source)||c.cg.len()>256*1024||c.cg_hash!=hash(c.cg.as_bytes()){return false;}
 let mut occupied=[false;MAX_VALUES];let mut names=std::collections::HashSet::new();
 for u in &c.uniforms{
  if u.name.is_empty()||u.name.len()>63||u.name.starts_with("art_")||!u.name.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_')||!names.insert(&u.name)||u.count==0||u.count>MAX_VALUES||u.offset>MAX_VALUES-u.count{return false;}
  for i in u.offset..u.offset+u.count{if occupied[i]{return false;}occupied[i]=true;}
 }true
}
fn write_conversion_file(path:&str,data:&[u8])->std::io::Result<()>{
 let tmp=format!("{path}.tmp");std::fs::write(&tmp,data)?;std::fs::rename(&tmp,path)
}
#[cfg(test)]
fn prepare_conversion(source:&[u8],base:Option<&str>,allow:bool)->Result<Converted,String>{
 prepare_conversion_shared(source,base,None,allow)
}
fn prepare_conversion_shared(source:&[u8],base:Option<&str>,shared:Option<&str>,allow:bool)->Result<Converted,String>{
 for (scope,base) in [("game",base),("shared",shared)]{
 if let Some(base)=base{
  let path=format!("{base}.conversion.json");
  if std::fs::metadata(&path).is_ok_and(|m|m.len()<=512*1024){
   if let Ok(data)=std::fs::read(path){if let Ok(c)=serde_json::from_slice::<Converted>(&data){
    if conversion_valid(&c,source)&&std::fs::read(format!("{base}.cg")).is_ok_and(|s|s==c.cg.as_bytes()){
     crate::core_info!("[shader-cache] Cg hit scope={} path={}",scope,base);return Ok(c);
    }
   }}
  }
 }
 }
 if !allow{return Err("automatic shader conversion disabled; no matching Cg cache".into());}
 unsafe{art3m1s_gxm_shader_stage(4)};
 let(cg,layout)=crate::render_pipeline::hlsl::translate_cg_effect(source)?;
 let c=Converted{abi:2,source_hash:hash(source),cg_hash:hash(cg.as_bytes()),cg,uniforms:layout.into_iter().map(|u|Uniform{name:u.name,offset:u.offset,count:u.count}).collect()};
 if let Some(base)=base{
  let result=write_conversion_file(&format!("{base}.cg"),c.cg.as_bytes()).and_then(|_|write_conversion_file(&format!("{base}.conversion.json"),&serde_json::to_vec(&c).unwrap()));
  if let Err(e)=result{crate::core_warn!("[shader-cache] Cg save failed: {}",e);}
 }Ok(c)
}
pub(super) fn clear(){changed();unsafe{art3m1s_gxm_external_compiler_end()};PROGRAMS.with(|p|{for(_,v)in std::mem::take(&mut *p.borrow_mut()){unsafe{art3m1s_gxm_external_release(v.handle)}}});}
pub(super) fn registered(id:&str)->bool{PROGRAMS.with(|p|p.borrow().contains_key(id))}
// Source identity, not a script-selected ID/path, establishes the color contract.
// All pointwise color filters share the same retention/translation policy. Unknown
// programs keep the general screen-space cache and its full invalidation key.
fn color_contract(effect:&ShaderEffect)->u32 {
 if effect.mask_texture.is_some() || effect.user_texture.is_some()
    || effect.uniforms.get("alpha").is_some_and(|v|v.as_slice()!=[1.])
    || effect.uniforms.values().flatten().any(|v|!v.is_finite()) {return 0;}
 PROGRAMS.with(|p|p.borrow().get(&effect.name).map_or(0,|p|{
  if p.builtin_gray {return 1;}
  // RGB gains preserve premultiplication only in this range. sepia2's additive
  // offset is a different contract and must keep its isolation boundary.
  if (p.builtin_cmul||p.builtin_sepia) && ["red","green","blue"].iter().all(|key|
      effect.uniforms.get(*key).is_some_and(|v|v.len()==1&&(0. ..=1.).contains(&v[0]))) {
   if p.builtin_cmul {2} else {3}
  } else {0}
 }))
}
pub(super) fn premultiplied_color(effect:&ShaderEffect)->bool {color_contract(effect)!=0}
pub(super) fn premultiplied_gray(effect:&ShaderEffect)->bool {color_contract(effect)==1}
#[cfg(test)]
pub(super) fn mark_test_builtin_sepia(id:&str){PROGRAMS.with(|p|p.borrow_mut().get_mut(id).unwrap().builtin_sepia=true);changed();}
#[cfg(test)]
pub(super) fn mark_test_builtin_cmul(id:&str){PROGRAMS.with(|p|p.borrow_mut().get_mut(id).unwrap().builtin_cmul=true);changed();}
#[cfg(test)]
pub(super) fn mark_test_builtin_gray(id:&str){PROGRAMS.with(|p|p.borrow_mut().get_mut(id).unwrap().builtin_gray=true);}
pub(super) fn encode(effect:Option<&ShaderEffect>,alpha:f32,color:[f32;3])->CustomDraw{
 let mut d=CustomDraw::default();let Some(e)=effect else{return d;};
 PROGRAMS.with(|p|{let programs=p.borrow();if let Some(p)=programs.get(&e.name){
  d.program=p.handle;d.user_texture=e.user_texture.map_or(0,|t|t.0);
  for u in &p.uniforms{
   let dst=&mut d.values[u.offset..u.offset+u.count];
   u.bind(dst,e,alpha,color);
  }
 }});d
}
#[cfg(test)]
mod tests{
 use super::*;
 #[test]fn different_source_generates_cache_only_when_enabled_then_reuses_it(){
  let src=b"float alpha; void vs(){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(){result=float4(0.2,0.4,0.6,alpha);}";
  let root=std::env::temp_dir().join(format!("art3-generate-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
  std::fs::create_dir(&root).unwrap();let base=root.join("effect").to_str().unwrap().to_owned();
  TEST_CACHE.with(|v|*v.borrow_mut()=Some(base.clone()));COMPILE_CALLS.with(|v|v.set(0));
  assert!(super::super::bundled_effects::equivalent(&hash(src)).is_none());
  assert!(register_source_at("generate","custom/effect.hlsl",src).is_err());
  ALLOW_GENERATE.with(|v|v.set(true));
  register_source_at("generate","custom/effect.hlsl",src).unwrap();
  assert!(std::path::Path::new(&format!("{base}.conversion.json")).exists());
  assert!(!PROGRAMS.with(|p|p.borrow()["generate"].builtin_gray));
  assert_eq!(COMPILE_CALLS.with(|v|v.get()),1);
  ALLOW_GENERATE.with(|v|v.set(false));
  register_source_at("generate","custom/effect.hlsl",src).unwrap();
  assert_eq!(COMPILE_CALLS.with(|v|v.get()),1);
  let changed:Vec<u8>=src.iter().map(|b|if *b==b'2'{b'3'}else{*b}).collect();
  assert!(register_source_at("generate","custom/effect.hlsl",&changed).is_err());
  ALLOW_GENERATE.with(|v|v.set(true));FAIL_COMPILE.with(|v|v.set(true));
  assert!(register_source_at("generate","custom/effect.hlsl",&changed).is_err());
  assert!(registered("generate"));
  FAIL_COMPILE.with(|v|v.set(false));ALLOW_GENERATE.with(|v|v.set(false));TEST_CACHE.with(|v|*v.borrow_mut()=None);
  clear();std::fs::remove_dir_all(root).unwrap();
 }
 #[test]fn unified_parameter_descriptions_match_packages_and_binding(){
  let schema:serde_json::Value=serde_json::from_str(include_str!("bundled/parameters.json")).unwrap();
  let manifest:serde_json::Value=serde_json::from_str(include_str!("bundled/manifest.json")).unwrap();
  for effect in schema["effects"].as_array().unwrap(){
   let entry=manifest.as_array().unwrap().iter().find(|e|e["implementation"]==effect["implementation"]).unwrap();
   let(_,hash,package)=super::super::bundled_effects::lookup(entry["source_hash"].as_str().unwrap()).unwrap();
   let(meta,_)=parse_hashed(hash,package).unwrap();
   assert_eq!(meta.uniforms.len(),effect["parameters"].as_array().unwrap().len());
   for(u,desc)in meta.uniforms.iter().zip(effect["parameters"].as_array().unwrap()){
    assert_eq!(u.name,desc["name"].as_str().unwrap());assert_eq!(u.offset,desc["offset"].as_u64().unwrap()as usize);assert_eq!(u.count,desc["count"].as_u64().unwrap()as usize);
    let mut values=vec![0.;u.count];
    let e=ShaderEffect{name:"probe".into(),uniforms:Default::default(),mask_texture:None,user_texture:None};
    u.bind(&mut values,&e,1.,[1.;3]);
    assert_eq!(values,desc["default"].as_array().unwrap().iter().map(|v|v.as_f64().unwrap()as f32).collect::<Vec<_>>());
   }
  }
 }
 #[test]fn fallback_is_platform_aware_and_never_guesses_custom_or_aa_effects(){
  let fallback=super::super::bundled_effects::fallback;
  assert_eq!(fallback("system/shader/pc/gray.hlsl").unwrap().0,"gray");
  assert_eq!(fallback("SYSTEM\\SHADER\\MB\\GRAY.GLSL").unwrap().0,"gray");
  assert_eq!(fallback("system/shader/sw/sepia.glsl").unwrap().0,"sepia");
  assert_eq!(fallback("system/shader/ps/blur_h_p.sb").unwrap().0,"blur_h");
  for path in ["custom/gray.hlsl","system/shader/pc/custom/gray.hlsl","system/shader/pc/../gray.hlsl","system/shader/pc/blur_h_aa.hlsl","system/shader/ps/blur_h_v.sb"]{assert!(fallback(path).is_none(),"{path}");}
 }
 #[test]fn valid_cache_precedes_fallback_and_invalid_cache_does_not_block_it(){
  let src=b"float alpha; void vs(){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(){result=float4(alpha,0,0,1);}";
  let root=std::env::temp_dir().join(format!("art3-fallback-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
  std::fs::create_dir(&root).unwrap();let base=root.join("gray").to_str().unwrap().to_string();
  TEST_CACHE.with(|p|*p.borrow_mut()=Some(base.clone()));
  // Cache miss selects the mapped PSV implementation without creating a conversion.
  register_source_at("fallback_probe","system/shader/pc/gray.hlsl",src).unwrap();
  assert!(PROGRAMS.with(|p|p.borrow()["fallback_probe"].builtin_gray));
  assert!(!root.join("gray.conversion.json").exists());
  prepare_conversion(src,Some(&base),true).unwrap();
  register_source_at("fallback_probe","system/shader/pc/gray.hlsl",src).unwrap();
  assert!(!PROGRAMS.with(|p|p.borrow()["fallback_probe"].builtin_gray));
  FAIL_COMPILE.with(|f|f.set(true));
  register_source_at("fallback_probe","system/shader/pc/gray.hlsl",src).unwrap();
  assert!(PROGRAMS.with(|p|p.borrow()["fallback_probe"].builtin_gray));
  FAIL_COMPILE.with(|f|f.set(false));
  register_source_at("fallback_probe","system/shader/pc/gray.hlsl",b"changed").unwrap();
  assert!(PROGRAMS.with(|p|p.borrow()["fallback_probe"].builtin_gray));
  assert!(register_source_at("fallback_probe","custom/unknown.hlsl",b"changed").is_err());
  assert!(PROGRAMS.with(|p|p.borrow()["fallback_probe"].builtin_gray));
  register_source_at("mobile_probe","system/shader/mb/gray.glsl",&[]).unwrap();
  assert!(PROGRAMS.with(|p|p.borrow()["mobile_probe"].builtin_gray));
  TEST_CACHE.with(|p|*p.borrow_mut()=None);clear();std::fs::remove_dir_all(root).unwrap();
 }
 #[test]fn portable_contracts_do_not_reuse_pc_additive_color_proofs(){
  assert_eq!(builtin_contract("glsl_mb_blur_h"),"blur_h");
  assert_eq!(builtin_contract("glsl_sw_blur_v"),"blur_v");
  assert_eq!(builtin_contract("glsl_mb_cmul"),"cmul");
  assert_ne!(builtin_contract("glsl_mb_gray"),"gray");
  assert_ne!(builtin_contract("glsl_sw_sepia"),"sepia");
 }
 #[test]fn shared_cg_cache_fallback_priority_and_mismatch(){
  let source=b"float alpha; void vs(){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(){result=float4(alpha,0,0,1);}";
  let root=std::env::temp_dir().join(format!("art3-shared-cg-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
  std::fs::create_dir(&root).unwrap();
  let local=root.join("local").to_str().unwrap().to_string();let shared=root.join("shared").to_str().unwrap().to_string();
  let original=prepare_conversion(source,Some(&shared),true).unwrap();
  let shared_bytes=std::fs::read(format!("{shared}.conversion.json")).unwrap();
  assert_eq!(prepare_conversion_shared(source,Some(&local),Some(&shared),false).unwrap().cg,original.cg);
  assert!(!std::path::Path::new(&format!("{local}.cg")).exists());
  assert!(prepare_conversion_shared(b"other-source",Some(&local),Some(&shared),false).is_err());
  let mut own=prepare_conversion(source,Some(&local),true).unwrap();own.cg.push_str("\n// per-game variant");own.cg_hash=hash(own.cg.as_bytes());
  std::fs::write(format!("{local}.cg"),&own.cg).unwrap();std::fs::write(format!("{local}.conversion.json"),serde_json::to_vec(&own).unwrap()).unwrap();
  assert_eq!(prepare_conversion_shared(source,Some(&local),Some(&shared),false).unwrap().cg,own.cg);
  std::fs::write(format!("{local}.cg"),b"corrupt").unwrap();
  assert_eq!(prepare_conversion_shared(source,Some(&local),Some(&shared),false).unwrap().cg,original.cg);
  assert_eq!(std::fs::read(format!("{shared}.conversion.json")).unwrap(),shared_bytes);
  std::fs::write(format!("{shared}.cg"),b"corrupt").unwrap();
  assert!(prepare_conversion_shared(source,Some(&local),Some(&shared),false).is_err());
  assert!(prepare_conversion_shared(source,Some(&local),Some(&shared),true).is_ok());
  assert_eq!(std::fs::read(format!("{shared}.cg")).unwrap(),b"corrupt");
  std::fs::remove_dir_all(root).unwrap();
 }
 #[test]fn cg_cache_reuses_without_conversion_and_invalidates_changed_source(){
  let source=b"float alpha; void vs(){resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;} void ps(){result=float4(alpha,0,0,1);}";
  let base=std::env::temp_dir().join(format!("art3-cg-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
  let base=base.to_str().unwrap();assert!(prepare_conversion(source,Some(base),false).is_err());
  let c=prepare_conversion(source,Some(base),true).unwrap();
  assert_eq!(prepare_conversion(source,Some(base),false).unwrap().cg,c.cg);
  assert!(prepare_conversion(b"changed",Some(base),false).is_err());
  std::fs::write(format!("{base}.cg"),b"corrupt").unwrap();assert!(prepare_conversion(source,Some(base),false).is_err());
  prepare_conversion(source,Some(base),true).unwrap();
  assert!(prepare_conversion(source,Some(base),false).is_ok());
  std::fs::remove_file(format!("{base}.cg")).unwrap();std::fs::remove_file(format!("{base}.conversion.json")).unwrap();
 }
 #[test]fn bundled_programs_cover_portable_sources_and_reject_changed_source(){
  let hash="not a known source";assert!(super::super::bundled_effects::lookup(hash).is_none());
  let manifest:serde_json::Value=serde_json::from_str(include_str!("bundled/manifest.json")).unwrap();
  assert_eq!(manifest.as_array().unwrap().len(),39);
  let hashes:std::collections::HashSet<_>=manifest.as_array().unwrap().iter().map(|e|e["source_hash"].as_str().unwrap()).collect();
  assert_eq!(hashes.len(),39);
  let packages:std::collections::HashSet<_>=manifest.as_array().unwrap().iter().map(|e|e["package"].as_str().unwrap()).collect();
  assert!(packages.len()<hashes.len(),"equivalent sources must share packages");
  for e in manifest.as_array().unwrap(){let(_,canonical_hash,package)=super::super::bundled_effects::lookup(e["source_hash"].as_str().unwrap()).unwrap();
   assert!(parse_hashed(canonical_hash,package).is_ok(),"{}",e["name"]);
   assert!(parse_hashed("changed source fingerprint",package).is_err());
   assert_eq!(&package[..4],b"AGX1");let m=u32::from_le_bytes(package[4..8].try_into().unwrap())as usize;
   let meta:Metadata=serde_json::from_slice(&package[12..12+m]).unwrap();assert_eq!(meta.source_hash,canonical_hash);assert_eq!(canonical_hash,e["canonical_hash"].as_str().unwrap());
   let b=u32::from_le_bytes(package[8..12].try_into().unwrap())as usize;assert_eq!(12+m+b,package.len());
   assert!(unsafe{art3m1s_gxm_external_register(package[12+m..].as_ptr(),b)}!=0);
  }
 }
 #[unsafe(no_mangle)]extern "C" fn art3m1s_gxm_external_conversion_enabled()->i32{ALLOW_GENERATE.with(|v|i32::from(v.get()))}
 thread_local!{static ALLOW_GENERATE:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};static COMPILE_CALLS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
 thread_local!{static TEST_CACHE:RefCell<Option<String>>=const{RefCell::new(None)};static FAIL_COMPILE:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}
 #[unsafe(no_mangle)]extern "C" fn art3m1s_gxm_external_cache_path(_:*const std::ffi::c_char,out:*mut std::ffi::c_char,size:usize)->i32{
  TEST_CACHE.with(|p|{let p=p.borrow();let Some(p)=p.as_ref()else{return 0;};if p.len()+1>size{return 0;}
   unsafe{std::ptr::copy_nonoverlapping(p.as_ptr(),out.cast(),p.len());*out.add(p.len())=0;}1})
 }
 #[unsafe(no_mangle)]extern "C" fn art3m1s_gxm_external_shared_cache_path(_:*const std::ffi::c_char,_:*mut std::ffi::c_char,_:usize)->i32{0}
 #[unsafe(no_mangle)]extern "C" fn art3m1s_gxm_external_load_cached(_:*const std::ffi::c_char,_:*const std::ffi::c_char,_:*const std::ffi::c_char)->u32{FAIL_COMPILE.with(|f|if f.get(){0}else{7})}
 #[unsafe(no_mangle)]extern "C" fn art3m1s_gxm_external_compile(_:*const std::ffi::c_char,_:*const std::ffi::c_char,_:*const std::ffi::c_char)->u32{COMPILE_CALLS.with(|v|v.set(v.get()+1));FAIL_COMPILE.with(|f|if f.get(){0}else{7})}
 #[unsafe(no_mangle)]extern "C" fn art3m1s_gxm_shader_stage(_:i32){}
 #[unsafe(no_mangle)]extern "C" fn art3m1s_gxm_external_compiler_end(){}
 #[unsafe(no_mangle)]extern "C" fn art3m1s_gxm_external_register(_:*const u8,_:usize)->u32{7}
 #[unsafe(no_mangle)]extern "C" fn art3m1s_gxm_external_uniform(_:u32,_:*const std::ffi::c_char,_:u32,_:u32)->i32{1}
 #[unsafe(no_mangle)]extern "C" fn art3m1s_gxm_external_release(_:u32){}
 pub(super) fn fixture(source:&[u8],uniforms:serde_json::Value)->Vec<u8>{
  let m=serde_json::to_vec(&serde_json::json!({"abi":1,"source_hash":hash(source),"uniforms":uniforms})).unwrap();
  let mut b=vec![0;156];b[..4].copy_from_slice(b"GXP\0");b[8..12].copy_from_slice(&156u32.to_le_bytes());
  let mut p=b"AGX1".to_vec();p.extend((m.len() as u32).to_le_bytes());p.extend((b.len() as u32).to_le_bytes());p.extend(m);p.extend(b);p
 }
 #[test]fn package_checks_source_length_uniform_bounds_and_atomic_replacement(){
  let src=b"test";let good=fixture(src,serde_json::json!([{"name":"alpha","offset":0,"count":1},{"name":"weights","offset":1,"count":8}]));
  assert!(parse(src,&good).is_ok());assert!(parse(b"changed",&good).is_err());assert!(parse(src,&good[..good.len()-1]).is_err());
  let bad=fixture(src,serde_json::json!([{"name":"bad","offset":127,"count":8}]));assert!(parse(src,&bad).is_err());
  register("test_external",src,&good).unwrap();assert!(registered("test_external"));
  assert!(register("test_external",b"changed",&good).is_err());assert!(registered("test_external"));
  let e=ShaderEffect{name:"test_external".into(),uniforms:[("weights".into(),vec![0.5,0.25])].into(),mask_texture:None,user_texture:None};
  let d=encode(Some(&e),0.7,[1.;3]);assert_eq!(&d.values[..4],&[0.7,0.5,0.25,0.]);clear();assert!(!registered("test_external"));
 }
}

// Restrict the source-cache optimization to the source-hash-verified built-in.
// Mask/user inputs and invalid coordinates keep the original isolated route.
pub(super) fn cacheable_mosaic(effect:&ShaderEffect)->bool {
 effect.mask_texture.is_none() && effect.user_texture.is_none()
 && ["size","ratio"].iter().all(|key|effect.uniforms.get(*key)
     .is_some_and(|v|v.len()==1 && v[0].is_finite() && v[0]>0.))
 && revision()!=u64::MAX
 && PROGRAMS.with(|p|p.borrow().get(&effect.name).is_some_and(|p|p.builtin_mosaic))
}
// Verified mosaic only selects a bilinear sample. Verified H/V blur is a
// nonnegative weighted sum. With alpha=1 and no mask, both preserve RGB<=A;
// a neutral source-over wrapper can be removed while the filter still runs.
// Source-hash-verified coordinate contracts, not script IDs. Zero means the
// effect depends on screen coordinates or has not been audited for translation.
// ABI: Gaussian H/V=1/2, Kawase XY/X/Y=3/4/5, pointwise color=6. Scale changes are not translation reuse.
pub(super) fn translation_kind(effect:&ShaderEffect)->u32 {
 if premultiplied_color(effect){return 6;}
 if effect.mask_texture.is_some() || effect.user_texture.is_some()
    || effect.uniforms.get("alpha").is_some_and(|v|v.as_slice()!=[1.])
    || effect.uniforms.values().flatten().any(|v|!v.is_finite()) {return 0;}
 let kind=PROGRAMS.with(|p|p.borrow().get(&effect.name).map_or(0,|p|p.translation_kind));
 match kind {
  1|2 if premultiplied_spatial_filter(effect)
     && effect.uniforms.get(if kind==1{"width"}else{"height"})
       .is_some_and(|v|v.len()==1&&v[0]>=0.) => kind,
  3..=5 if verified_kawase(effect) => kind,
  6 if premultiplied_gray(effect) => kind,
  _=>0,
 }
}
pub(super) fn verified_kawase(effect:&ShaderEffect)->bool {
 PROGRAMS.with(|p|p.borrow().get(&effect.name).is_some_and(|p|p.builtin_kawase))
    && premultiplied_spatial_filter(effect)
}
pub(super) fn premultiplied_spatial_filter(effect:&ShaderEffect)->bool {
 if effect.mask_texture.is_some() || effect.user_texture.is_some()
    || effect.uniforms.get("alpha").is_some_and(|v|v.as_slice()!=[1.]) {return false;}
 if cacheable_mosaic(effect){return true;}
 // Source-verified Kawase kernels average two/four RGBA samples with fixed
 // positive weights summing to one. Keep every blur pass; only its neutral
 // parent isolation can disappear. User IDs alone never establish this proof.
 if PROGRAMS.with(|p|p.borrow().get(&effect.name).is_some_and(|p|p.builtin_kawase)) {
  return ["offset","size"].iter().all(|key|effect.uniforms.get(*key)
   .is_some_and(|v|v.len()==1 && v[0].is_finite()));
 }
 PROGRAMS.with(|p|p.borrow().get(&effect.name).is_some_and(|p|p.builtin_blur))
    && effect.uniforms.get("weights").is_some_and(|v|v.len()==8 && v.iter().all(|w|w.is_finite()&&(0. ..=1.).contains(w)))
}
#[cfg(test)]
pub(super) fn mark_test_builtin_mosaic(id:&str){PROGRAMS.with(|p|p.borrow_mut().get_mut(id).unwrap().builtin_mosaic=true);changed();}
#[cfg(test)]
pub(super) fn mark_test_builtin_blur(id:&str){PROGRAMS.with(|p|p.borrow_mut().get_mut(id).unwrap().builtin_blur=true);changed();}

#[cfg(test)]
pub(super) fn mark_test_builtin_kawase(id:&str){PROGRAMS.with(|p|{let mut p=p.borrow_mut();let p=p.get_mut(id).unwrap();p.builtin_kawase=true;p.translation_kind=3;});changed();}
#[cfg(test)]
pub(super) fn mark_test_translation(id:&str,kind:u32){PROGRAMS.with(|p|{let mut p=p.borrow_mut();let p=p.get_mut(id).unwrap();p.translation_kind=kind;p.builtin_blur=matches!(kind,1|2);p.builtin_gray=kind==6;});changed();}
