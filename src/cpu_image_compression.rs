//! Opt-in, per-project CPU image cache. Checks run while PNG rows are emitted;
//! compression runs only after the enabled checks pass, on the prefetch worker.
use crate::resource_ledger::{Owner,Tracked};
use std::sync::{Mutex,OnceLock};
pub(crate) const BLOCK:usize=128*1024;
const MIN_BYTES:usize=512*1024;
const LIMIT:usize=16*1024*1024;

#[derive(Clone,Debug)]
pub(crate) struct Policy {
    pub size_enabled:bool,pub min_bytes:usize,
    pub enabled:bool,pub ratio_enabled:bool,pub percent:u32,
    pub runs_enabled:bool,pub mean_bytes:u32,pub folders:Vec<String>,pub ignore_bg:bool,
}
impl Default for Policy {
    fn default()->Self{Self{enabled:false,size_enabled:true,min_bytes:MIN_BYTES,ratio_enabled:true,percent:60,runs_enabled:false,mean_bytes:256,folders:vec![],ignore_bg:false}}
}
pub(crate) fn folder(path:&str)->Option<String>{
    let p=path.replace('\\',"/").trim_end_matches('/').to_ascii_lowercase();
    if p.is_empty()||p.len()>240||p.starts_with('/')||p.split('/').any(|s|s.is_empty()||s=="."||s=="..")
        ||p.chars().any(|c|c.is_control()||c==':'||c=='|'){None}else{Some(p)}
}
impl Policy {
    pub fn accepts_path(&self,path:&str)->bool {
        if !self.enabled{return false;}
        let Some(path)=folder(path) else{return false;};
        if self.ignore_bg&&(path.starts_with("bg/")||path.starts_with("image/bg/")||path.contains("/image/bg/")){return false;}
        self.folders.iter().any(|f|path.strip_prefix(f).is_some_and(|s|s.starts_with('/')))
    }
    pub fn accepts(&self,bytes:usize,s:Stats)->bool{
        self.accepts_size(bytes as u64)
            &&(!self.ratio_enabled||s.zero_pixels*100>(bytes/4) as u64*self.percent as u64)
            &&(!self.runs_enabled||(s.runs>0&&s.zero_bytes>=s.runs*self.mean_bytes as u64))
    }
    pub fn accepts_size(&self,bytes:u64)->bool{bytes>0&&bytes<=LIMIT as u64&&(!self.size_enabled||bytes>=self.min_bytes as u64)}
}
static POLICY:OnceLock<Mutex<Policy>>=OnceLock::new();
pub(crate) fn set_policy(p:Policy){*POLICY.get_or_init(||Mutex::new(Policy::default())).lock().unwrap()=p;}
pub(crate) fn policy_for(path:&str)->Option<Policy>{let p=POLICY.get_or_init(||Mutex::new(Policy::default())).lock().unwrap();p.accepts_path(path).then(||p.clone())}

