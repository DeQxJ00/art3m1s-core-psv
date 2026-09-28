//! Conservative retained-allocation estimates; excludes allocator headers.
use std::{collections::BTreeMap, mem::size_of};
pub(crate) trait HeapBytes { fn heap_bytes(&self)->usize; }
impl HeapBytes for String { fn heap_bytes(&self)->usize{self.capacity()} }
impl<T:HeapBytes> HeapBytes for Vec<T> {
    fn heap_bytes(&self)->usize{self.capacity()*size_of::<T>()+self.iter().map(HeapBytes::heap_bytes).sum::<usize>()}
}
impl<T:HeapBytes> HeapBytes for Option<T> { fn heap_bytes(&self)->usize{self.as_ref().map_or(0,HeapBytes::heap_bytes)} }
impl<T:HeapBytes> HeapBytes for Box<T> { fn heap_bytes(&self)->usize{size_of::<T>()+(**self).heap_bytes()} }
impl<K:HeapBytes,V:HeapBytes> HeapBytes for BTreeMap<K,V> {
    fn heap_bytes(&self)->usize{
        // Rust BTree nodes hold up to 11 pairs; non-root nodes hold at least
        // five. Charge every node as an internal node (12 child pointers).
        let nodes=if self.is_empty(){0}else{1+(self.len()-1)/5};
        nodes*(4*size_of::<usize>()+11*(size_of::<K>()+size_of::<V>())+12*size_of::<usize>())
            +self.iter().map(|(k,v)|k.heap_bytes()+v.heap_bytes()).sum::<usize>()
    }
}
impl<A:HeapBytes,B:HeapBytes> HeapBytes for (A,B){fn heap_bytes(&self)->usize{self.0.heap_bytes()+self.1.heap_bytes()}}
impl<T,const N:usize> HeapBytes for [T;N]{fn heap_bytes(&self)->usize{0}}
macro_rules! scalar {($($t:ty),*)=>{$(impl HeapBytes for $t{fn heap_bytes(&self)->usize{0}})*};}
scalar!(f32,i64,usize);
macro_rules! fields {($t:ty,$($f:ident),+)=>{impl crate::memory::HeapBytes for $t{
    fn heap_bytes(&self)->usize{0 $(+crate::memory::HeapBytes::heap_bytes(&self.$f))+}
}};}
pub(crate) use fields;
