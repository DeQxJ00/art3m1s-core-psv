use super::*;
fn cache(groups:usize,bytes:usize)->Cache{let c=Cache::new(crate::image_cache_budget::CacheBudget::new(32*MIB));c.state.lock().unwrap().policy=Policy{enabled:true,groups,bytes};c}
fn files(color:usize,mask:usize)->impl Fn(&str)->Option<usize>{move|p|{if p.ends_with("_m.ogv"){(mask>0).then_some(mask)}else{Some(color)}}}
fn read(_p:&str,out:&mut[u8],offset:usize)->bool{out.fill(17);if offset==0{out[..4].copy_from_slice(b"OggS");}true}
fn finish(c:&Cache,color:usize,mask:usize){for _ in 0..100{c.process_with(files(color,mask),read);if c.state.lock().unwrap().queue.is_empty(){return;}}panic!("queue did not complete");}
#[test]fn paired_files_over_old_limit_publish_together_and_replay_without_copy(){
    let c=cache(4,16*MIB);c.plan(&["image/anime/fx.ogv".into()]);
    c.process_with(files(2*MIB,3*MIB),read);
    assert_eq!(c.budget.lock().unwrap().video,5*MIB);assert_eq!(c.groups.load(Ordering::Relaxed),1);
    assert!(c.acquire("image/anime/fx.ogv").is_none()); // partial color not a hit
    finish(&c,2*MIB,3*MIB);let a=c.acquire("image/anime/fx.ogv").unwrap();let b=c.acquire("image\\anime\\FX.OGV").unwrap();
    assert!(Arc::ptr_eq(&a,&b));assert_eq!(a.mask.len(),3*MIB);
    drop(a);drop(b);assert_eq!(c.groups.load(Ordering::Relaxed),1);
    c.clear();assert_eq!(c.budget.lock().unwrap().video,0);
}
#[test]fn both_limits_include_leases_and_pending_groups(){
    let c=cache(1,1000);c.plan(&["a.ogv".into()]);finish(&c,400,400);let a=c.acquire("a.ogv").unwrap();
    c.plan(&["b.ogv".into()]);assert!(c.process_with(files(10,0),read));assert_eq!(c.groups.load(Ordering::Relaxed),1);
    drop(a);finish(&c,10,0);assert!(!c.state.lock().unwrap().entries.contains_key("a.ogv"));assert_eq!(c.budget.lock().unwrap().video,10);
    let d=cache(4,1000);d.plan(&["a.ogv".into(),"b.ogv".into()]);d.process_with(files(400,400),read);
    assert!(d.process_with(files(400,400),read));assert_eq!(d.budget.lock().unwrap().video,800);assert_eq!(d.groups.load(Ordering::Relaxed),1);
}
#[test]fn oversized_missing_or_broken_mask_never_publishes_color_only(){
    let c=cache(4,1024);c.plan(&["large.ogv".into()]);c.process_with(files(800,800),read);
    assert!(c.state.lock().unwrap().entries.is_empty());assert_eq!(c.budget.lock().unwrap().video,0);
    c.plan(&["bad.ogv".into()]);c.process_with(files(400,400),|p,o,n|!p.ends_with("_m.ogv")&&read(p,o,n));
    assert!(c.state.lock().unwrap().entries.is_empty());assert_eq!(c.budget.lock().unwrap().video,0);
    c.plan(&["solo.ogv".into()]);finish(&c,400,0);assert!(c.acquire("solo.ogv").unwrap().mask.is_empty());
}
#[test]fn branch_change_and_game_reset_cancel_partial_io_without_invalidating_live_views(){
    let c=cache(4,8*MIB);c.plan(&["old.ogv".into()]);c.process_with(files(MIB,MIB),read);
    c.plan(&["new.ogv".into()]);assert_eq!(c.budget.lock().unwrap().video,0);finish(&c,20,20);
    let data=c.acquire("new.ogv").unwrap();c.clear();assert_eq!(c.budget.lock().unwrap().video,40);assert_eq!(&data.color[..4],b"OggS");
    drop(data);assert_eq!(c.budget.lock().unwrap().video,0);assert_eq!(c.groups.load(Ordering::Relaxed),0);
}
#[test]fn concurrent_cancel_discards_late_result(){
    let c=cache(4,8*MIB);c.plan(&["old.ogv".into()]);
    c.process_with(files(40,40),|p,o,n|{c.clear();read(p,o,n)});
    assert!(c.state.lock().unwrap().entries.is_empty());assert_eq!(c.budget.lock().unwrap().video,0);
}
#[test]fn idle_headroom_is_requested_but_never_spent_before_reclaimed(){
    let c=cache(4,8*MIB);{let mut b=c.budget.lock().unwrap();b.set_ready(24*MIB);b.set_idle(8*MIB);}
    c.plan(&["a.ogv".into()]);assert!(c.process_with(files(MIB,MIB),read));
    {let mut b=c.budget.lock().unwrap();assert_eq!(b.video,0);assert_eq!(b.idle_limit(8*MIB),6*MIB);b.set_idle(6*MIB);}
    finish(&c,MIB,MIB);{let b=c.budget.lock().unwrap();assert_eq!(b.video+b.ready+b.idle,b.limit);assert_eq!(b.ready_limit(),24*MIB);}
    c.reclaim(8*MIB);assert_eq!(c.budget.lock().unwrap().video,0);
}
#[test]fn disabled_policy_never_allocates_and_round_robin_read_is_bounded(){
    let c=cache(4,8*MIB);c.state.lock().unwrap().policy.enabled=false;c.plan(&["a.ogv".into()]);assert!(c.acquire("a.ogv").is_none());assert!(c.state.lock().unwrap().jobs.is_empty());
    c.state.lock().unwrap().policy.enabled=true;c.plan(&["a.ogv".into()]);let count=AtomicUsize::new(0);
    c.process_with(files(MIB,MIB),|p,o,n|{count.fetch_add(o.len(),Ordering::Relaxed);read(p,o,n)});assert_eq!(count.load(Ordering::Relaxed),QUANTUM);
}
#[test]fn playback_promotes_current_group_over_earlier_hints_even_in_same_block(){
    let c=cache(1,1024);c.plan(&["first.ogv".into(),"second.ogv".into()]);
    c.process_with(files(400,400),read);assert!(c.process_with(files(400,400),read));
    assert!(c.acquire("second.ogv").is_none());c.process_with(files(400,400),read);
    let g=c.acquire("second.ogv").unwrap();assert_eq!(g.color.len(),400);assert_eq!(c.groups.load(Ordering::Relaxed),1);
    assert!(!c.state.lock().unwrap().entries.contains_key("first.ogv"));
}
