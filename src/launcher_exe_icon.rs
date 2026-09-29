//! Bounded PE icon resource reader for the launcher. It never executes the EXE.
use image::RgbaImage;

const MAX_ICON: usize = 2 * 1024 * 1024;
fn u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}
fn u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

struct Section { rva: u32, raw: u32, size: u32 }
struct Pe<'a> { data: &'a [u8], base: usize, sections: Vec<Section> }
impl<'a> Pe<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        if data.get(..2)? != b"MZ" { return None; }
        let pe = usize::try_from(u32le(data, 0x3c)?).ok()?;
        if data.get(pe..pe.checked_add(4)?)? != b"PE\0\0" { return None; }
        let count = usize::from(u16le(data, pe + 6)?);
        if count == 0 || count > 96 { return None; }
        let optional = pe.checked_add(24)?;
        let optional_len = usize::from(u16le(data, pe + 20)?);
        let directory = match u16le(data, optional)? {
            0x10b => 96usize, 0x20b => 112usize, _ => return None,
        };
        let resource_entry = optional.checked_add(directory + 16)?;
        if resource_entry.checked_add(8)? > optional.checked_add(optional_len)? { return None; }
        let resource_rva = u32le(data, resource_entry)?;
        if resource_rva == 0 { return None; }
        let section_table = optional.checked_add(optional_len)?;
        let mut sections = Vec::with_capacity(count);
        for i in 0..count {
            let at = section_table.checked_add(i.checked_mul(40)?)?;
            let rva = u32le(data, at + 12)?;
            let size = u32le(data, at + 16)?;
            let raw = u32le(data, at + 20)?;
            if usize::try_from(raw).ok()?.checked_add(usize::try_from(size).ok()?)? > data.len() {
                return None;
            }
            sections.push(Section { rva, raw, size });
        }
        let mut result = Self { data, base: 0, sections };
        result.base = result.offset(resource_rva, 16)?;
        Some(result)
    }
    fn offset(&self, rva: u32, len: usize) -> Option<usize> {
        let len = u32::try_from(len).ok()?;
        self.sections.iter().find_map(|s| {
            let relative = rva.checked_sub(s.rva)?;
            if relative.checked_add(len)? > s.size { return None; }
            let at = usize::try_from(s.raw.checked_add(relative)?).ok()?;
            self.data.get(at..at.checked_add(usize::try_from(len).ok()?)?)?;
            Some(at)
        })
    }
    fn relative(&self, value: u32, size: usize) -> Option<usize> {
        let at = self.base.checked_add(usize::try_from(value).ok()?)?;
        self.data.get(at..at.checked_add(size)?)?;
        Some(at)
    }
    fn entries(&self, directory: u32) -> Option<Vec<(u32, u32)>> {
        let at = self.relative(directory, 16)?;
        let count = usize::from(u16le(self.data, at + 12)?)
            .checked_add(usize::from(u16le(self.data, at + 14)?))?;
        if count > 256 { return None; }
        let start = self.relative(directory.checked_add(16)?, count.checked_mul(8)?)?;
        (0..count).map(|i| Some((u32le(self.data, start + i * 8)?,
            u32le(self.data, start + i * 8 + 4)?))).collect()
    }
    fn branch(&self, directory: u32, id: u32) -> Option<u32> {
        self.entries(directory)?.into_iter().find_map(|(name, child)| {
            (name == id && child & 0x8000_0000 != 0).then_some(child & 0x7fff_ffff)
        })
    }
    fn blob(&self, leaf: u32) -> Option<&'a [u8]> {
        let at = self.relative(leaf, 16)?;
        let rva = u32le(self.data, at)?;
        let len = usize::try_from(u32le(self.data, at + 4)?).ok()?;
        if len == 0 || len > MAX_ICON { return None; }
        let at = self.offset(rva, len)?;
        self.data.get(at..at.checked_add(len)?)
    }
    fn first_language_blob(&self, directory: u32) -> Option<&'a [u8]> {
        for (_, leaf) in self.entries(directory)? {
            if leaf & 0x8000_0000 == 0 {
                if let Some(bytes) = self.blob(leaf) { return Some(bytes); }
            }
        }
        None
    }
    fn icon(&self, icon_type: u32, id: u32) -> Option<&'a [u8]> {
        self.first_language_blob(self.branch(icon_type, id)?)
    }
}

