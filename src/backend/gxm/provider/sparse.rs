use super::*;
unsafe extern "C"{
    fn art3m1s_gxm_sparse_write(handle:usize,offset:usize,source:*const u8,len:usize)->i32;
    fn art3m1s_gxm_surface_publish_strided(handle:usize,id:u64,proof:*const u8,count:usize)->i32;
}
pub(super) fn restore_step(handle:usize,bytes:&[u8],cursor:&mut crate::cpu_image_compression::Restore)->Option<bool>{
    cursor.step(bytes,&mut|offset,source,len|unsafe{art3m1s_gxm_sparse_write(handle,offset,source.map_or(std::ptr::null(),|p|p.as_ptr()),len)>0})
}
impl GxmTextureProvider{
    pub(super) fn upload_sparse(&mut self,name:&str,bytes:Tracked<Vec<u8>>,proof:Option<TileProof>)->Option<(TextureId,TextureInfo)>{
        let(w,h)=crate::cpu_image_compression::dimensions(&bytes)?;
        let cells=proof.as_ref()?.certificate_for_size(w,h)?;
        let info=TextureInfo{width:w,height:h};let id=TextureId(self.next_id);
        let started=Instant::now();
        let result=(||{
            let mut surface=PrivateSurface::new(w,h)?;
            let mut cursor=crate::cpu_image_compression::Restore::new();
            while !restore_step(surface.handle,&bytes,&mut cursor)?{}
            if unsafe{art3m1s_gxm_surface_publish_strided(surface.handle,id.0,cells.as_ptr(),cells.len())}<=0{return None;}
            surface.handle=0;Some(())
        })();
        let us=elapsed_us(started);self.timing.uploads+=1;self.timing.upload_us+=us;self.timing.upload_max_us=self.timing.upload_max_us.max(us);
        if result.is_none(){self.timing.upload_errors+=1;self.defer_upload(((w as usize+7)&!7)*h as usize*4);self.keep_encoded(name,bytes,proof);return None;}
        self.next_id+=1;self.revision=self.revision.wrapping_add(1).max(1);
        let opaque=proof.as_ref().and_then(|p|p.opaque_for_size(w,h)).unwrap_or(false);
        self.entries.insert(name.into(),Entry{native_bytes:0,bc3:false,alpha_only:false,gray:false,id,info,
            rgba:Tracked::bytes(Vec::new(),Owner::Provider),opaque,revision:self.revision,last_used:self.cache_clock,cacheable:true,reclaimable:false,shared:true});
        self.ids.insert(id,name.into());
        crate::core_info!("GXM zero-span-hit name={} size={}x{} packed={} upload_us={}",name,w,h,bytes.len(),us);
        self.keep_encoded(name,bytes,proof);Some((id,info))
    }
}
