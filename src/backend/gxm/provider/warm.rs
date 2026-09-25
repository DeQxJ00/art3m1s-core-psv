//! Bounded render-thread upload of already-decoded story images. Staging is
//! private until every row is present; current/transition textures stay pinned.
use super::*;
use crate::image_cache_budget::CacheParts;

const COPY_BYTES: usize = 128 * 1024;
const PLAN_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

unsafe extern "C" {
    fn art3m1s_gxm_surface_warm_allowed(bytes: usize) -> i32;
    fn art3m1s_gxm_surface_publish_strided(handle: usize, id: u64, proof: *const u8, count: usize) -> i32;
}

pub(super) struct WarmUpload {
    pub(super) name: String,
    info: TextureInfo,
    pixels: Tracked<Vec<u8>>,
    proof: Option<TileProof>,
    source: Option<Tracked<Vec<u8>>>,
    surface: Option<PrivateSurface>,
    row: usize,
    steps: usize,
    work_us: u64,
    max_us: u64,
}
impl WarmUpload {
    fn gpu_bytes(&self) -> usize { ((self.info.width as usize + 7) & !7) * self.info.height as usize * 4 }
    fn parts(&self) -> CacheParts {
        CacheParts { decoded: self.pixels.capacity(), encoded: self.source.as_ref().map_or(0, |p|p.capacity()),
            proof: self.proof.as_ref().map_or(0, TileProof::bytes),
            gpu: if self.surface.is_some() { self.gpu_bytes() } else { 0 } }
    }
}

#[derive(Default)]
pub(super) struct WarmState {
    prefetch: Option<PrefetchSource>,
    plan: Vec<String>,
    cursor: usize,
    tick: usize,
    admitted: usize,
    pub(super) job: Option<WarmUpload>,
}
impl WarmState {
    pub(super) fn parts(&self) -> CacheParts { self.job.as_ref().map_or_else(CacheParts::default, WarmUpload::parts) }
}

