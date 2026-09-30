//! Opt-in external texture-format study. Uses the production PNG decoder.
use crate::resource_ledger::Tracked;
use std::{ffi::c_void,io::Cursor};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_texture_study_decode(
    source:*const u8,len:usize,width:*mut u32,height:*mut u32,pixels:*mut *const u8,
)->*mut c_void{
    if source.is_null()||width.is_null()||height.is_null()||pixels.is_null()||len==0||len>16*1024*1024{return std::ptr::null_mut();}
    let result=(||{
        let bytes=unsafe{std::slice::from_raw_parts(source,len)};
        let decoder=image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
        crate::resource_ledger::decode_rgba(decoder,16*1024*1024).ok()
    })();
    let Some(image)=result else{return std::ptr::null_mut();};
    unsafe{*width=image.width();*height=image.height();*pixels=image.as_ptr();}
    Box::into_raw(Box::new(image)).cast()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_texture_study_free(handle:*mut c_void){
    if !handle.is_null(){drop(unsafe{Box::from_raw(handle.cast::<Tracked<image::RgbaImage>>())});}
}
#[cfg(test)] mod tests{
    use super::*;
    #[test] fn probe_decoder_preserves_channels_and_rejects_invalid_input(){
        let image=image::RgbaImage::from_fn(17,9,|x,y|image::Rgba([x as u8,y as u8,91,if x==0{0}else{139}]));
        let mut png=Cursor::new(Vec::new());image.write_to(&mut png,image::ImageFormat::Png).unwrap();
        let bytes=png.into_inner();let(mut w,mut h)=(0,0);let mut p=std::ptr::null();
        unsafe{
            let handle=art3m1s_texture_study_decode(bytes.as_ptr(),bytes.len(),&mut w,&mut h,&mut p);
            assert!(!handle.is_null());assert_eq!((w,h),(17,9));
            assert_eq!(std::slice::from_raw_parts(p,17*9*4),image.as_raw());art3m1s_texture_study_free(handle);
            assert!(art3m1s_texture_study_decode(bytes.as_ptr(),3,&mut w,&mut h,&mut p).is_null());
            assert!(art3m1s_texture_study_decode(std::ptr::null(),0,&mut w,&mut h,&mut p).is_null());
        }
    }
}
