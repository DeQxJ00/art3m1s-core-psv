//! Fallible large image output allocation shared by the Direct foreground and prefetch paths.
//! This does not replace codec-internal allocators or provide a process-wide memory limit.
use image::{ColorType, ImageDecoder, ImageError, ImageResult, RgbaImage};

pub(crate) const LAUNCHER_ICON_SIDE: usize = 48;

pub(crate) fn launcher_icon(png: &[u8], output: &mut [u8]) -> bool {
    if output.len() != LAUNCHER_ICON_SIDE * LAUNCHER_ICON_SIDE * 4
        || png.len() > 2 * 1024 * 1024
        || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return false;
    }
    let Ok(decoder) = image::codecs::png::PngDecoder::new(std::io::Cursor::new(png)) else {
        return false;
    };
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || width > 2048 || height > 2048 {
        return false;
    }
    let Ok(source) = rgba(decoder, 16 * 1024 * 1024) else {
        return false;
    };
    launcher_rgba_icon(&source, output)
}

pub(crate) fn launcher_rgba_icon(source: &RgbaImage, output: &mut [u8]) -> bool {
    if output.len() != LAUNCHER_ICON_SIDE * LAUNCHER_ICON_SIDE * 4 {
        return false;
    }
    let (width, height) = source.dimensions();
    if width == 0 || height == 0 || width > 2048 || height > 2048 {
        return false;
    }
    let scale = (LAUNCHER_ICON_SIDE as f64 / width as f64)
        .min(LAUNCHER_ICON_SIDE as f64 / height as f64);
    let w = (width as f64 * scale).round().max(1.0) as u32;
    let h = (height as f64 * scale).round().max(1.0) as u32;
    let thumb = image::imageops::resize(source, w, h, image::imageops::FilterType::Triangle);
    output.fill(0);
    let x = (LAUNCHER_ICON_SIDE - w as usize) / 2;
    let y = (LAUNCHER_ICON_SIDE - h as usize) / 2;
    for row in 0..h as usize {
        let dst = ((y + row) * LAUNCHER_ICON_SIDE + x) * 4;
        let src = row * w as usize * 4;
        output[dst..dst + w as usize * 4].copy_from_slice(&thumb.as_raw()[src..src + w as usize * 4]);
    }
    true
}

fn memory_error() -> ImageError {
    ImageError::Limits(image::error::LimitError::from_kind(image::error::LimitErrorKind::InsufficientMemory))
}

pub(crate) fn rgba(decoder: impl ImageDecoder, limit: usize) -> ImageResult<RgbaImage> {
    rgba_with_reserve(decoder,limit,|data,size|data.try_reserve_exact(size).is_ok())
}

pub(crate) fn rgba_with_reserve(decoder: impl ImageDecoder, limit: usize,
    reserve: impl FnOnce(&mut Vec<u8>,usize)->bool) -> ImageResult<RgbaImage> {
    let (width,height)=decoder.dimensions();
    let output=(width as usize).checked_mul(height as usize).and_then(|n|n.checked_mul(4)).ok_or_else(memory_error)?;
    let storage=usize::try_from(decoder.total_bytes()).map_err(|_|memory_error())?.max(output);
    if storage>limit{return Err(memory_error());}
    let mut data=Vec::new();
    if !reserve(&mut data,storage) || data.capacity()<storage {return Err(memory_error());}
    data.resize(storage,0);
    rgba_into(decoder,&mut data,limit)?;
    data.truncate(output);
    RgbaImage::from_raw(width,height,data).ok_or_else(memory_error)
}

