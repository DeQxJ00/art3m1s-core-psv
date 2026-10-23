//! Software fallback for row-major PSB ASTC_8BPP (4x4, 16-byte blocks).
use crate::{EmoteError, Result};

pub fn validate(data: &[u8], width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(EmoteError::InvalidFormat("ASTC dimensions outside 1..4096".into()));
    }
    let expected = width.div_ceil(4) as usize * height.div_ceil(4) as usize * 16;
    if data.len() != expected {
        return Err(EmoteError::InvalidFormat(format!("ASTC payload has {} bytes, expected {expected}", data.len())));
    }
    Ok(())
}

/// Output words have RGBA byte order in memory. Callers own the buffer.
pub fn decode_into(data: &[u8], width: u32, height: u32, output: &mut [u32]) -> Result<()> {
    validate(data, width, height)?;
    if output.len() != width as usize * height as usize {
        return Err(EmoteError::InvalidFormat("ASTC output size mismatch".into()));
    }
    texture2ddecoder::decode_astc(data, width as usize, height as usize, 4, 4, output)
        .map_err(|e| EmoteError::InvalidFormat(e.into()))?;
    for pixel in output {
        let [b, g, r, a] = pixel.to_ne_bytes();
        *pixel = u32::from_ne_bytes([r, g, b, a]);
    }
    Ok(())
}

pub fn decode_rgba8(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    validate(data, width, height)?;
    let mut pixels = Vec::new();
    let count = width as usize * height as usize;
    pixels.try_reserve_exact(count).map_err(|_| EmoteError::InvalidFormat("ASTC allocation failed".into()))?;
    pixels.resize(count, 0);
    decode_into(data, width, height, &mut pixels)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(count * 4).map_err(|_| EmoteError::InvalidFormat("ASTC allocation failed".into()))?;
    for pixel in pixels { bytes.extend_from_slice(&pixel.to_ne_bytes()); }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn constant(rgba: [u8; 4]) -> [u8; 16] {
        let mut block = [0xff; 16];
        block[0] = 0xfc; block[1] = 0xfd;
        for (i, value) in rgba.into_iter().enumerate() {
            block[8+i*2..10+i*2].copy_from_slice(&(u16::from(value)*257).to_le_bytes());
        }
        block
    }
    #[test]
    fn colors_alpha_and_partial_blocks() {
        let colors = [[240, 30, 10, 255], [10, 80, 200, 128], [40, 50, 60, 0], [1, 2, 3, 64]];
        let data: Vec<u8> = colors.into_iter().flat_map(constant).collect();
        let rgba = decode_rgba8(&data, 5, 7).unwrap();
        for y in 0..7 { for x in 0..5 {
            assert_eq!(&rgba[(y*5+x)*4..(y*5+x+1)*4], &colors[(y/4)*2+x/4]);
        }}
    }
    #[test]
    fn rejects_bad_lengths_and_dimensions_before_allocating() {
        assert!(decode_rgba8(&[0;15],4,4).is_err());
        assert!(decode_rgba8(&[0;17],4,4).is_err());
        for (w,h) in [(0,4),(4,0),(4097,1),(1,4097),(u32::MAX,u32::MAX)] {
            assert!(decode_rgba8(&[],w,h).is_err());
        }
        assert!(decode_into(&constant([0;4]),4,4,&mut [0;15]).is_err());
    }
}
