//! Private native-to-native rendering bridge used by the Art3m1s core facade.

use std::ffi::c_void;

#[cfg(any(feature = "native-upstream", feature = "native-upstream-smoke"))]
use crate::protocol::ART3M1S_KRKR_STATUS_OK;

pub const ART3M1S_KRKR_RENDER_HOST_ABI_VERSION: u32 = 1;

pub type RenderBeginFrameFn = unsafe extern "C" fn(*mut c_void, u32, u32) -> i32;
pub type RenderCreateTextureFn = unsafe extern "C" fn(*mut c_void, u32, u32) -> u64;
pub type RenderUpdateTextureFn =
    unsafe extern "C" fn(*mut c_void, u64, *const u8, u32, u32, u32) -> i32;
pub type RenderDestroyTextureFn = unsafe extern "C" fn(*mut c_void, u64);
pub type RenderDrawTextureFn = unsafe extern "C" fn(*mut c_void, u64, f32, f32, f32, f32) -> i32;
pub type RenderEndFrameFn = unsafe extern "C" fn(*mut c_void) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Art3m1sKrkrRenderHostV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub user_data: *mut c_void,
    pub begin_frame: Option<RenderBeginFrameFn>,
    pub create_texture: Option<RenderCreateTextureFn>,
    pub update_texture: Option<RenderUpdateTextureFn>,
    pub destroy_texture: Option<RenderDestroyTextureFn>,
    pub draw_texture: Option<RenderDrawTextureFn>,
    pub end_frame: Option<RenderEndFrameFn>,
    pub reserved: [u64; 4],
}

impl Art3m1sKrkrRenderHostV1 {
    pub fn new(user_data: *mut c_void) -> Self {
        Self {
            struct_size: std::mem::size_of::<Self>() as u32,
            abi_version: ART3M1S_KRKR_RENDER_HOST_ABI_VERSION,
            user_data,
            begin_frame: None,
            create_texture: None,
            update_texture: None,
            destroy_texture: None,
            draw_texture: None,
            end_frame: None,
            reserved: [0; 4],
        }
    }
}

#[cfg(any(feature = "native-upstream", feature = "native-upstream-smoke"))]
unsafe extern "C" {
    fn art3m1s_krkr_native_set_render_host_v1(host: *const Art3m1sKrkrRenderHostV1) -> i32;
}

#[cfg(any(feature = "native-upstream", feature = "native-upstream-smoke"))]
pub fn set_native_render_host(host: Option<&Art3m1sKrkrRenderHostV1>) -> Result<(), i32> {
    let status = unsafe {
        art3m1s_krkr_native_set_render_host_v1(host.map_or(std::ptr::null(), std::ptr::from_ref))
    };
    if status == ART3M1S_KRKR_STATUS_OK {
        Ok(())
    } else {
        Err(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_host_layout_is_versioned() {
        let host = Art3m1sKrkrRenderHostV1::new(std::ptr::dangling_mut());
        assert_eq!(host.struct_size as usize, std::mem::size_of_val(&host));
        assert_eq!(host.abi_version, ART3M1S_KRKR_RENDER_HOST_ABI_VERSION);
        assert!(host.begin_frame.is_none());
    }
}
