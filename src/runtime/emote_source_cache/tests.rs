use super::*;
use crate::image_cache_budget::CacheBudget;
#[test]fn pressure_deferred_prefetch_retries_after_idle_release_and_cancel_drops_the_job(){
    let budget=CacheBudget::new(100);let c=Cache::new(budget.clone(),64);
    budget.lock().unwrap().set_idle(100);c.plan(&["next".into()]);
    assert!(c.process_one_with(|c,_,_|{
        assert!(c.reserve(24).is_none());Err("model prefetch deferred: cache pressure".into())
    }));
    {let s=c.state.lock().unwrap();assert_eq!(s.queue.len(),1);assert!(!s.pending["next"].active);}
    budget.lock().unwrap().set_idle(76);
    assert!(!c.process_one_with(|c,_,_|Ok(source(c,24))));
    {let s=c.state.lock().unwrap();assert!(s.queue.is_empty());assert!(s.pending.is_empty());assert!(s.entries.contains_key("next"));}
    c.plan(&["cancelled".into()]);
    c.process_one_with(|c,_,_|{c.plan(&[]);Err("model prefetch deferred: cache pressure".into())});
    assert!(c.state.lock().unwrap().queue.is_empty());
    c.read_with("next",|_,_,_|panic!("recovered prefetch must be a hit")).unwrap();
}
#[test]fn model_admission_waits_for_idle_without_evicting_cached_psb_or_overspending(){
    let budget=CacheBudget::new(100);let c=Cache::new(budget.clone(),64);
    drop(c.read_with("cached",|c,_,_|Ok(source(c,24))).unwrap());
    {let mut b=budget.lock().unwrap();b.set_ready(16);b.set_idle(60);}
    assert!(c.reserve(16).is_none());
    assert!(c.state.lock().unwrap().entries.contains_key("cached"));
    {let mut b=budget.lock().unwrap();assert_eq!(b.emote,24);assert_eq!(b.idle,60);
        assert_eq!(b.idle_limit(100),44);b.set_idle(44);}
    let admitted=c.reserve(16).unwrap();
    {let b=budget.lock().unwrap();assert_eq!(b.ready+b.idle+b.emote,100);}
    assert!(c.state.lock().unwrap().entries.contains_key("cached"));drop(admitted);
}
#[test]fn image_scratch_does_not_discard_psb_while_idle_can_be_reclaimed(){
    let budget=CacheBudget::new(100);let c=Cache::new(budget.clone(),64);
    drop(c.read_with("model",|c,_,_|Ok(source(c,24))).unwrap());
    {let mut b=budget.lock().unwrap();b.set_ready(60);b.set_idle(16);}
    {let mut s=c.state.lock().unwrap();c.evict_locked(&mut s,24,false);assert!(s.entries.contains_key("model"));}
    budget.lock().unwrap().set_idle(0);
    {let mut s=c.state.lock().unwrap();c.evict_locked(&mut s,24,false);assert!(s.entries.is_empty());}
}
fn source(c:&Cache,size:usize)->Arc<Source>{Arc::new(Source{bytes:Arc::new(vec![0;size]),_reservation:c.reserve(size),_charge:Charge::observed(Owner::Source,size),parsed:Mutex::new(None)})}
#[test]fn repeated_load_shares_bytes_and_pressure_does_not_free_a_live_upload(){
    let budget=CacheBudget::new(80);let c=Cache::new(budget.clone(),64);
    let a=c.read_with("models\\one.psb",|c,_,_|Ok(source(c,32))).unwrap();
    let b=c.read_with("models/one.psb",|_,_,_|panic!("cache hit must not read")).unwrap();
    assert!(Arc::ptr_eq(&a,&b));assert_eq!(budget.lock().unwrap().emote,32);
    c.clear();assert_eq!(budget.lock().unwrap().emote,32);
    drop(a);assert_eq!(budget.lock().unwrap().emote,32);
    drop(b);assert_eq!(budget.lock().unwrap().emote,0);
}
#[test]fn oldest_unused_source_is_evicted_but_shared_budget_is_respected(){
    let budget=CacheBudget::new(100);let c=Cache::new(budget.clone(),64);
    drop(c.read_with("a",|c,_,_|Ok(source(c,32))).unwrap());
    drop(c.read_with("b",|c,_,_|Ok(source(c,32))).unwrap());
    drop(c.read_with("a",|_,_,_|panic!()).unwrap());
    drop(c.read_with("c",|c,_,_|Ok(source(c,32))).unwrap());
    let s=c.state.lock().unwrap();assert!(s.entries.contains_key("a"));assert!(!s.entries.contains_key("b"));drop(s);
    {let mut b=budget.lock().unwrap();b.set_ready(36);assert_eq!(b.ready_limit(),36);assert_eq!(b.idle_limit(100),0);}
    assert!(c.reserve(1).is_some()); // Reclaims an unleased entry instead of overspending.
    let b=budget.lock().unwrap();assert!(b.ready+b.idle+b.emote<=b.limit);
}
#[test]fn cancelled_plan_does_not_publish_a_late_result(){
    let c=Cache::new(CacheBudget::new(128),64);c.plan(&["a".into(),"b".into(),"c".into()]);
    let ticket=c.state.lock().unwrap().pending["a"].ticket;
    let bytes=source(&c,32);c.plan(&["c".into()]);assert!(!c.valid("a",ticket));
    drop(c.finish("a",ticket,Ok(bytes),true).unwrap());
    let s=c.state.lock().unwrap();assert!(!s.entries.contains_key("a"));assert_eq!(s.plan,["c"]);
}
#[test]fn image_headroom_is_not_limited_by_the_model_subquota(){
    let budget=CacheBudget::new(192);let c=Cache::new(budget.clone(),32);
    drop(c.read_with("a",|c,_,_|Ok(source(c,24))).unwrap());
    {let mut s=c.state.lock().unwrap();c.evict_locked(&mut s,64,false);assert!(s.entries.contains_key("a"));}
    budget.lock().unwrap().set_ready(120);
    {let mut s=c.state.lock().unwrap();c.evict_locked(&mut s,64,false);assert!(s.entries.is_empty());}
    assert_eq!(budget.lock().unwrap().emote,0);
}
#[test]fn active_prefetch_is_shared_with_demand_and_failure_can_retry(){
    let c=Arc::new(Cache::new(CacheBudget::new(128),64));c.plan(&["a".into()]);
    let ticket={let mut s=c.state.lock().unwrap();let p=s.pending.get_mut("a").unwrap();p.active=true;p.ticket};
    let wait=c.clone();let consumer=std::thread::spawn(move||wait.read_with("a",|_,_,_|panic!("must join existing read")).unwrap());
    while !c.state.lock().unwrap().pending["a"].demanded{std::thread::yield_now();}
    let bytes=source(&c,32);drop(c.finish("a",ticket,Ok(bytes.clone()),true).unwrap());
    assert!(Arc::ptr_eq(&consumer.join().unwrap(),&bytes));
    assert!(c.read_with("bad",|_,_,_|Err("short read".into())).is_err());
    assert!(c.read_with("bad",|c,_,_|Ok(source(c,16))).is_ok());
}

