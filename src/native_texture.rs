//! Validated 2D DDS/PVR3 containers. Payloads stay compressed through READY and
//! native upload; the runtime currently samples mip zero, like PNG resources.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
#[repr(u32)]
pub(crate) enum Format {
    Bc1=1,Bc2,Bc3,Bc4,Bc4Signed,Bc5,Bc5Signed,
    PvrtcRgb2,PvrtcRgba2,PvrtcRgb4,PvrtcRgba4,Pvrtc2_2,Pvrtc2_4,Etc1,
}
impl Format {
    pub fn block(self)->(u32,u32,usize){
        match self {
            Self::PvrtcRgb2|Self::PvrtcRgba2|Self::Pvrtc2_2=>(8,4,8),
            Self::Bc2|Self::Bc3|Self::Bc5|Self::Bc5Signed=>(4,4,16),
            _=>(4,4,8),
        }
    }
    pub fn pvrtc1(self)->bool{matches!(self,Self::PvrtcRgb2|Self::PvrtcRgba2|Self::PvrtcRgb4|Self::PvrtcRgba4)}
    pub fn opaque(self)->bool{matches!(self,Self::Bc4|Self::Bc4Signed|Self::Bc5|Self::Bc5Signed|Self::PvrtcRgb2|Self::PvrtcRgb4|Self::Etc1)}
    pub fn bytes(self,w:u32,h:u32)->usize{
        let (bw,bh,n)=self.block();let min=if self.pvrtc1(){2}else{1};
        w.div_ceil(bw).max(min) as usize*h.div_ceil(bh).max(min) as usize*n
    }
    pub fn storage_bytes(self,w:u32,h:u32)->usize{self.bytes(w.next_power_of_two(),h.next_power_of_two())}
}
#[derive(Clone,Copy,Debug)]
pub(crate) struct Texture<'a>{pub format:Format,pub width:u32,pub height:u32,pub opaque:bool,pub data:&'a[u8]}
pub(crate) fn recognized(b:&[u8])->bool{b.starts_with(b"DDS ")||b.starts_with(b"PVR\x03")}
fn u32_at(b:&[u8],p:usize)->Result<u32,&'static str>{
    Ok(u32::from_le_bytes(b.get(p..p+4).ok_or("truncated header")?.try_into().unwrap()))
}
fn finish(b:&[u8],format:Format,w:u32,h:u32,mips:u32,start:usize,opaque:bool)->Result<Texture<'_>,&'static str>{
    if w==0||h==0||w>4096||h>4096{return Err("dimensions outside GXM limits");}
    if mips==0||mips>32-w.max(h).leading_zeros(){return Err("invalid mip count");}
    if format.pvrtc1(){
        let (bw,bh,_)=format.block();
        if !w.is_power_of_two()||!h.is_power_of_two()||w<bw*2||h<bh*2{return Err("PVRTC1 requires power-of-two dimensions and at least 2x2 blocks");}
    }
    let mut total=0usize;
    for level in 0..mips {total=total.checked_add(format.bytes((w>>level).max(1),(h>>level).max(1))).ok_or("payload overflow")?;}
    let end=start.checked_add(total).ok_or("payload overflow")?;
    if end>b.len(){return Err("truncated mip payload");}
    Ok(Texture{format,width:w,height:h,opaque:opaque||format.opaque(),data:&b[start..start+format.bytes(w,h)]})
}
pub(crate) fn parse(b:&[u8])->Result<Texture<'_>,&'static str>{
    use Format::*;
    if b.starts_with(b"PVR\x03"){
        let flags=u32_at(b,4)?;let code=u32_at(b,8)?;
        if flags&!2!=0||flags&2!=0{return Err("unsupported PVR flags/premultiplied alpha");}
        if u32_at(b,12)?!=0{return Err("uncompressed PVR format");}
        if u32_at(b,16)?>1{return Err("unknown PVR color space");}
        let channel=u32_at(b,20)?;
        let format=match code {
            0=>PvrtcRgb2,1=>PvrtcRgba2,2=>PvrtcRgb4,3=>PvrtcRgba4,4=>Pvrtc2_2,5=>Pvrtc2_4,
            6=>Etc1,7=>Bc1,9=>Bc2,11=>Bc3,12=>if channel==1{Bc4Signed}else{Bc4},13=>if channel==1{Bc5Signed}else{Bc5},
            _=>return Err("unsupported PVR compression"),
        };
        if channel!=0 && !(channel==1&&matches!(format,Bc4Signed|Bc5Signed)){return Err("unsupported PVR channel type");}
        let h=u32_at(b,24)?;let w=u32_at(b,28)?;
        if u32_at(b,32)?!=1||u32_at(b,36)?!=1||u32_at(b,40)?!=1{return Err("only single 2D textures supported");}
        let mips=u32_at(b,44)?;
        let start=52usize.checked_add(u32_at(b,48)? as usize).ok_or("metadata overflow")?;
        if start>b.len(){return Err("truncated metadata");}
        let mut pos=52;
        while pos<start {
            if start-pos<12{return Err("truncated metadata header");}
            let creator=u32_at(b,pos)?;let key=u32_at(b,pos+4)?;let n=u32_at(b,pos+8)? as usize;
            pos+=12;let end=pos.checked_add(n).ok_or("metadata overflow")?;
            if end>start{return Err("truncated metadata block");}
            // PVR orientation: right/down/out is the runtime's image convention.
            if creator==0x03525650&&key==3&&(n!=3||b[pos..end]!=[0,0,0]){return Err("unsupported PVR orientation");}
            pos=end;
        }
        finish(b,format,w,h,mips,start,false)
    }else if b.starts_with(b"DDS "){
        if u32_at(b,4)?!=124||u32_at(b,76)?!=32{return Err("invalid DDS header size");}
        if u32_at(b,80)?&4==0{return Err("DDS must use a compressed FOURCC");}
        if u32_at(b,24)?>1||u32_at(b,112)?&0x20fe00!=0{return Err("DDS volume/cubemap not supported");}
        let h=u32_at(b,12)?;let w=u32_at(b,16)?;let mips=u32_at(b,28)?.max(1);
        let fourcc=b.get(84..88).ok_or("truncated FOURCC")?;
        let mut start=128;let mut opaque=false;
        let format=match fourcc {
            b"DXT1"=>Bc1,b"DXT3"=>Bc2,b"DXT5"=>Bc3,b"ATI1"|b"BC4U"=>Bc4,b"BC4S"=>Bc4Signed,
            b"ATI2"|b"BC5U"=>Bc5,b"BC5S"=>Bc5Signed,
            b"DX10"=>{
                if u32_at(b,132)?!=3||u32_at(b,136)?&4!=0||u32_at(b,140)?!=1{return Err("DDS DX10 must be single 2D");}
                let alpha=u32_at(b,144)?;
                if !matches!(alpha,0|1|3){return Err("unsupported DDS alpha mode");}opaque=alpha==3;start=148;
                match u32_at(b,128)?{71|72=>Bc1,74|75=>Bc2,77|78=>Bc3,80=>Bc4,81=>Bc4Signed,83=>Bc5,84=>Bc5Signed,_=>return Err("unsupported DXGI compression")}
            },
            _=>return Err("unsupported DDS compression"),
        };
        finish(b,format,w,h,mips,start,opaque)
    }else{Err("not a native texture container")}
}