impl GxmTextureProvider {
    /// Callback must atomically transfer the ready payload's accounting to
    /// idle, and must neither wait nor perform source reads/decodes.
    pub(crate) fn with_warm_prefetch(mut self, source: impl Fn(&str) -> Option<PreparedImage> + 'static) -> Self {
        self.warm.prefetch = Some(Box::new(source)); self
    }
    pub fn set_warm_plan(&mut self, paths: &[String]) {
        self.warm.plan = paths.iter().filter(|p| !crate::ui_image_lifetime::transient_menu_image(p))
            .take(24).cloned().collect();
        self.warm.cursor = 0; self.warm.admitted = 0;
        if self.warm.job.as_ref().is_some_and(|j| !self.warm.plan.contains(&j.name)) { self.cancel_warm_upload(); }
    }
    fn update_warm_account(&self) {
        if let Some(b) = &self.cache_budget { b.lock().unwrap().set_idle(self.idle_parts().total()); }
    }
    pub(super) fn cancel_warm_upload(&mut self) {
        let Some(job) = self.warm.job.take() else { return; };
        let WarmUpload {name, info, pixels, proof, source, surface, ..} = job;
        drop(surface); // unpublished memory has no GPU readers
        self.decoded.insert(name.clone(), DecodedEntry {gray:false,info,rgba:pixels,last_used:self.cache_clock});
        if source.is_some() || proof.is_some() {
            self.encoded.insert(name, EncodedEntry {bytes:source.unwrap_or_else(||Tracked::bytes(Vec::new(),Owner::Provider)),proof,last_used:self.cache_clock});
        }
        self.update_warm_account();
    }
    pub(super) fn cancel_warm_under_pressure(&mut self) {
        if self.warm.job.is_none() { return; }
        let parts = self.idle_parts();
        let pressure = parts.gpu > IDLE_GPU_BUDGET || self.cache_budget.as_ref().is_some_and(|b| {
            parts.total() > b.lock().unwrap().idle_limit(self.idle_budget)
        });
        if pressure { self.cancel_warm_upload(); }
    }
    pub fn warm_step(&mut self) {
        self.cancel_warm_under_pressure();
        if self.warm.job.is_none() {
            self.warm.tick = self.warm.tick.wrapping_add(1);
            if self.warm.tick % 4 != 0 || self.warm.plan.is_empty() || self.warm.admitted >= PLAN_BYTES { return; }
            let i = self.warm.cursor % self.warm.plan.len(); self.warm.cursor += 1;
            let name = self.warm.plan[i].clone();
            if self.entries.contains_key(&name) || self.decoded.contains_key(&name) { return; }
            // No cache eviction or GPU fence to make speculative space.
            if self.idle_parts().gpu + MAX_IMAGE_BYTES > IDLE_GPU_BUDGET
                || unsafe { art3m1s_gxm_surface_warm_allowed(MAX_IMAGE_BYTES) } <= 0 { return; }
            let Some((Ok(PreparedPixels::Rgba(p)), proof, source)) = self.warm.prefetch.as_ref().and_then(|f| f(&name)) else { return; };
            let info = TextureInfo {width:p.width(),height:p.height()};
            let mut pixels = p.into_raw(); pixels.transfer(Owner::Provider);
            let mut proof=proof; if let Some(p)=proof.as_mut(){p.transfer(Owner::Provider);}
            let mut source=source; if let Some(p)=source.as_mut(){p.transfer(Owner::Provider);}
            self.warm.job = Some(WarmUpload { name, info, pixels, proof, source, surface:None, row:0,steps:0,work_us:0,max_us:0 });
            self.update_warm_account();
            return; // acquiring READY and allocating GPU memory use separate frames
        }
        let started = Instant::now();
        if self.warm.job.as_ref().unwrap().surface.is_none() {
            let gpu = self.warm.job.as_ref().unwrap().gpu_bytes();
            let cache = self.cache_budget.clone();
            let mut account = cache.as_ref().map(|b|b.lock().unwrap());
            let parts = self.idle_parts();
            if gpu > MAX_IMAGE_BYTES || self.warm.admitted + gpu > PLAN_BYTES
                || parts.gpu + gpu > IDLE_GPU_BUDGET
                || account.as_ref().is_none_or(|b|parts.total()+gpu > b.idle_limit(self.idle_budget))
                || unsafe { art3m1s_gxm_surface_warm_allowed(gpu) } <= 0 {
                drop(account); self.cancel_warm_upload(); return;
            }
            let job = self.warm.job.as_mut().unwrap();
            job.surface = PrivateSurface::new(job.info.width,job.info.height);
            if job.surface.is_none() { drop(account); self.cancel_warm_upload(); return; }
            self.warm.admitted += gpu;
            if let Some(b)=account.as_mut(){b.set_idle(parts.total()+gpu);}
        } else if self.warm.job.as_ref().unwrap().row < self.warm.job.as_ref().unwrap().info.height as usize {
            let job = self.warm.job.as_mut().unwrap();
            let w = job.info.width as usize; let stride = (w+7)&!7;
            let end = (job.row + (COPY_BYTES/(stride*4)).max(1)).min(job.info.height as usize);
            let target = job.surface.as_mut().unwrap().bytes();
            for y in job.row..end {
                let src = &job.pixels[y*w*4..(y+1)*w*4];
                let dst = &mut target[y*stride*4..(y+1)*stride*4];
                dst[..w*4].copy_from_slice(src);
                for pad in dst[w*4..].chunks_exact_mut(4) {pad.copy_from_slice(&src[(w-1)*4..]);}
                job.row=y+1;
                if y%8==7 && elapsed_us(started)>=750 {break;}
            }
        } else {
            let job = self.warm.job.as_mut().unwrap();
            let Some(cells)=job.proof.as_ref().and_then(|p|p.certificate_for_size(job.info.width,job.info.height)) else {
                self.cancel_warm_upload(); return;
            };
            let id = TextureId(self.next_id);
            if unsafe { art3m1s_gxm_surface_publish_strided(job.surface.as_ref().unwrap().handle,id.0,cells.as_ptr(),cells.len()) } <= 0 {
                self.cancel_warm_upload(); return;
            }
            job.surface.as_mut().unwrap().handle=0;
            let job = self.warm.job.take().unwrap();
            self.next_id += 1; self.revision = self.revision.wrapping_add(1).max(1);
            let opaque=job.proof.as_ref().and_then(|p|p.opaque_for_size(job.info.width,job.info.height)).unwrap_or(false);
            self.entries.insert(job.name.clone(),Entry {id,info:job.info,rgba:job.pixels,opaque,revision:self.revision,
                last_used:self.cache_clock,cacheable:true,reclaimable:true,shared:true,gray:false,alpha_only:false});
            self.ids.insert(id,job.name.clone());
            // Keep the delivered source under the existing compressed cap.
            if let Some(source)=job.source {self.keep_encoded(&job.name,source,job.proof);}
            self.update_warm_account();
            crate::core_info!("GXM warm-upload name={} size={}x{} steps={} work_us={} max_step_us={} bytes_per_step={}",
                job.name,job.info.width,job.info.height,job.steps+1,job.work_us+elapsed_us(started),job.max_us.max(elapsed_us(started)),COPY_BYTES);
            return;
        }
        let us=elapsed_us(started);
        let job=self.warm.job.as_mut().unwrap();job.steps+=1;job.work_us+=us;job.max_us=job.max_us.max(us);
    }
}
