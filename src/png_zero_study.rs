//! External demo only: count exact zero pixels while expanding/copying PNG rows.
//! Both const variants use the same row decoder; production decoding is unchanged.
use crate::resource_ledger::Tracked;
use std::{ffi::c_void, io::Cursor};
const LIMIT: usize = 16 * 1024 * 1024;

fn decode<const COUNT: bool>(source: &[u8]) -> Option<(Tracked<image::RgbaImage>, usize)> {
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(source), png::Limits { bytes: LIMIT });
    decoder.set_transformations(png::Transformations::IDENTITY);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    // These are the two formats covered by this experiment. Never silently
    // reinterpret low-bit, 16-bit, interlaced, grayscale, RGB or animated PNG.
    if info.bit_depth != png::BitDepth::Eight || info.interlaced || info.animation_control.is_some() {
        return None;
    }
    let (w, h, color) = (info.width, info.height, info.color_type);
    let bytes = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    if bytes == 0 || bytes > LIMIT { return None; }
    let mut palette = [0u32; 256];
    let palette_len = if color == png::ColorType::Indexed {
        let rgb = info.palette.as_ref()?;
        if rgb.is_empty() || rgb.len() % 3 != 0 || rgb.len() > 768 { return None; }
        let alpha = info.trns.as_deref().unwrap_or(&[]);
        if alpha.len() > rgb.len() / 3 { return None; }
        for (i, pixel) in rgb.chunks_exact(3).enumerate() {
            palette[i] = u32::from_ne_bytes([pixel[0], pixel[1], pixel[2], alpha.get(i).copied().unwrap_or(255)]);
        }
        rgb.len() / 3
    } else if color == png::ColorType::Rgba { 0 } else { return None; };
    let mut output = Vec::new();
    output.try_reserve_exact(bytes).ok()?;
    output.resize(bytes, 0);
    let mut zero_pixels = 0usize;
    for row in output.chunks_exact_mut(w as usize * 4) {
        let input = reader.next_row().ok()??;
        let data = input.data();
        if color == png::ColorType::Indexed {
            if data.len() != w as usize { return None; }
            for (out, &index) in row.chunks_exact_mut(4).zip(data) {
                if index as usize >= palette_len { return None; }
                let rgba = palette[index as usize];
                out.copy_from_slice(&rgba.to_ne_bytes());
                if COUNT { zero_pixels += usize::from(rgba == 0); }
            }
        } else {
            if data.len() != row.len() { return None; }
            if COUNT {
                for (out, input) in row.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
                    let rgba = u32::from_ne_bytes(input.try_into().ok()?);
                    out.copy_from_slice(&rgba.to_ne_bytes());
                    zero_pixels += usize::from(rgba == 0);
                }
            } else { row.copy_from_slice(data); }
        }
    }
    reader.finish().ok()?;
    Some((image::RgbaImage::from_raw(w, h, output)?.into(), zero_pixels))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_png_zero_study_decode(
    source: *const u8, len: usize, count: i32, width: *mut u32, height: *mut u32,
    pixels: *mut *const u8, zeros: *mut u64,
) -> *mut c_void {
    if source.is_null() || width.is_null() || height.is_null() || pixels.is_null()
        || zeros.is_null() || len == 0 || len > LIMIT { return std::ptr::null_mut(); }
    let bytes = unsafe { std::slice::from_raw_parts(source, len) };
    let result = if count != 0 { decode::<true>(bytes) } else { decode::<false>(bytes) };
    let Some((image, count)) = result else { return std::ptr::null_mut(); };
    unsafe { *width = image.width(); *height = image.height(); *pixels = image.as_ptr(); *zeros = count as u64; }
    // Shares the exact handle type/free routine with texture_study.
    Box::into_raw(Box::new(image)).cast()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(bytes: &[u8]) {
        let expected = image::load_from_memory(bytes).unwrap().into_rgba8();
        let count = expected.pixels().filter(|p| p.0 == [0,0,0,0]).count();
        let (plain, ignored) = decode::<false>(bytes).unwrap();
        let (counted, actual) = decode::<true>(bytes).unwrap();
        assert_eq!(plain.data, expected); assert_eq!(counted.data, expected);
        assert_eq!(ignored, 0); assert_eq!(actual, count);
    }
    #[test]
    fn count_preserves_hidden_rgb_palette_alpha_and_odd_rows() {
        let mut indexed = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut indexed, 7, 3);
            encoder.set_color(png::ColorType::Indexed); encoder.set_depth(png::BitDepth::Eight);
            encoder.set_palette(vec![0,0,0, 17,31,49, 0,0,0, 9,8,7]);
            encoder.set_trns(vec![0,0,255]);
            encoder.write_header().unwrap().write_image_data(&(0..21).map(|i|(i%4) as u8).collect::<Vec<_>>()).unwrap();
        }
        check(&indexed);
        let img = image::RgbaImage::from_fn(7,3,|x,y| image::Rgba(match (x+y)%4 {
            0=>[0,0,0,0],1=>[17,31,49,0],2=>[0,0,0,255],_=>[1,2,3,4],
        }));
        let mut bytes=Cursor::new(Vec::new());img.write_to(&mut bytes,image::ImageFormat::Png).unwrap();check(bytes.get_ref());
        assert!(decode::<true>(&indexed[..indexed.len()/2]).is_none());
        assert!(decode::<true>(b"invalid").is_none());
    }
    #[test]
    #[ignore = "requires the external original-file manifest"]
    fn original_file_manifest_matches_production() {
        let path=std::env::var("ART3M1S_PNG_STUDY_MANIFEST").unwrap();
        let manifest:serde_json::Value=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for sample in manifest["samples"].as_array().unwrap() {
            check(&std::fs::read(sample["source"].as_str().unwrap()).unwrap());
        }
    }
}
