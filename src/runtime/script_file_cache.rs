//! Bounded source reuse for repeated small files read by game scripts.
use std::sync::Mutex;
use std::collections::VecDeque;
use crate::resource_ledger::{Tracked,Owner};

const MAX_FILE:usize=32*1024;
const MAX_BYTES:usize=256*1024;
const MAX_ENTRIES:usize=64;
struct Entry {path:String,bytes:Tracked<Vec<u8>>}
impl Entry {fn cost(&self)->usize{self.path.capacity()+self.bytes.capacity()}}
#[derive(Default)]
pub(super) struct SmallFiles {
    epoch:u64, entries:VecDeque<Entry>, bytes:usize, hits:u64, misses:u64,
}
impl SmallFiles {
    fn synchronize(&mut self,epoch:u64){
        if self.epoch!=epoch {self.entries.clear();self.bytes=0;self.epoch=epoch;}
    }
    pub(super) fn read(cache:&Mutex<Self>, path:&str, epoch:impl Fn()->u64,
        load:impl FnOnce()->Result<Vec<u8>,String>)->Result<Vec<u8>,String> {
        let started=crate::profile_clock::Instant::now();
        let before=epoch();
        {
            let mut c=cache.lock().unwrap();c.synchronize(before);
            if let Some(index)=c.entries.iter().position(|e|e.path==path){
                let entry=c.entries.remove(index).unwrap();
                let data=entry.bytes.to_vec();c.entries.push_back(entry);c.hits+=1;
                if c.hits==1||c.hits%64==0{
                    crate::core_info!("[script-file-cache] hits={} misses={} entries={} retained_bytes={} hit_us={}",
                        c.hits,c.misses,c.entries.len(),c.bytes,started.elapsed().as_micros());
                }
                return Ok(data);
            }
            c.misses+=1;
        }
        // Never retain the cache mutex while entering the host/archive reader.
        // Failed reads and data read across invalidation are not reusable.
        let data=load()?;
        if data.len()<=MAX_FILE && path.len()<=1024 && epoch()==before {
            let mut c=cache.lock().unwrap();
            if epoch()==before {
                c.synchronize(before);
                if !c.entries.iter().any(|e|e.path==path){
                    let entry=Entry{path:path.to_owned(),bytes:Tracked::bytes(data.clone(),Owner::Source)};
                    let cost=entry.cost();
                    while c.entries.len()>=MAX_ENTRIES || c.bytes+cost>MAX_BYTES {
                        let Some(old)=c.entries.pop_front() else{break;};c.bytes-=old.cost();
                    }
                    c.bytes+=cost;c.entries.push_back(entry);
                }
            }
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn repeated_reads_reuse_bytes_but_epoch_changes_and_errors_do_not() {
        let cache=Mutex::new(SmallFiles::default());let calls=Cell::new(0);
        let read=|epoch|SmallFiles::read(&cache,"asset/table.csv",||epoch,||{
            calls.set(calls.get()+1);Ok(vec![calls.get() as u8;20])}).unwrap();
        let mut first=read(1);first[0]=99;
        assert_eq!(read(1),vec![1;20]);assert_eq!(calls.get(),1);
        assert_eq!(read(2),vec![2;20]);assert_eq!(calls.get(),2);
        assert!(SmallFiles::read(&cache,"missing",||2,||Err("missing".into())).is_err());
        assert_eq!(SmallFiles::read(&cache,"missing",||2,||Ok(vec![3])).unwrap(),vec![3]);
    }
    #[test]
    fn cache_is_bounded_lru_and_does_not_keep_large_or_invalidated_reads() {
        let cache=Mutex::new(SmallFiles::default());
        for i in 0..MAX_ENTRIES {
            SmallFiles::read(&cache,&format!("asset/{i}"),||1,||Ok(vec![1;32])).unwrap();
        }
        SmallFiles::read(&cache,"asset/0",||1,||panic!("must be cached")).unwrap();
        SmallFiles::read(&cache,"new",||1,||Ok(vec![2])).unwrap();
        {
            let c=cache.lock().unwrap();assert_eq!(c.entries.len(),MAX_ENTRIES);
            assert!(c.entries.iter().any(|e|e.path=="asset/0"));
            assert!(!c.entries.iter().any(|e|e.path=="asset/1"));
        }
        for i in 0..20 {
            SmallFiles::read(&cache,&format!("chunk/{i}"),||1,||Ok(vec![3;MAX_FILE])).unwrap();
            assert!(cache.lock().unwrap().bytes<=MAX_BYTES);
        }
        let calls=Cell::new(0);
        for _ in 0..2 {
            SmallFiles::read(&cache,"large",||1,||{calls.set(calls.get()+1);Ok(vec![4;MAX_FILE+1])}).unwrap();
        }
        assert_eq!(calls.get(),2);
        let epoch=Cell::new(1);
        SmallFiles::read(&cache,"changed",||epoch.get(),||{epoch.set(2);Ok(vec![5])}).unwrap();
        assert_eq!(SmallFiles::read(&cache,"changed",||2,||Ok(vec![6])).unwrap(),vec![6]);
        // The host reader can itself invalidate or reenter without a cache lock.
        SmallFiles::read(&cache,"unlocked",||2,||{assert!(cache.try_lock().is_ok());Ok(vec![])}).unwrap();
    }

}