#[test]
#[ignore = "requires EMOTE_CACHE_TEST_MODEL external fixture"]
fn parsed_data_is_shared_but_players_are_independent_and_leases_survive_eviction(){
    let bytes=std::fs::read(std::env::var("EMOTE_CACHE_TEST_MODEL").unwrap()).unwrap();
    let budget=CacheBudget::new(64*1024*1024);let c=Cache::new(budget.clone(),32*1024*1024);
    let raw_size=bytes.capacity();
    let a=c.read_with("model",|c,_,_|Ok(Arc::new(Source{bytes:Arc::new(bytes),_reservation:c.reserve(raw_size),
        _charge:Charge::observed(Owner::Source,raw_size),parsed:Mutex::new(None)}))).unwrap();
    let first=c.model(&a,"model").unwrap();let second=c.model(&a,"model").unwrap();
    assert!(Arc::ptr_eq(&first,&second));assert!(first.source_document().is_none());
    assert_eq!(Arc::strong_count(&a.bytes),1); // Cached model owns no texture buffer.
    let views=first.texture_data(&a).unwrap();assert!(!views.is_empty());drop(views);
    let mut one=art3m1s_emote::EmotePlayer::default();let two=art3m1s_emote::EmotePlayer::default();
    let before=format!("{two:?}");one.set_variable("face_talk",5.0,0.0,0);
    assert_eq!(format!("{two:?}"),before);assert_ne!(format!("{one:?}"),before);
    assert_eq!(budget.lock().unwrap().emote,raw_size+first.bytes);
    c.clear();drop(a);assert_eq!(budget.lock().unwrap().emote,first.bytes);
    drop(first);assert!(budget.lock().unwrap().emote>0);drop(second);
    assert_eq!(budget.lock().unwrap().emote,0);assert_eq!(c.parsed_live.load(Ordering::Relaxed),0);
}