#[derive(Clone,Copy,Default,Debug,PartialEq,Eq)]
pub(crate) struct Stats{pub zero_pixels:u64,pub zero_bytes:u64,pub runs:u64}
#[derive(Default)]
struct Counter{stats:Stats,at:usize,cell:u32,previous_zero:bool}
impl Counter{
    #[inline] fn pixel<const RATIO:bool,const RUNS:bool>(&mut self,p:u32){
        if RATIO{self.stats.zero_pixels+=u64::from(p==0);}
        if RUNS{
            if self.at%BLOCK==0{self.previous_zero=false;}
            self.cell|=p;self.at+=4;
            if self.at%64==0{self.finish_cell(64);}
        }
    }
    fn finish_cell(&mut self,bytes:usize){let empty=self.cell==0;if empty{self.stats.zero_bytes+=bytes as u64;if !self.previous_zero{self.stats.runs+=1;}}self.previous_zero=empty;self.cell=0;}
    fn finish<const RUNS:bool>(mut self)->Stats{if RUNS&&self.at%64!=0{self.finish_cell(self.at%64);}self.stats}
}
pub(crate) fn supported(source:&[u8],policy:&Policy)->bool{
    source.len()>=33&&source.starts_with(b"\x89PNG\r\n\x1a\n")&&source[24]==8&&matches!(source[25],3|6)&&source[28]==0
        &&(u32::from_be_bytes(source[16..20].try_into().unwrap()) as u64).checked_mul(u32::from_be_bytes(source[20..24].try_into().unwrap()) as u64).and_then(|n|n.checked_mul(4)).is_some_and(|n|policy.accepts_size(n))
}
pub(crate) fn decode<R:std::io::BufRead+std::io::Seek>(source:R,limit:usize,decoder_limit:usize,p:&Policy,cancel:&dyn Fn()->bool)->Option<(Tracked<image::RgbaImage>,Stats)>{
    match(p.ratio_enabled,p.runs_enabled){
        (true,true)=>rows::<R,true,true>(source,limit,decoder_limit,cancel),
        (true,false)=>rows::<R,true,false>(source,limit,decoder_limit,cancel),
        (false,true)=>rows::<R,false,true>(source,limit,decoder_limit,cancel),
        (false,false)=>rows::<R,false,false>(source,limit,decoder_limit,cancel),
    }
}
fn rows<R:std::io::BufRead+std::io::Seek,const RATIO:bool,const RUNS:bool>(source:R,limit:usize,decoder_limit:usize,cancel:&dyn Fn()->bool)->Option<(Tracked<image::RgbaImage>,Stats)>{
    let mut decoder=png::Decoder::new_with_limits(source,png::Limits{bytes:decoder_limit});
    decoder.set_transformations(png::Transformations::IDENTITY);
    let mut reader=decoder.read_info().ok()?;let info=reader.info();
    if info.bit_depth!=png::BitDepth::Eight||info.interlaced||info.animation_control.is_some(){return None;}
    let(w,h,color)=(info.width,info.height,info.color_type);
    let bytes=(w as usize).checked_mul(h as usize)?.checked_mul(4)?;if bytes==0||bytes>limit{return None;}
    let mut palette=[0u32;256];
    let palette_len=if color==png::ColorType::Indexed{
        let rgb=info.palette.as_deref()?;let alpha=info.trns.as_deref().unwrap_or(&[]);
        if rgb.is_empty()||rgb.len()%3!=0||rgb.len()>768||alpha.len()>rgb.len()/3{return None;}
        for(i,p)in rgb.chunks_exact(3).enumerate(){palette[i]=u32::from_ne_bytes([p[0],p[1],p[2],alpha.get(i).copied().unwrap_or(255)]);}rgb.len()/3
    }else if color==png::ColorType::Rgba{0}else{return None;};
    let mut charge=crate::resource_ledger::Charge::reserve(Owner::Decode,bytes);
    let mut out=Vec::new();out.try_reserve_exact(bytes).ok()?;out.resize(bytes,0);charge.commit(out.capacity());
    let mut counter=Counter::default();
    for row in out.chunks_exact_mut(w as usize*4){
        if cancel(){return None;}
        let input=reader.next_row().ok()??;let data=input.data();
        if color==png::ColorType::Indexed{
            if data.len()!=w as usize{return None;}
            for(out,&index)in row.chunks_exact_mut(4).zip(data){if index as usize>=palette_len{return None;}let p=palette[index as usize];out.copy_from_slice(&p.to_ne_bytes());counter.pixel::<RATIO,RUNS>(p);}
        }else{
            if data.len()!=row.len(){return None;}
            if RATIO||RUNS{for(out,p)in row.chunks_exact_mut(4).zip(data.chunks_exact(4)){let p=u32::from_ne_bytes(p.try_into().ok()?);out.copy_from_slice(&p.to_ne_bytes());counter.pixel::<RATIO,RUNS>(p);}}
            else{row.copy_from_slice(data);}
        }
    }
    reader.finish().ok()?;
    Some((Tracked{data:image::RgbaImage::from_raw(w,h,out)?,charge},counter.finish::<RUNS>()))
}

