//! Opt-in demo only. Never changes the live READY/IDLE cache policy.
//! All codecs preserve even the RGB components of fully transparent pixels.
use std::{ffi::c_void, io::Write};
const LIMIT: usize = 16 * 1024 * 1024;

// Allocation-free selection probe. u64 ORs reduce per-byte branches in large
// empty regions; an early first-word check keeps nonempty cells cheap.
fn zero_cell(bytes: &[u8]) -> bool {
    let mut words=bytes.chunks_exact(8);
    let mut combined=0u64;
    if let Some(first)=words.next() {
        if u64::from_le_bytes(first.try_into().unwrap())!=0 { return false; }
    }
    for word in words.by_ref() { combined |= u64::from_le_bytes(word.try_into().unwrap()); }
    combined==0 && words.remainder().iter().all(|&b|b==0)
}
fn zero_span_stats(src: &[u8], gated: bool) -> [u64;4] {
    if gated && src.len()<512*1024 { return [0,0,src.len() as u64,0]; }
    let(mut zeros,mut runs,mut estimated)=(0usize,0usize,0usize);
    for block in src.chunks(128*1024) {
        let(mut records,mut literal,mut was_zero)=(0usize,0usize,false);
        for (i,cell) in block.chunks(64).enumerate() {
            let empty=zero_cell(cell);
            if empty {
                zeros+=cell.len();
                if !was_zero { runs+=1;records+=1; }
            } else { literal+=cell.len();if i==0 {records+=1;} }
            was_zero=empty;
        }
        estimated+=(12+records*8+literal).min(12+block.len());
    }
    let eligible=src.len()>=512*1024 && zeros*2>=src.len() && runs>0
        && zeros>=runs*256 && estimated*4<=src.len()*3;
    [zeros as u64,runs as u64,estimated as u64,u64::from(eligible)]
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_cache_study_select(src:*const u8,len:usize,gated:i32,out:*mut u64)->i32 {
    if src.is_null()||out.is_null()||len==0||len>LIMIT||len%4!=0 {return 0;}
    let stats=zero_span_stats(unsafe{std::slice::from_raw_parts(src,len)},gated!=0);
    unsafe{std::ptr::copy_nonoverlapping(stats.as_ptr(),out,4);}
    1
}

// ZeroSpan32 v1: exact zero RGBA gaps and bulk literal spans. A 64-byte
// classification unit absorbs short zero runs into literals to reduce dispatch.
// Header: "ZSP1", raw byte length, representation (0=raw, 1=spans).
// Spans: zero byte length, literal byte length, literal bytes; all multiples of 4.
// Hidden RGB at alpha=0 is literal data, never discarded. GPU writers can emit
// each gap with memset and each literal with memcpy, without reading output.
fn encode_zero_spans(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"ZSP1");
    out.extend_from_slice(&(src.len() as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    let mut at = 0;
    let zero = |p: usize| src[p..(p + 64).min(src.len())].iter().all(|&b| b == 0);
    while at < src.len() {
        let start = at;
        while at < src.len() && zero(at) { at = (at + 64).min(src.len()); }
        let literal = at;
        while at < src.len() && !zero(at) { at = (at + 64).min(src.len()); }
        out.extend_from_slice(&((literal - start) as u32).to_le_bytes());
        out.extend_from_slice(&((at - literal) as u32).to_le_bytes());
        out.extend_from_slice(&src[literal..at]);
    }
    if out.len() >= src.len() + 12 {
        out.truncate(12);
        out[8..12].copy_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(src);
    }
    out
}

fn decode_zero_spans(src: &[u8], dst: &mut [u8]) -> bool {
    if src.len() < 12 || &src[..4] != b"ZSP1" { return false; }
    let word = |p: usize| u32::from_le_bytes(src[p..p+4].try_into().unwrap()) as usize;
    if word(4) != dst.len() { return false; }
    match word(8) {
        0 => {
            if src.len() - 12 != dst.len() { return false; }
            dst.copy_from_slice(&src[12..]);
            true
        }
        1 => {
            let (mut s, mut d) = (12, 0);
            while d < dst.len() {
                if src.len() - s < 8 { return false; }
                let zeros = word(s); let literals = word(s+4); s += 8;
                if zeros % 4 != 0 || literals % 4 != 0 || (zeros == 0 && literals == 0)
                    || zeros > dst.len()-d || literals > dst.len()-d-zeros
                    || literals > src.len()-s { return false; }
                dst[d..d+zeros].fill(0); d += zeros;
                dst[d..d+literals].copy_from_slice(&src[s..s+literals]);
                d += literals; s += literals;
            }
            s == src.len()
        }
        _ => false,
    }
}

fn encode(mode: u32, src: &[u8]) -> Option<Vec<u8>> {
    if src.is_empty() || src.len() > LIMIT || src.len() % 4 != 0 { return None; }
    match mode {
        0 => Some(src.to_vec()),
        1 => {
            let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(src).ok()?;
            encoder.finish().ok()
        }
        2 => Some(lz4_flex::block::compress(src)),
        // Byte LZW, LSB bit order, 8-bit alphabet; independent dictionary per block.
        6 => weezl::encode::Encoder::new(weezl::BitOrder::Lsb, 8).encode(src).ok(),
        7 => Some(encode_zero_spans(src)),
        3 => {
            // Packet RLE32: LE count, high bit = repeated RGBA pixel; otherwise
            // count literal pixels. Literal packets avoid exploding noisy images.
            let mut out = Vec::new();
            let pixels = src.len()/4;
            let same = |a: usize, b: usize| src[a*4..a*4+4] == src[b*4..b*4+4];
            let mut p = 0;
            while p < pixels {
                let start = p;
                if p+1 < pixels && same(p,p+1) {
                    p += 2;
                    while p < pixels && same(start,p) { p += 1; }
                    out.extend_from_slice(&((p-start) as u32 | 0x8000_0000).to_le_bytes());
                    out.extend_from_slice(&src[start*4..start*4+4]);
                } else {
                    p += 1;
                    while p < pixels && !(p+1 < pixels && same(p,p+1)) { p += 1; }
                    out.extend_from_slice(&((p-start) as u32).to_le_bytes());
                    out.extend_from_slice(&src[start*4..p*4]);
                }
            }
            Some(out)
        }
        _ => None,
    }
}

fn decode(mode: u32, src: &[u8], dst: &mut [u8]) -> bool {
    if dst.is_empty() || dst.len() > LIMIT || dst.len()%4 != 0 { return false; }
    match mode {
        0 => { if src.len()!=dst.len() {return false;} dst.copy_from_slice(src); true }
        1 => {
            let mut decoder = flate2::Decompress::new(true);
            matches!(decoder.decompress(src,dst,flate2::FlushDecompress::Finish),Ok(flate2::Status::StreamEnd))
                && decoder.total_out()==dst.len() as u64 && decoder.total_in()==src.len() as u64
        }
        2 => lz4_flex::block::decompress_into(src,dst).ok()==Some(dst.len()),
        6 => {
            let mut decoder=weezl::decode::Decoder::new(weezl::BitOrder::Lsb,8);
            let (mut input,mut output)=(0,0);
            loop {
                let r=decoder.decode_bytes(&src[input..],&mut dst[output..]);
                input+=r.consumed_in;output+=r.consumed_out;
                match r.status {
                    Ok(weezl::LzwStatus::Done) => return input==src.len() && output==dst.len(),
                    Ok(weezl::LzwStatus::Ok) if r.consumed_in!=0 || r.consumed_out!=0 => {},
                    _ => return false,
                }
            }
        }
        7 => decode_zero_spans(src, dst),
        3 => {
            let (mut s,mut d)=(0,0);
            while s<src.len() {
                let Some(header)=src.get(s..s+4) else {return false;};
                let word=u32::from_le_bytes(header.try_into().unwrap()); s+=4;
                let count=(word&0x7fff_ffff) as usize;
                if count==0 || count>(dst.len()-d)/4 {return false;}
                let n=count*4;
                if word&0x8000_0000!=0 {
                    let Some(pixel)=src.get(s..s+4) else {return false;}; s+=4;
                    for chunk in dst[d..d+n].chunks_exact_mut(4) { chunk.copy_from_slice(pixel); }
                } else {
                    let Some(bytes)=src.get(s..s+n) else {return false;}; s+=n;
                    dst[d..d+n].copy_from_slice(bytes);
                }
                d+=n;
            }
            d==dst.len()
        }
        _ => false,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_cache_study_encode(mode:u32,src:*const u8,len:usize,data:*mut *const u8,size:*mut usize)->*mut c_void {
    if src.is_null() || data.is_null() || size.is_null() || len==0 || len>LIMIT {return std::ptr::null_mut();}
    let Some(bytes)=encode(mode,unsafe{std::slice::from_raw_parts(src,len)}) else {return std::ptr::null_mut();};
    // Return exact resident payload, not a geometrically overallocated Vec.
    let bytes=bytes.into_boxed_slice();
    unsafe{*data=bytes.as_ptr();*size=bytes.len();}
    Box::into_raw(Box::new(bytes)).cast()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_cache_study_decode(mode:u32,src:*const u8,len:usize,dst:*mut u8,size:usize)->i32 {
    if src.is_null() || dst.is_null() || len==0 || len>LIMIT*2 || size==0 || size>LIMIT {return 0;}
    // Caller supplies distinct source and destination allocations.
    i32::from(decode(mode,unsafe{std::slice::from_raw_parts(src,len)},unsafe{std::slice::from_raw_parts_mut(dst,size)}))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_cache_study_free(handle:*mut c_void) {
    if !handle.is_null() {drop(unsafe{Box::from_raw(handle.cast::<Box<[u8]>>())});}
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn exact_roundtrip_including_hidden_rgb_and_literal_runs() {
        let mut mixed=Vec::new();
        for i in 0..65536u32 {
            let pixel=if i%513<300 { [13,29,71,0] } else { i.to_le_bytes() };
            mixed.extend_from_slice(&pixel);
        }
        for src in [vec![0;4],vec![255;16384],mixed,(0..4096u32).flat_map(|i|i.wrapping_mul(2654435761).to_le_bytes()).collect()] {
            for mode in [0,1,2,3,6,7] {
                let compressed=encode(mode,&src).unwrap();let mut out=vec![0;src.len()];
                assert!(decode(mode,&compressed,&mut out));assert_eq!(src,out);
                assert!(!decode(mode,&compressed[..compressed.len()-1],&mut out));
                assert!(!decode(mode,&compressed,&mut vec![0;src.len()+4]));
            }
        }
    }
    #[test] fn invalid_packets_and_ffi_lifecycle() {
        for packet in [&[0,0,0,0][..],&[255,255,255,255,0,0,0,0],&[1,0,0,128],&[1,0,0,0,1,2,3]] {
            assert!(!decode(3,packet,&mut [0;16]));
        }
        assert!(encode(4,&[0;4]).is_none());assert!(encode(0,&[0;3]).is_none());
        for mode in [0,1,2,3,6,7] {unsafe {
            let src=[11,22,33,0,11,22,33,0];let(mut p,mut n)=(std::ptr::null(),0);
            let handle=art3m1s_cache_study_encode(mode,src.as_ptr(),src.len(),&mut p,&mut n);
            assert!(!handle.is_null());let mut out=[0;8];
            assert_eq!(art3m1s_cache_study_decode(mode,p,n,out.as_mut_ptr(),out.len()),1);
            assert_eq!(src,out);art3m1s_cache_study_free(handle);
        }}
    }
    #[test] fn zero_spans_edges_hidden_rgb_and_malformed_packets() {
        for len in [4,60,64,68,128,132,1024,131072] {
            let mut src=vec![0;len];
            for i in (0..len/4).step_by(67) { src[i*4..i*4+4].copy_from_slice(&[17,23,91,0]); }
            let packed=encode(7,&src).unwrap();let mut dst=vec![0xff;len];
            assert!(decode(7,&packed,&mut dst));assert_eq!(src,dst);
            let mut extra=packed.clone();extra.push(0);assert!(!decode(7,&extra,&mut dst));
            for cut in 0..packed.len().min(80) {assert!(!decode(7,&packed[..cut],&mut dst));}
        }
        let src=vec![0;1024];let p=encode(7,&src).unwrap();assert_eq!(p.len(),20);
        for (offset,value) in [(4,1020u32),(8,2),(12,u32::MAX),(12,3),(16,u32::MAX),(16,4)] {
            let mut bad=p.clone();bad[offset..offset+4].copy_from_slice(&value.to_le_bytes());
            assert!(!decode(7,&bad,&mut vec![0;1024]));
        }
        let mut empty=p;empty[12..20].fill(0);assert!(!decode(7,&empty,&mut vec![0;1024]));
    }
    #[test] fn zero_span_selection_predicts_payload_and_preserves_gate() {
        for len in [4,60,64,68,132,131068,131072,131076,524284,524288,524292] {
            let mut src=vec![0;len];
            for i in (0..len/4).step_by(67) {src[i*4..i*4+4].copy_from_slice(&[17,23,91,0]);}
            for data in [src,vec![0;len],vec![1;len]] {
                let exact:usize=data.chunks(128*1024).map(|b|encode_zero_spans(b).len()).sum();
                let stats=zero_span_stats(&data,false);assert_eq!(stats[2],exact as u64);
                let gated=zero_span_stats(&data,true);
                if len<512*1024 {assert_eq!(gated,[0,0,len as u64,0]);}
                else {assert_eq!(gated,stats);}
            }
        }
        assert_eq!(zero_span_stats(&vec![0;512*1024],true)[3],1);
        assert_eq!(zero_span_stats(&vec![0;512*1024-4],true)[3],0);
    }
}