#[test]
#[ignore = "requires EMOTE_CACHE_TEST_MODELS external fixture directory"]
fn eight_models_share_parsed_hits_and_evict_within_the_combined_quota(){
    let root=std::path::PathBuf::from(std::env::var("EMOTE_CACHE_TEST_MODELS").unwrap());
    let budget=CacheBudget::new(192*1024*1024);let c=Cache::new(budget.clone(),LIMIT);
    for i in 1..=8 {
        let path=format!("model{i:02}.psb");let bytes=std::fs::read(root.join(&path)).unwrap();
        let size=bytes.capacity();
        let raw=c.read_with(&path,|c,_,_|Ok(Arc::new(Source{bytes:Arc::new(bytes),_reservation:c.reserve(size),
            _charge:Charge::observed(Owner::Source,size),parsed:Mutex::new(None)}))).unwrap();
        let first=c.model(&raw,&path).unwrap();drop(raw);
        let hit=c.read_with(&path,|_,_,_|panic!("repeat must use retained bytes")).unwrap();
        let second=c.model(&hit,&path).unwrap();assert!(Arc::ptr_eq(&first,&second));
        assert!(budget.lock().unwrap().emote<=LIMIT);
    }
    {let s=c.state.lock().unwrap();assert!(s.evictions>0);assert!(!s.entries.contains_key("model01.psb"));}
    assert_eq!(c.parsed_hits.load(Ordering::Relaxed),8);
    c.clear();assert_eq!(budget.lock().unwrap().emote,0);assert_eq!(c.parsed_live.load(Ordering::Relaxed),0);
}

#[test]
fn parse_scratch_yields_to_published_data_and_transfers_without_double_charging(){
    let budget=CacheBudget::new(100);let c=Cache::new(budget.clone(),32);
    let raw=source(&c,24);
    {let mut b=budget.lock().unwrap();b.set_ready(40);b.set_idle(36);}
    assert!(c.reserve_scratch(20).is_none());
    {let mut b=budget.lock().unwrap();assert_eq!(b.idle_limit(100),16);b.set_idle(16);}
    let mut scratch=Some(c.reserve_scratch(20).unwrap());
    {let b=budget.lock().unwrap();assert_eq!(b.emote_scratch,20);assert_eq!(b.ready_limit(),40);assert_eq!(b.emote,24);}
    // The temporary reservation can exceed the remaining model subquota.
    let parsed=c.reserve_with_credit(8,&mut scratch).unwrap();
    {let b=budget.lock().unwrap();assert_eq!(b.emote,32);assert_eq!(b.emote_scratch,0);assert_eq!(b.ready+b.idle+b.emote,88);}
    drop(scratch);drop(parsed);drop(raw);assert_eq!(budget.lock().unwrap().emote,0);
}