fn dib_icon(data: &[u8], group_width: u32, group_height: u32) -> Option<RgbaImage> {
    let header = usize::try_from(u32le(data, 0)?).ok()?;
    if header < 40 || header > data.len() { return None; }
    let width = u32le(data, 4)?;
    let height = u32le(data, 8)?;
    let bpp = u16le(data, 14)?;
    if width == 0 || width > 1024 || height != group_height.checked_mul(2)?
        || width != group_width || !matches!(bpp, 24 | 32) || u32le(data, 16)? != 0 {
        return None;
    }
    let stride = usize::try_from(((width.checked_mul(u32::from(bpp))? + 31) / 32) * 4).ok()?;
    let rows = usize::try_from(group_height).ok()?;
    let xor_len = stride.checked_mul(rows)?;
    let pixels = data.get(header..header.checked_add(xor_len)?)?;
    let mask_stride = usize::try_from(((width + 31) / 32) * 4).ok()?;
    let mask_len = mask_stride.checked_mul(rows)?;
    let mask = data.get(header + xor_len..header.checked_add(xor_len)?.checked_add(mask_len)?);
    let all_alpha_zero = bpp == 32 && pixels.chunks_exact(4).all(|p| p[3] == 0);
    let mut output = vec![0; usize::try_from(width.checked_mul(group_height)?.checked_mul(4)?).ok()?];
    for y in 0..rows {
        let src_y = rows - 1 - y;
        for x in 0..width as usize {
            let src = src_y * stride + x * usize::from(bpp / 8);
            let dst = (y * width as usize + x) * 4;
            output[dst] = pixels[src + 2];output[dst + 1] = pixels[src + 1];
            output[dst + 2] = pixels[src];
            output[dst + 3] = if bpp == 32 && !all_alpha_zero { pixels[src + 3] } else { 255 };
            if let Some(mask) = mask {
                if mask[src_y * mask_stride + x / 8] & (0x80 >> (x % 8)) != 0 {
                    output[dst + 3] = 0;
                }
            }
        }
    }
    RgbaImage::from_raw(width, group_height, output)
}

pub(crate) fn extract(data: &[u8], output: &mut [u8]) -> bool {
    if data.len() > 32 * 1024 * 1024 || output.len() != crate::image_decode::LAUNCHER_ICON_SIDE.pow(2) * 4 {
        return false;
    }
    let Some(pe) = Pe::new(data) else { return false; };
    let (Some(groups), Some(icons)) = (pe.branch(0, 14), pe.branch(0, 3)) else { return false; };
    let Some(group_entries) = pe.entries(groups) else { return false; };
    let mut candidates = Vec::new();
    for (_, group_dir) in group_entries.into_iter().take(32) {
        if group_dir & 0x8000_0000 == 0 { continue; }
        let Some(group) = pe.first_language_blob(group_dir & 0x7fff_ffff) else { continue; };
        if u16le(group, 0) != Some(0) || u16le(group, 2) != Some(1) { continue; }
        let count = usize::from(u16le(group, 4).unwrap_or(0)).min(64);
        for i in 0..count {
            let at = 6 + i * 14;
            let (Some(&w), Some(&h), Some(id)) = (group.get(at), group.get(at + 1), u16le(group, at + 12)) else { continue; };
            let width = if w == 0 { 256 } else { u32::from(w) };
            let height = if h == 0 { 256 } else { u32::from(h) };
            let extent = width.max(height);
            let distance = if extent >= 48 { extent - 48 } else { 48 - extent + 256 };
            let depth = u16le(group, at + 6).unwrap_or(0);
            candidates.push((distance, std::cmp::Reverse(depth), id, width, height));
        }
    }
    candidates.sort_unstable();
    for (_, _, id, width, height) in candidates {
        let Some(blob) = pe.icon(icons, u32::from(id)) else { continue; };
        if blob.starts_with(b"\x89PNG\r\n\x1a\n") {
            if crate::image_decode::launcher_icon(blob, output) { return true; }
        } else if let Some(rgba) = dib_icon(blob, width, height) {
            if crate::image_decode::launcher_rgba_icon(&rgba, output) { return true; }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dib_bgra_and_and_mask_have_expected_alpha() {
        let mut dib=vec![0;40 + 2*4 + 4];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&2u32.to_le_bytes());
        dib[8..12].copy_from_slice(&2u32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib[40..48].copy_from_slice(&[0,0,255,0,0,255,0,0]);
        dib[48]=0x40;
        let image=dib_icon(&dib,2,1).unwrap();
        assert_eq!(image.get_pixel(0,0).0,[255,0,0,255]);
        assert_eq!(image.get_pixel(1,0).0,[0,255,0,0]);
        assert!(dib_icon(&dib[..45],2,1).is_none());
    }
    #[test]
    fn malformed_pe_is_rejected() {
        let mut output=[0;48*48*4];
        assert!(!extract(b"MZ",&mut output));
        assert!(!extract(&vec![0;32*1024*1024+1],&mut output));
    }
    #[test]
    fn optional_real_pe_icon_is_nonempty() {
        let Some(path)=std::env::var_os("ART3M1S_LAUNCHER_EXE_TEST") else { return; };
        let bytes=std::fs::read(path).unwrap();
        let mut output=[0;48*48*4];
        assert!(extract(&bytes,&mut output));
        assert!(output.chunks_exact(4).any(|pixel|pixel[3]!=0));
        if let Some(path)=std::env::var_os("ART3M1S_LAUNCHER_EXE_ICON_OUT") {
            image::save_buffer(path,&output,48,48,image::ColorType::Rgba8).unwrap();
        }
    }
}
