use super::*;
const SPRITE_PREFIX:&str="__sparse_sprite__/";
pub(super) fn sprite_key(name:&str)->String{format!("{SPRITE_PREFIX}{name}")}
pub(super) fn source_name(name:&str)->&str{name.strip_prefix(SPRITE_PREFIX).unwrap_or(name)}
pub(super) fn allocation_bytes(info:TextureInfo)->usize{
    (((info.width as usize+7)&!7)*info.height as usize*4).div_ceil(256*1024)*(256*1024)
}
unsafe extern "C"{
    fn art3m1s_gxm_sparse_write(handle:usize,offset:usize,source:*const u8,len:usize)->i32;
    fn art3m1s_gxm_surface_publish_strided(handle:usize,id:u64,proof:*const u8,count:usize)->i32;
}
pub(super) fn restore_step(handle:usize,bytes:&[u8],cursor:&mut crate::cpu_image_compression::Restore)->Option<bool>{
    cursor.step(bytes,&mut|offset,source,len|unsafe{art3m1s_gxm_sparse_write(handle,offset,source.map_or(std::ptr::null(),|p|p.as_ptr()),len)>0})
}
// Intersect the validated stream spans with the source rectangle. Addresses
// sent to sparse_write are packed crop coordinates; the host handles stride.
// No full-size RGBA allocation, scan, or CDRAM readback is needed.
pub(super) fn restore_crop_step(handle:usize,bytes:&[u8],cursor:&mut crate::cpu_image_compression::Restore,rect:[u32;4])->Option<bool>{
    let (width,height)=crate::cpu_image_compression::dimensions(bytes)?;
    let [x,y,w,h]=rect;
    if w==0||h==0||x.checked_add(w)?>width||y.checked_add(h)?>height{return None;}
    let row=width as usize*4;let crop_row=w as usize*4;
    cursor.step(bytes,&mut|offset,source,len|{
        let end=offset+len;
        let first=(offset/row).max(y as usize);
        let last=((end-1)/row+1).min((y+h)as usize);
        for line in first..last{
            let left=line*row+x as usize*4;
            let start=offset.max(left);let stop=end.min(left+crop_row);
            if start>=stop{continue;}
            let target=(line-y as usize)*crop_row+start-left;
            let src=source.map_or(std::ptr::null(),|p|unsafe{p.as_ptr().add(start-offset)});
            if unsafe{art3m1s_gxm_sparse_write(handle,target,src,stop-start)}<=0{return false;}
        }
        true
    })
}
impl GxmTextureProvider{
    pub(super) fn upload_sparse(&mut self,name:&str,bytes:Tracked<Vec<u8>>,proof:Option<TileProof>)->Option<(TextureId,TextureInfo)>{
        let(w,h)=crate::cpu_image_compression::dimensions(&bytes)?;
        proof.as_ref()?.certificate_for_size(w,h)?;
        let original=TextureInfo{width:w,height:h};
        let crop=if self.resolving_sprite{proof.as_ref()?.sprite_crop()}else{None};
        let cropped_proof=crop.and_then(|r|proof.as_ref()?.cropped_sprite_proof(r));
        let crop=crop.filter(|_|cropped_proof.is_some());
        let info=crop.map_or(original,|[_,_,width,height]|TextureInfo{width,height});
        let key=crop.map_or_else(||name.to_owned(),|_|sprite_key(name));
        let cells=cropped_proof.as_ref().or(proof.as_ref())?.certificate_for_size(info.width,info.height)?;
        let id=TextureId(self.next_id);
        let started=Instant::now();
        let result=(||{
            let mut surface=PrivateSurface::new(info.width,info.height).ok_or("allocate")?;
            let mut cursor=crate::cpu_image_compression::Restore::new();
            loop{
                let done=if let Some(rect)=crop{restore_crop_step(surface.handle,&bytes,&mut cursor,rect)}
                    else{restore_step(surface.handle,&bytes,&mut cursor)}.ok_or("restore")?;
                if done{break;}
            }
            if unsafe{art3m1s_gxm_surface_publish_strided(surface.handle,id.0,cells.as_ptr(),cells.len())}<=0{return Err("publish");}
            surface.handle=0;Ok(())
        })();
        let us=elapsed_us(started);self.timing.uploads+=1;self.timing.upload_us+=us;self.timing.upload_max_us=self.timing.upload_max_us.max(us);
        let failure_key=format!("sparse-upload:{}",name);
        if let Err(stage)=result{
            self.timing.upload_errors+=1;
            let requested=allocation_bytes(info);
            if self.reported_failures.insert(failure_key){crate::core_warn!("GXM sparse-upload-deferred name={} stage={} size={}x{} requested={} packed={}",name,stage,info.width,info.height,requested,bytes.len());}
            self.defer_upload(requested);self.keep_encoded(name,bytes,proof);return None;
        }
        self.reported_failures.remove(&failure_key);
        self.next_id+=1;self.revision=self.revision.wrapping_add(1).max(1);
        let opaque=cropped_proof.as_ref().or(proof.as_ref()).and_then(|p|p.opaque_for_size(info.width,info.height)).unwrap_or(false);
        self.entries.insert(key.clone(),Entry { sprite_crop:crop.map(|r|(original,r)),native_bytes:0,bc3:false,alpha_only:false,gray:false,id,info,
            rgba:Tracked::bytes(Vec::new(),Owner::Provider),opaque,revision:self.revision,last_used:self.cache_clock,cacheable:true,reclaimable:false,shared:true});
        self.ids.insert(id,key);
        crate::core_info!("GXM zero-span-hit name={} size={}x{} gpu={}x{} crop={:?} packed={} upload_us={}",name,w,h,info.width,info.height,crop,bytes.len(),us);
        self.keep_encoded(name,bytes,proof);Some((id,info))
    }
}