// The destination may be a private mapped surface. No ownership/deallocation
// is assumed; caller publishes only after this returns success.
pub(crate) fn rgba_into(decoder: impl ImageDecoder, data:&mut [u8], limit:usize)->ImageResult<()> {
    let (width,height)=decoder.dimensions();
    let color=decoder.color_type();
    let (channels,depth)=match color {
        ColorType::L8=>(1,1), ColorType::La8=>(2,1), ColorType::Rgb8=>(3,1), ColorType::Rgba8=>(4,1),
        ColorType::L16=>(1,2), ColorType::La16=>(2,2), ColorType::Rgb16=>(3,2), ColorType::Rgba16=>(4,2),
        _=>return Err(ImageError::Unsupported(image::error::UnsupportedError::from_format_and_kind(
            image::error::ImageFormatHint::Unknown,image::error::UnsupportedErrorKind::Color(color.into())))),
    };
    let pixels=(width as usize).checked_mul(height as usize).ok_or_else(memory_error)?;
    let output=pixels.checked_mul(4).ok_or_else(memory_error)?;
    let input=pixels.checked_mul(channels*depth).ok_or_else(memory_error)?;
    let storage=input.max(output);
    if storage>limit || data.len()<storage || decoder.total_bytes()!=input as u64 {return Err(memory_error());}
    decoder.read_image(&mut data[..input])?;
    let convert=|data:&mut [u8],i:usize| {
        let source=i*channels*depth;
        let read=|channel:usize| {
            let offset=source+channel*depth;
            if depth==1 {data[offset]} else {
                // ImageDecoder returns 16-bit channels in native endianness.
                ((u16::from_ne_bytes([data[offset],data[offset+1]]) as u32+128)/257) as u8
            }
        };
        let pixel=match channels {
            1=>{let l=read(0);[l,l,l,255]},
            2=>{let l=read(0);[l,l,l,read(1)]},
            3=>[read(0),read(1),read(2),255],
            _=>[read(0),read(1),read(2),read(3)],
        };
        data[i*4..i*4+4].copy_from_slice(&pixel);
    };
    if color!=ColorType::Rgba8 {
        if channels*depth<4 {for i in (0..pixels).rev(){convert(data,i);}}
        else {for i in 0..pixels {convert(data,i);}}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    #[test]
    fn launcher_icon_centers_png_and_preserves_alpha() {
        let mut png = Vec::new();
        let pixels = [255, 0, 0, 128, 0, 255, 0, 255];
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, 2, 1, ColorType::Rgba8.into()).unwrap();
        let mut output = [0x55; LAUNCHER_ICON_SIDE * LAUNCHER_ICON_SIDE * 4];
        assert!(launcher_icon(&png, &mut output));
        assert_eq!(&output[..4], &[0, 0, 0, 0]);
        assert!(output.chunks_exact(4).any(|p| p[3] != 0));
        assert!(!launcher_icon(&png[..8], &mut output));
    }
    #[test]
    fn png_depths_match_reference_and_allocation_rejection_is_recoverable() {
        for color in [ColorType::L8,ColorType::La8,ColorType::Rgb8,ColorType::Rgba8,
            ColorType::L16,ColorType::La16,ColorType::Rgb16,ColorType::Rgba16] {
            let raw:Vec<u8>=(0..7*3*color.bytes_per_pixel() as usize).map(|i|(i*37) as u8).collect();
            let mut png=Vec::new();
            image::codecs::png::PngEncoder::new(&mut png).write_image(&raw,7,3,color.into()).unwrap();
            let decoder=||image::ImageReader::new(std::io::Cursor::new(&png)).with_guessed_format().unwrap().into_decoder().unwrap();
            let expected=image::load_from_memory(&png).unwrap().into_rgba8();
            assert_eq!(rgba(decoder(),4096).unwrap(),expected,"{color:?}");
            assert!(rgba_with_reserve(decoder(),4096,|_,_|false).is_err());
            assert!(rgba(decoder(),1).is_err());
        }
    }
    #[test]
    fn jpeg_matches_reference() {
        let raw:Vec<u8>=(0..19*9*3).map(|i|(i*53) as u8).collect();
        let mut jpeg=Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg).write_image(&raw,19,9,ColorType::Rgb8.into()).unwrap();
        let decoder=image::ImageReader::new(std::io::Cursor::new(&jpeg)).with_guessed_format().unwrap().into_decoder().unwrap();
        assert_eq!(rgba(decoder,8192).unwrap(),image::load_from_memory(&jpeg).unwrap().into_rgba8());
    }
    #[test]
    fn pal8_expands_every_index_and_partial_trns_defaults_to_opaque() {
        let alpha=[0,1,127,128,254,255,17,32,64,96,160,192,224,240,250,253,0];
        for (png,transparent) in [
            (include_bytes!("image_decode_testdata/pal8-opaque.png").as_slice(),false),
            (include_bytes!("image_decode_testdata/pal8-trns.png").as_slice(),true),
        ] {
            let decoder=||image::ImageReader::new(std::io::Cursor::new(png)).with_guessed_format().unwrap().into_decoder().unwrap();
            let actual=rgba(decoder(),8192).unwrap();
            assert_eq!(actual.dimensions(),(19,17));
            for (i,pixel) in actual.pixels().enumerate() {
                let index=i%256;
                let expected_alpha=if transparent {alpha.get(index).copied().unwrap_or(255)}else{255};
                assert_eq!(pixel.0,[(index*37) as u8,(index*71+3) as u8,(255-index) as u8,expected_alpha],"index={index} trns={transparent}");
            }
            assert_eq!(actual,image::load_from_memory(png).unwrap().into_rgba8());
            assert!(rgba_with_reserve(decoder(),8192,|_,_|false).is_err());
        }
    }
    #[test]
    fn rgba32_preserves_transparent_rgb_and_alpha_edges_at_odd_width() {
        let alpha=[0,1,127,128,254,255];
        let raw:Vec<u8>=(0..19*7).flat_map(|i|[(i*37) as u8,(255-i%256) as u8,(i*71) as u8,alpha[i%alpha.len()]]).collect();
        let mut png=Vec::new();
        image::codecs::png::PngEncoder::new(&mut png).write_image(&raw,19,7,ColorType::Rgba8.into()).unwrap();
        let decoder=image::ImageReader::new(std::io::Cursor::new(&png)).with_guessed_format().unwrap().into_decoder().unwrap();
        assert_eq!(rgba(decoder,8192).unwrap().as_raw(),&raw);
    }
    #[test]
    fn external_surface_decode_matches_pal8_rgba32_and_preserves_guards(){
        let mut samples=vec![include_bytes!("image_decode_testdata/pal8-opaque.png").to_vec(),
            include_bytes!("image_decode_testdata/pal8-trns.png").to_vec()];
        let raw:Vec<u8>=(0..19*7).flat_map(|i|[(i*37) as u8,(i*7) as u8,17,(i*19) as u8]).collect();
        let mut png=Vec::new();image::codecs::png::PngEncoder::new(&mut png).write_image(&raw,19,7,ColorType::Rgba8.into()).unwrap();samples.push(png);
        for bytes in samples{
            let decoder=||image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().unwrap().into_decoder().unwrap();
            let expected=image::load_from_memory(&bytes).unwrap().into_rgba8();let size=expected.as_raw().len();
            let mut guarded=vec![0xa5;size+32];rgba_into(decoder(),&mut guarded[16..size+16],8192).unwrap();
            assert_eq!(&guarded[16..size+16],expected.as_raw());assert!(guarded[..16].iter().chain(&guarded[size+16..]).all(|&x|x==0xa5));
            assert!(rgba_into(decoder(),&mut guarded[16..size+15],8192).is_err());
        }
    }
}
