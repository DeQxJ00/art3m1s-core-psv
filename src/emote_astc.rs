/// Decode into caller-owned, aligned RGBA storage; no retained pointers.
/// The host supplies disjoint readable input and writable output allocations.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_emote_decode_astc_4x4(
    data: *const u8, bytes: u32, width: u32, height: u32,
    output: *mut u32, output_bytes: u32,
) -> i32 {
    if data.is_null() || output.is_null() || (output as usize) % 4 != 0
        || width == 0 || height == 0 || width > 4096 || height > 4096
        || output_bytes != width * height * 4
        || bytes != width.div_ceil(4) * height.div_ceil(4) * 16 { return 0; }
    let input = unsafe { std::slice::from_raw_parts(data, bytes as usize) };
    let output = unsafe { std::slice::from_raw_parts_mut(output, (output_bytes/4) as usize) };
    i32::from(art3m1s_emote::astc::decode_into(input,width,height,output).is_ok())
}
