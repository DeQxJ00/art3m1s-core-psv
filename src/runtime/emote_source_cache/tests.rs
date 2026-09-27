use super::*;
use crate::image_cache_budget::CacheBudget;
fn source(c:&Cache,size:usize)->Arc<Source>{Arc::new(Source{bytes:Arc::new(vec![0;size]),_reservation:c.reserve(size),_charge:Charge::observed(Owner::Source,size)})}
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