fn fixture_source(c:&Cache,path:&std::path::Path)->Arc<Source>{
    let bytes=std::fs::read(path).unwrap();let size=bytes.capacity();
    Arc::new(Source{bytes:Arc::new(bytes),_reservation:Some(c.reserve(size).unwrap()),
        _charge:Charge::observed(Owner::Source,size),parsed:Mutex::new(None)})
}
#[test]
#[ignore = "requires EMOTE_CACHE_TEST_MODELS external fixture directory"]
fn prefetch_parses_eight_models_before_first_demand_and_keeps_quota(){
    let root=std::path::PathBuf::from(std::env::var("EMOTE_CACHE_TEST_MODELS").unwrap());
    let budget=CacheBudget::new(192*1024*1024);let c=Cache::new(budget.clone(),LIMIT);
    for i in 1..=8 {
        let path=format!("model{i:02}.psb");c.plan(&[path.clone()]);
        assert!(!c.process_one_with(|c,p,_|Ok(fixture_source(c,&root.join(p)))));
        let raw=c.read_with(&path,|_,_,_|panic!("preparsed demand must not read")).unwrap();
        let before=c.parsed_misses.load(Ordering::Relaxed);
        let first=c.model(&raw,&path).unwrap();assert!(first.source_document().is_none());
        assert_eq!(c.parsed_misses.load(Ordering::Relaxed),before);
        // A warm hit must not wait behind another model's parsing gate.
        let gate=c.parse_gate.lock().unwrap();let second=c.model(&raw,&path).unwrap();drop(gate);
        assert!(Arc::ptr_eq(&first,&second));assert_eq!(Arc::strong_count(&raw.bytes),1);
        let b=budget.lock().unwrap();assert_eq!(b.emote_scratch,0);assert!(b.emote<=LIMIT);
    }
    assert_eq!(c.parsed_misses.load(Ordering::Relaxed),8);assert_eq!(c.parsed_hits.load(Ordering::Relaxed),16);
    c.clear();assert_eq!(budget.lock().unwrap().emote,0);assert_eq!(c.parsed_live.load(Ordering::Relaxed),0);
}
#[test]
#[ignore = "requires EMOTE_CACHE_TEST_MODEL external fixture"]
fn preparse_pressure_keeps_bytes_for_retry_demand_and_cancellation(){
    let path=std::path::PathBuf::from(std::env::var("EMOTE_CACHE_TEST_MODEL").unwrap());
    let budget=CacheBudget::new(64*1024*1024);let c=Cache::new(budget.clone(),LIMIT);
    c.plan(&["retry".into()]);
    assert!(c.process_one_with(|c,_,_|{
        let source=fixture_source(c,&path);let mut b=c.budget.lock().unwrap();
        let idle=b.limit-b.emote;b.set_idle(idle);Ok(source)
    }));
    assert_eq!(c.parsed_misses.load(Ordering::Relaxed),0);
    assert!(c.state.lock().unwrap().pending["retry"].source.is_some());
    budget.lock().unwrap().set_idle(0);
    assert!(!c.process_one_with(|_,_,_|panic!("retry must reuse read bytes")));
    let ready=c.read_with("retry",|_,_,_|panic!()).unwrap();assert!(ready.parsed.lock().unwrap().is_some());drop(ready);c.clear();
    c.plan(&["demand".into()]);
    assert!(c.process_one_with(|c,_,_|{
        let source=fixture_source(c,&path);let mut b=c.budget.lock().unwrap();let idle=b.limit-b.emote;b.set_idle(idle);Ok(source)
    }));
    let raw=c.read_with("demand",|_,_,_|panic!("demand must reuse deferred bytes")).unwrap();
    assert!(c.state.lock().unwrap().pending.is_empty());assert!(c.state.lock().unwrap().queue.is_empty());
    drop(raw);c.clear();budget.lock().unwrap().set_idle(0);
    c.plan(&["cancel".into()]);
    assert!(c.process_one_with(|c,_,_|{
        let source=fixture_source(c,&path);let mut b=c.budget.lock().unwrap();let idle=b.limit-b.emote;b.set_idle(idle);Ok(source)
    }));
    c.plan(&[]);assert_eq!(budget.lock().unwrap().emote,0);assert_eq!(budget.lock().unwrap().emote_scratch,0);
    assert!(!c.process_one_with(|_,_,_|panic!("cancelled work must not run")));
}
