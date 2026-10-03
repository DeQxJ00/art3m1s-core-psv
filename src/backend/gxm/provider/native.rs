use super::*;

impl GxmTextureProvider {
    pub(super) fn upload_native(&mut self,name:&str,bytes:Tracked<Vec<u8>>)->Option<(TextureId,TextureInfo)>{
        let t=match crate::native_texture::parse(&bytes){Ok(t)=>t,Err(error)=>{
            self.timing.decode_errors+=1;
            if self.reported_failures.insert(name.into()){crate::core_warn!("GXM native texture rejected: {name}: {error}");}
            return None;
        }};
        let info=TextureInfo{width:t.width,height:t.height};
        let opaque=t.opaque||(self.ignore_background_alpha&&background_alpha::background_source(name));
        let pixel_bytes=t.format.storage_bytes(t.width,t.height);
        let id=self.entries.get(name).map_or(TextureId(self.next_id),|e|e.id);
        let started=Instant::now();
        let ok=unsafe{art3m1s_gxm_upload_compressed(id.0,t.width,t.height,t.format as u32,opaque as u32,t.data.as_ptr(),t.data.len())};
        let us=elapsed_us(started);self.timing.uploads+=1;self.timing.upload_us+=us;
        self.timing.upload_max_us=self.timing.upload_max_us.max(us);
        if ok<=0{
            self.timing.upload_errors+=1;
            if ok==0{self.defer_upload(pixel_bytes.div_ceil(256*1024)*256*1024);self.keep_encoded(name,bytes,None);}
            else if self.reported_failures.insert(name.into()){
                crate::core_warn!("GXM native texture unsupported by this device: {name}: {:?}",t.format);
            }
            return None;
        }
        self.timing.upload_bytes+=t.data.len() as u64;
        let native_bytes=ok as usize;
        if id.0==self.next_id{self.next_id+=1;}
        self.revision=self.revision.wrapping_add(1).max(1);
        self.decoded.remove(name);self.encoded.remove(name);
        self.entries.insert(name.into(),Entry { sprite_crop:None,native_bytes,bc3:true,alpha_only:false,gray:false,id,info,
            rgba:Tracked::bytes(Vec::new(),Owner::Provider),opaque,revision:self.revision,
            last_used:self.cache_clock,cacheable:true,reclaimable:false,shared:true});
        self.ids.insert(id,name.into());
        crate::core_info!("GXM native-upload name={} format={:?} size={}x{} source_bytes={} gpu_pixel_bytes={} gpu_alloc_bytes={} upload_us={}",
            name,t.format,t.width,t.height,bytes.len(),pixel_bytes,native_bytes,us);
        self.keep_encoded(name,bytes,None);
        Some((id,info))
    }
}