// Preserve legacy selection; native formats are candidates when PNG/raw/JPEG
// are absent. Explicit .dds/.pvr paths also reach the same parser by magic.
pub(crate) const SUFFIXES:[&str;6]=[".png","",".jpg",".jpeg",".dds",".pvr"];

pub(crate) fn candidates(path:&str)->impl Iterator<Item=std::borrow::Cow<'_,str>>{
    use std::borrow::Cow;
    let stem=path.rsplit_once('.').filter(|(_,ext)|["png","jpg","jpeg"].iter().any(|e|ext.eq_ignore_ascii_case(e))).map(|(stem,_)|stem);
    SUFFIXES.into_iter().map(move |s|if s.is_empty(){Cow::Borrowed(path)}else{Cow::Owned(format!("{path}{s}"))})
        .chain(stem.into_iter().flat_map(|s|[Cow::Owned(format!("{s}.dds")),Cow::Owned(format!("{s}.pvr"))]))
}

// Script size queries use bounded header reads, independently of payload
// validation (which is mandatory before admission/upload).
pub(crate) fn header_dimensions(b:&[u8])->Option<(u32,u32)>{
    let (w,h)=if b.starts_with(b"DDS ")&&u32_at(b,4).ok()?==124&&b.len()>=128{
        (u32_at(b,16).ok()?,u32_at(b,12).ok()?)
    }else if b.starts_with(b"PVR\x03")&&b.len()>=52{
        (u32_at(b,28).ok()?,u32_at(b,24).ok()?)
    }else{return None;};
    (w>0&&h>0&&w<=4096&&h<=4096).then_some((w,h))
}

