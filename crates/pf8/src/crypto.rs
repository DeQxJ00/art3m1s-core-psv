//! Cryptographic operations for PF8 files.

use crate::format;
use sha1::{Digest, Sha1};

/// Generates encryption key from PF8 archive header data
pub fn generate_key(data: &[u8], index_size: u32) -> Vec<u8> {
    let start = format::offsets::INDEX_DATA_START;
    let end = start + index_size as usize;

    if end > data.len() {
        // Fallback to available data if index_size is larger than actual data
        let available_data = &data[start..];
        let mut hasher = Sha1::new();
        hasher.update(available_data);
        return hasher.finalize().to_vec();
    }

    let index_data = &data[start..end];
    let mut hasher = Sha1::new();
    hasher.update(index_data);
    hasher.finalize().to_vec()
}

/// Encrypts data using XOR with the provided key, starting from a specific offset
pub fn encrypt(data: &mut [u8], key: &[u8], offset: usize) {
    if data.is_empty() { return; }
    assert!(!key.is_empty(), "empty XOR key");
    // Runtime division per byte is particularly expensive on ARMv7. Align
    // the first span once, then XOR complete key periods without division.
    let phase=offset%key.len();
    let head=(key.len()-phase).min(data.len());
    let (first,rest)=data.split_at_mut(head);
    for (byte,k) in first.iter_mut().zip(&key[phase..]) { *byte^=*k; }
    for chunk in rest.chunks_mut(key.len()) {
        for (byte,k) in chunk.iter_mut().zip(key) { *byte^=*k; }
    }
}

/// Decrypts data using XOR with the provided key (same as encrypt for XOR)
pub fn decrypt(data: &[u8], key: &[u8]) -> Vec<u8> {
    let mut output=data.to_vec();encrypt(&mut output,key,0);output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn period_spans_match_reference_and_split_reads() {
        for len in [1,3,20,31,256] {
            let key:Vec<u8>=(0..len).map(|i|(i*17+7) as u8).collect();
            for offset in [0,1,19,20,31,32767,32768] {
                for size in [0,1,19,20,21,33,257,32769] {
                    let source:Vec<u8>=(0..size).map(|i|(i*13+3) as u8).collect();
                    let expected:Vec<u8>=source.iter().enumerate().map(|(i,b)|b^key[(offset+i)%len]).collect();
                    let mut actual=source.clone();encrypt(&mut actual,&key,offset);assert_eq!(actual,expected);
                    let mut split=source.clone();
                    for (i,chunk) in split.chunks_mut(113).enumerate(){encrypt(chunk,&key,offset+i*113);}
                    assert_eq!(split,expected);
                    encrypt(&mut actual,&key,offset);assert_eq!(actual,source);
                    if offset==0 {assert_eq!(decrypt(&expected,&key),source);}
                }
            }
        }
        encrypt(&mut [],&[],0);assert!(decrypt(&[],&[]).is_empty());
    }
}