pub(crate) fn recognized(b:&[u8])->bool{b.starts_with(b"ZSC1")}
pub(crate) fn dimensions(b:&[u8])->Option<(u32,u32)>{
    if b.len()<16||!recognized(b){return None;}
    let w=word(b,4)?;let h=word(b,8)?;
    let n=(w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    (w>0&&h>0&&n<=LIMIT&&n==word(b,12)? as usize).then_some((w,h))
}
fn word(b:&[u8],at:usize)->Option<u32>{Some(u32::from_le_bytes(b.get(at..at+4)?.try_into().ok()?))}
fn append(out:&mut Tracked<Vec<u8>>,b:&[u8],limit:usize)->Option<()>{
    let needed=out.len().checked_add(b.len())?;if needed>limit{return None;}
    if needed>out.capacity(){
        let target=needed.max(out.capacity().saturating_mul(2)).max(4096).min(limit);
        let additional=target-out.len();out.try_reserve_exact(additional).ok()?;out.sync_capacity();
        if out.capacity()>limit{return None;}
    }
    out.extend_from_slice(b);Some(())
}
pub(crate) fn compress(w:u32,h:u32,rgba:&[u8],max_storage:usize,cancel:&dyn Fn()->bool)->Option<Tracked<Vec<u8>>>{
    if (w as usize).checked_mul(h as usize)?.checked_mul(4)?!=rgba.len()||rgba.is_empty()||rgba.len()>LIMIT{return None;}
    // This is a scratch-allocation bound, not a 25% saving requirement.
    let mut out=Tracked::bytes(Vec::new(),Owner::Decode);append(&mut out,b"ZSC1",max_storage)?;
    for v in [w,h,rgba.len() as u32]{append(&mut out,&v.to_le_bytes(),max_storage)?;}
    for block in rgba.chunks(BLOCK){
        if cancel(){return None;}
        let header=out.len();append(&mut out,&[0;8],max_storage)?;
        let mut at=0;
        let zero=|p:usize|block[p..(p+64).min(block.len())].chunks(4).all(|v|v==[0,0,0,0]);
        while at<block.len(){
            let start=at;while at<block.len()&&zero(at){at=(at+64).min(block.len());}
            let literal=at;while at<block.len()&&!zero(at){at=(at+64).min(block.len());}
            append(&mut out,&((literal-start)as u32).to_le_bytes(),max_storage)?;
            append(&mut out,&((at-literal)as u32).to_le_bytes(),max_storage)?;
            append(&mut out,&block[literal..at],max_storage)?;
        }
        let len=out.len()-header-8;
        out[header..header+4].copy_from_slice(&(block.len()as u32).to_le_bytes());
        out[header+4..header+8].copy_from_slice(&(len as u32).to_le_bytes());
    }
    if out.capacity()>max_storage{return None;}
    out.shrink_to_fit();out.sync_capacity();Some(out)
}
#[derive(Default)]
pub(crate) struct Restore{pub input:usize,pub output:usize}
impl Restore{
    pub fn new()->Self{Self{input:16,output:0}}
    // One 128 KiB block. The callback writes directly to the private GPU
    // allocation. None source denotes a zero fill; no reads from GPU memory.
    pub fn step(&mut self,b:&[u8],write:&mut impl FnMut(usize,Option<&[u8]>,usize)->bool)->Option<bool>{
        let(w,h)=dimensions(b)?;let total=w as usize*h as usize*4;
        if self.output==total{return (self.input==b.len()).then_some(true);}
        let raw=word(b,self.input)? as usize;let packed=word(b,self.input+4)? as usize;self.input+=8;
        if raw!=BLOCK.min(total-self.output){return None;}
        let end=self.input.checked_add(packed)?;if end>b.len(){return None;}
        let target=self.output+raw;
        while self.output<target{
            if end.saturating_sub(self.input)<8{return None;}
            let zero=word(b,self.input)? as usize;let len=word(b,self.input+4)? as usize;self.input+=8;
            if zero%4!=0||len%4!=0||(zero==0&&len==0)||zero>target-self.output||len>target-self.output-zero||len>end-self.input{return None;}
            if zero>0&&!write(self.output,None,zero){return None;}self.output+=zero;
            if len>0&&!write(self.output,Some(&b[self.input..self.input+len]),len){return None;}self.output+=len;self.input+=len;
        }
        if self.input!=end{return None;}
        if self.output==total&&self.input!=b.len(){return None;}
        Some(self.output==total)
    }
}

#[cfg(test)] mod tests {
    use super::*;
    fn reference(bytes:&[u8])->Stats{
        let mut s=Stats{zero_pixels:bytes.chunks_exact(4).filter(|p|*p==[0,0,0,0]).count() as u64,..Default::default()};
        for block in bytes.chunks(BLOCK){let mut previous=false;for cell in block.chunks(64){let zero=cell.iter().all(|b|*b==0);if zero{s.zero_bytes+=cell.len() as u64;s.runs+=u64::from(!previous);}previous=zero;}}s
    }
    fn restore(bytes:&[u8])->Option<Vec<u8>>{
        let(w,h)=dimensions(bytes)?;let mut out=vec![0x99;w as usize*h as usize*4];let mut cursor=Restore::new();
        while !cursor.step(bytes,&mut|at,data,len|{if let Some(data)=data{out[at..at+len].copy_from_slice(data);}else{out[at..at+len].fill(0);}true})?{}Some(out)
    }
    fn sample(indexed:bool)->(Vec<u8>,Vec<u8>){
        let(w,h)=(513u32,257u32);let palette=[[0,0,0,0],[12,34,56,0],[20,30,40,127],[255,40,9,255]];
        let indices:Vec<u8>=(0..w*h).map(|i|if i%4301<4000{0}else{(i%3+1)as u8}).collect();
        let raw:Vec<_>=indices.iter().flat_map(|i|palette[*i as usize]).collect();let mut bytes=Vec::new();
        {let mut e=png::Encoder::new(&mut bytes,w,h);e.set_depth(png::BitDepth::Eight);
        if indexed{e.set_color(png::ColorType::Indexed);e.set_palette(palette.iter().flat_map(|p|p[..3].iter().copied()).collect::<Vec<_>>());e.set_trns(vec![0,0,127]);}
        else{e.set_color(png::ColorType::Rgba);}let mut writer=e.write_header().unwrap();writer.write_image_data(if indexed{&indices}else{&raw}).unwrap();}
        (bytes,raw)
    }
    #[test] fn independent_decode_counters_match_reference_and_preserve_hidden_rgb(){
        for indexed in [false,true]{let(bytes,raw)=sample(indexed);assert!(supported(&bytes,&Policy::default()));let expect=reference(&raw);
            for ratio in [false,true]{for runs in [false,true]{let p=Policy{ratio_enabled:ratio,runs_enabled:runs,..Default::default()};
                let(image,s)=decode(std::io::Cursor::new(&bytes),LIMIT,LIMIT,&p,&||false).unwrap();
                assert_eq!(image.as_raw(),&raw);assert_eq!(s.zero_pixels,if ratio{expect.zero_pixels}else{0});
                assert_eq!(s.zero_bytes,if runs{expect.zero_bytes}else{0});assert_eq!(s.runs,if runs{expect.runs}else{0});
                assert!(restore(&compress(513,257,&raw,LIMIT,&||false).unwrap()).unwrap()==raw);
            }}
        }
    }
    #[test] fn selection_is_before_encoding_with_strict_ratio_and_independent_switches(){
        let mut p=Policy::default();assert!(!p.enabled);assert!(p.ratio_enabled);assert!(!p.runs_enabled);
        let bytes=640*1024;let mut s=Stats{zero_pixels:(bytes/4*60/100)as u64,zero_bytes:256,runs:1};
        assert!(!p.accepts(bytes,s));s.zero_pixels+=1;assert!(p.accepts(bytes,s));
        p.runs_enabled=true;assert!(p.accepts(bytes,s));s.zero_bytes=255;assert!(!p.accepts(bytes,s));
        p.ratio_enabled=false;s.zero_pixels=0;s.zero_bytes=256;assert!(p.accepts(bytes,s));
        s.runs=0;assert!(!p.accepts(bytes,s));p.runs_enabled=false;assert!(p.accepts(bytes,s));
        assert!(!p.accepts(MIN_BYTES-4,s));assert!(!p.accepts(LIMIT+4,s));
        p.folders=vec!["image/fg".into()];assert!(!p.accepts_path("image/fg/a.png"));p.enabled=true;
        assert!(p.accepts_path("IMAGE\\FG\\nested\\a.png"));assert!(!p.accepts_path("image/fg2/a.png"));assert!(!p.accepts_path("image/fg/../a.png"));
        p.folders.push("pc/image/bg".into());p.ignore_bg=true;assert!(!p.accepts_path("pc/image/bg/a.png"));
    }
    #[test] fn configurable_size_gate_uses_header_without_decoding(){
        let(bytes,_)=sample(false);let mut p=Policy::default();assert!(supported(&bytes,&p));
        p.min_bytes=1024*1024;assert!(!supported(&bytes,&p));p.size_enabled=false;assert!(supported(&bytes,&p));
        assert!(p.accepts_size(4));assert!(!p.accepts_size(0));assert!(!p.accepts_size(LIMIT as u64+4));
        p.size_enabled=true;p.min_bytes=128*1024;assert!(!p.accepts_size(128*1024-4));assert!(p.accepts_size(128*1024));
        let mut corrupt=bytes;corrupt[16..24].fill(255);assert!(!supported(&corrupt,&p));
    }
    #[test] fn codec_checks_bounds_cancel_and_rejects_truncation(){
        let(_,raw)=sample(false);let b=compress(513,257,&raw,LIMIT,&||false).unwrap();assert_eq!(restore(&b).unwrap(),raw);
        assert!(compress(513,257,&raw,16,&||false).is_none());assert!(compress(513,257,&raw,LIMIT,&||true).is_none());
        for len in [0,4,15,16,20,24,b.len()-1]{assert!(restore(&b[..len]).is_none());}
        let mut corrupt=b.data.clone();corrupt.extend_from_slice(&[0]);assert!(restore(&corrupt).is_none());
        for at in [4,8,12,16,20,24,28]{let mut corrupt=b.data.clone();corrupt[at..at+4].copy_from_slice(&u32::MAX.to_le_bytes());assert!(restore(&corrupt).is_none());}
        let mut cursor=Restore::new();assert!(cursor.step(&b,&mut|_,_,_|false).is_none());
    }
    #[test] fn blocks_break_runs_and_last_cell_counts_only_existing_bytes(){
        let raw=vec![0;BLOCK*2+12];let mut c=Counter::default();for _ in raw.chunks_exact(4){c.pixel::<true,true>(0);}
        assert_eq!(c.finish::<true>(),Stats{zero_pixels:raw.len()as u64/4,zero_bytes:raw.len()as u64,runs:3});
        let bytes=compress(1,(raw.len()/4)as u32,&raw,LIMIT,&||false).unwrap();assert_eq!(restore(&bytes).unwrap(),raw);
    }
    #[test] #[ignore="Set CPU_CACHE_CORPUS to an external PNG directory"]
    fn external_corpus_matches_original_pixels_all_switches(){
        fn visit(path:&std::path::Path,files:&mut Vec<std::path::PathBuf>){for e in std::fs::read_dir(path).unwrap(){let p=e.unwrap().path();if p.is_dir(){visit(&p,files);}else if p.extension().is_some_and(|e|e.eq_ignore_ascii_case("png")){files.push(p);}}}
        let mut files=Vec::new();visit(std::path::Path::new(&std::env::var("CPU_CACHE_CORPUS").unwrap()),&mut files);let mut count=0;
        for path in files{let bytes=std::fs::read(&path).unwrap();if !supported(&bytes,&Policy::default()){continue;}let original=image::load_from_memory(&bytes).unwrap().to_rgba8();let expected=reference(original.as_raw());
            for ratio in [false,true]{for runs in [false,true]{let p=Policy{ratio_enabled:ratio,runs_enabled:runs,..Default::default()};let(image,s)=decode(std::io::Cursor::new(&bytes),LIMIT,LIMIT,&p,&||false).unwrap();assert_eq!(image.data,original,"{}",path.display());assert_eq!(s,Stats{zero_pixels:if ratio{expected.zero_pixels}else{0},zero_bytes:if runs{expected.zero_bytes}else{0},runs:if runs{expected.runs}else{0}});}}
            let b=compress(original.width(),original.height(),original.as_raw(),LIMIT,&||false).unwrap();assert_eq!(restore(&b).unwrap(),*original.as_raw());count+=1;
        }assert!(count>0);println!("Verified {count} candidate PNGs, four switch combinations and exact compression roundtrip");
    }
}