#[cfg(test)] pub(crate) mod tests {
    use super::*;
    #[test] fn script_paths_and_header_queries(){
        assert_eq!(candidates(":bg/room").collect::<Vec<_>>(),[":bg/room.png",":bg/room",":bg/room.jpg",":bg/room.jpeg",":bg/room.dds",":bg/room.pvr"]);
        assert_eq!(candidates("image/fg/body.png").last().unwrap(),"image/fg/body.pvr");
        assert_eq!(candidates("body.dds").filter(|p|p.as_ref()=="body.dds").count(),1);
        let b=pvr(11,0,32,16);assert_eq!(header_dimensions(&b[..52]),Some((32,16)));assert!(parse(&b[..52]).is_err());
    }
    pub(crate) fn pvr(code:u32,channel:u32,w:u32,h:u32)->Vec<u8>{
        let mut b=Vec::new();for n in [0x03525650,0,code,0,0,channel,h,w,1,1,1,1,0]{b.extend_from_slice(&n.to_le_bytes());}
        b.resize(52+16*1024,0);let t=parse(&b).unwrap();b.truncate(52+t.data.len());b
    }
    #[test] fn all_formats_sizes_and_rejection(){
        for (code,channel,f) in [(7,0,Format::Bc1),(9,0,Format::Bc2),(11,0,Format::Bc3),(12,0,Format::Bc4),(12,1,Format::Bc4Signed),(13,0,Format::Bc5),(13,1,Format::Bc5Signed),
            (0,0,Format::PvrtcRgb2),(1,0,Format::PvrtcRgba2),(2,0,Format::PvrtcRgb4),(3,0,Format::PvrtcRgba4),(4,0,Format::Pvrtc2_2),(5,0,Format::Pvrtc2_4),(6,0,Format::Etc1)]{
            let b=pvr(code,channel,32,16);let t=parse(&b).unwrap();assert_eq!(t.format,f);assert_eq!(t.data.len(),f.storage_bytes(32,16));
            for n in 0..b.len(){assert!(parse(&b[..n]).is_err());}
            for (at,val) in [(4,2),(28,0),(28,4097),(32,2),(36,2),(40,6),(44,32),(48,u32::MAX)]{
                let mut bad=b.clone();bad[at..at+4].copy_from_slice(&val.to_le_bytes());assert!(parse(&bad).is_err());
            }
        }
        assert!(!recognized(b"PNG"));
    }
    #[test] fn dds_legacy_dx10_and_invalid_flags(){
        for (cc,format) in [(b"DXT1",Format::Bc1),(b"DXT3",Format::Bc2),(b"DXT5",Format::Bc3),(b"BC4U",Format::Bc4),(b"BC4S",Format::Bc4Signed),(b"BC5U",Format::Bc5),(b"BC5S",Format::Bc5Signed)]{
            let mut b=vec![0;128+format.bytes(17,9)];b[..4].copy_from_slice(b"DDS ");
            for (at,v) in [(4,124u32),(12,9),(16,17),(76,32),(80,4)]{b[at..at+4].copy_from_slice(&v.to_le_bytes());}b[84..88].copy_from_slice(cc);
            assert_eq!(parse(&b).unwrap().format,format);assert_eq!(parse(&b).unwrap().width,17);
            b[112..116].copy_from_slice(&0x200u32.to_le_bytes());assert!(parse(&b).is_err());
        }
        for code in [71,72,74,75,77,78,80,81,83,84] {
            let mut b=vec![0;148+64];b[..4].copy_from_slice(b"DDS ");b[84..88].copy_from_slice(b"DX10");
            for (at,v) in [(4,124u32),(12,4),(16,4),(76,32),(80,4),(128,code),(132,3),(140,1)]{b[at..at+4].copy_from_slice(&v.to_le_bytes());}
            assert!(parse(&b).is_ok());b[144]=2;assert!(parse(&b).is_err());
        }
    }
}
