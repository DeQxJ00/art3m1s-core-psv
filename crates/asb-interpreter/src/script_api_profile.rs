//! Per-thread inclusive API spans for the existing bounded slow-command log.
use std::cell::Cell;
#[derive(Clone,Copy,Default)]
pub(crate) struct Snapshot { calls:[u64;15], micros:[u64;15] }
thread_local! { static TOTAL:Cell<Snapshot>=Cell::new(Snapshot::default()); }
fn now()->u64{
    unsafe extern "C" {fn sceKernelGetProcessTimeWide()->u64;}
    unsafe{sceKernelGetProcessTimeWide()}
}
pub(crate) fn snapshot()->Snapshot{TOTAL.with(Cell::get)}
pub(crate) struct Span {kind:usize,started:u64}
pub(crate) fn span(kind:usize)->Span{Span{kind,started:now()}}
impl Drop for Span{
    fn drop(&mut self){let elapsed=now().saturating_sub(self.started);TOTAL.with(|cell|{
        let mut v=cell.get();v.calls[self.kind]+=1;v.micros[self.kind]+=elapsed;cell.set(v);
    });}
}
impl Snapshot{
    pub(crate) fn since(self,before:Self)->String{
        let names=["tag","var","exists","block","bind","unbind","comments","size","tag_convert","tag_lock","tag_observe","tag_query","tag_queue","tag_plain","file_read"];
        names.iter().enumerate().filter_map(|(i,name)|{
            let count=self.calls[i].saturating_sub(before.calls[i]);
            (count>0).then(||format!("{}:{}/{}",name,count,self.micros[i].saturating_sub(before.micros[i])))
        }).collect::<Vec<_>>().join(",")
    }
}
