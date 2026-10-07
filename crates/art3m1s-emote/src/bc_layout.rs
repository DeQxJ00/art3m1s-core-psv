//! PSV GXM block order. Coordinates are compression blocks, not pixels.
pub fn payload_len(width: u32, height: u32, bc1: bool, swizzled: bool) -> Option<usize> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 { return None; }
    let (w, h) = if swizzled {
        (width.next_power_of_two().max(4), height.next_power_of_two().max(4))
    } else { (width, height) };
    Some(w.div_ceil(4) as usize * h.div_ceil(4) as usize * if bc1 { 8 } else { 16 })
}

pub fn block_index(x: usize, y: usize, bw: usize, bh: usize) -> usize {
    let (mut index, mut shift, mut bit) = (0, 0, 1);
    while bit < bw || bit < bh {
        if bit < bh { index |= usize::from(y & bit != 0) << shift; shift += 1; }
        if bit < bw { index |= usize::from(x & bit != 0) << shift; shift += 1; }
        bit *= 2;
    }
    index
}

pub fn linearize(data: &[u8], width: u32, height: u32, bc1: bool) -> Option<Vec<u8>> {
    if data.len() != payload_len(width, height, bc1, true)? { return None; }
    let bytes = if bc1 { 8 } else { 16 };
    let (sw, sh) = (width.div_ceil(4) as usize, height.div_ceil(4) as usize);
    let (bw, bh) = (sw.next_power_of_two(), sh.next_power_of_two());
    let mut out = vec![0; payload_len(width, height, bc1, false)?];
    for y in 0..sh { for x in 0..sw {
        let from = block_index(x, y, bw, bh) * bytes;
        let to = (y * sw + x) * bytes;
        out[to..to + bytes].copy_from_slice(&data[from..from + bytes]);
    }}
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn rectangular_order_and_padding() {
        // Independent expected indices for a 4x2 block grid: Y occupies bit0.
        assert_eq!((0..2).flat_map(|y|(0..4).map(move |x|block_index(x,y,4,2))).collect::<Vec<_>>(),
            [0,2,4,6,1,3,5,7]);
        assert_eq!((0..4).flat_map(|y|(0..2).map(move |x|block_index(x,y,2,4))).collect::<Vec<_>>(),
            [0,2,1,3,4,6,5,7]);
        for bc1 in [false, true] {
            let bytes=if bc1 {8}else{16};
            let mut data=vec![0;payload_len(12,8,bc1,true).unwrap()];
            for (src,tag) in [(0,1),(2,2),(4,3),(1,4),(3,5),(5,6)] {
                data[src*bytes..(src+1)*bytes].fill(tag);
            }
            let out=linearize(&data,12,8,bc1).unwrap();
            for (i,b) in out.chunks_exact(bytes).enumerate() { assert!(b.iter().all(|v|*v==i as u8+1)); }
            assert!(linearize(&data[..data.len()-1],12,8,bc1).is_none());
        }
        assert_eq!(payload_len(960,544,false,true),Some(1048576));
        assert_eq!(payload_len(960,544,true,true),Some(524288));
        assert_eq!(payload_len(1,1,true,true),Some(8));
        assert_eq!(payload_len(0,1,false,true),None);
        assert_eq!(payload_len(4097,1,false,true),None);
    }
}
