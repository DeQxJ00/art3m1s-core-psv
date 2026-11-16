//! Bounded deferred voice checks for an audited script callback.
use crate::tags::var_handler::SoundInfoSnapshot;
use crate::VariableStore;

pub(crate) const COMMIT_TAG: &str = "\u{b}voice_bgm_recovery";
const LABEL: &str = "bgm_fade_up";
const REGISTRY: &str = "art3m1s.voice_bgm_recovery";

struct Job { file: String, next: u32, last_frame: u64, ready: bool }
#[derive(Default)]
pub(crate) struct Scheduler { serial: u64, pending: Option<Job> }
impl Scheduler {
    pub(crate) fn request(&mut self, frame: u64, file: &str) {
        if self.pending.as_ref().is_some_and(|job| job.file == file) { return; }
        self.cancel();
        self.pending = Some(Job { file: file.into(), next: 100, last_frame: frame, ready: false });
    }
    pub(crate) fn is_pending(&self) -> bool { self.pending.is_some() }
    pub(crate) fn tick(&mut self, frame: u64, sound: &SoundInfoSnapshot, vars: &mut VariableStore) -> Option<u64> {
        let job = self.pending.as_mut()?;
        if job.ready || frame <= job.last_frame { return None; }
        job.last_frame = frame;
        let started = std::time::Instant::now();
        for _ in 0..3 {
            let id = format!("{}.0", job.next);
            let playing = sound.se.iter().any(|ch| ch.id == id && ch.playing);
            if playing && !is_stopping(vars, &id) { self.cancel(); return None; }
            if !playing {
                vars.with_local_writes(false, |vars| vars.delete_group(&format!("snd.se.{id}")));
            }
            job.next += 1;
            if job.next > 120 {
                job.ready = true;
                return Some(self.serial);
            }
            // Bound the number of costly absent-group deletions too. A single
            // deletion is atomic, but subsequent ones can wait for another tick.
            if started.elapsed().as_micros() >= 1_000 { break; }
        }
        None
    }
    pub(crate) fn finish(&mut self, token: u64, sound: &SoundInfoSnapshot, vars: &VariableStore) -> Option<String> {
        if token != self.serial || !self.pending.as_ref().is_some_and(|j| j.ready) { return None; }
        let job = self.pending.take()?;
        // Recheck the whole relevant range, including channels checked in an
        // earlier tick. No intermediate script variables are materialized.
        let blocked = sound.se.iter().any(|ch| ch.playing && relevant_voice(&ch.id) && !is_stopping(vars, &ch.id));
        (!blocked).then_some(job.file)
    }
    pub(crate) fn cancel(&mut self) { self.serial = self.serial.wrapping_add(1); self.pending = None; }
}

pub(crate) fn relevant_voice(id: &str) -> bool {
    id.strip_suffix(".0").and_then(|id| id.parse::<u32>().ok()).is_some_and(|n| (100..=120).contains(&n))
}

fn is_stopping(vars: &VariableStore, id: &str) -> bool {
    // The audited sound helper writes stop|pan|gain|loop|file. Missing or
    // malformed state must not allow a playing voice to raise the BGM gain.
    let Some(crate::variable::Value::String(flag)) = vars.get(&format!("snd.se.{id}")) else { return false; };
    let fields: Vec<_> = flag.splitn(5, '|').collect();
    fields.len() == 5 && fields[0] == "1" && !fields[1].is_empty() && !fields[2].is_empty() && fields[3].len() == 1
}

fn fingerprint(code: &str) -> u64 {
    code.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3))
}

/// Both complete Lua blocks must match the audited implementations. Names
/// alone are insufficient: projects can replace either helper independently.
pub(crate) fn after_block(lua: &mlua::Lua, file: &str, code: &str) -> mlua::Result<()> {
    let hash = fingerprint(code);
    if !matches!((code.len(), hash), (438, 0xb44f9511c1df8902) | (17153, 0x129ab4529bd49fd5)) { return Ok(()); }
    let state = lua.named_registry_value::<mlua::Table>(REGISTRY).unwrap_or(lua.create_table()?);
    if code.len() == 438 {
        state.set("original", lua.globals().get::<mlua::Function>("bgmFadeUpBacklog")?)?;
        state.set("file", file)?;
    } else {
        let filters = lua.globals().get::<mlua::Table>("tagFilters")?;
        state.set("helper", filters.get::<mlua::Function>("se_sound_info")?)?;
    }
    lua.set_named_registry_value(REGISTRY, state.clone())?;
    if let (Ok(original), Ok(helper), Ok(file)) = (
        state.get::<mlua::Function>("original"), state.get::<mlua::Function>("helper"), state.get::<String>("file"),
    ) { install(lua, original, helper, file)?; }
    Ok(())
}

fn install(lua: &mlua::Lua, original: mlua::Function, helper: mlua::Function, file: String) -> mlua::Result<()> {
    let state = lua.named_registry_value::<mlua::Table>(REGISTRY).unwrap_or(lua.create_table()?);
    state.set("helper", helper.clone())?;
    let wrapper = lua.create_function(move |lua, (engine, params): (mlua::AnyUserData, mlua::Value)| {
        let same_helper = lua.globals().get::<mlua::Table>("tagFilters").ok()
            .and_then(|t| t.get::<mlua::Function>("se_sound_info").ok())
            .is_some_and(|f| f.to_pointer() == helper.to_pointer());
        if same_helper {
            let api = engine.borrow::<crate::lua_engine::EngineApi>()?;
            let mut ctx = api.ctx.lock().unwrap();
            if ctx.callbacks.deferred_bgm_recovery_enabled()
                && ctx.scripts_view.get(&file).is_some_and(|s| s.get_label_line(LABEL).is_some())
            {
                let fresh = !ctx.bgm_recovery.is_pending();
                let frame = ctx.frame_number;
                ctx.bgm_recovery.request(frame, &file);
                if fresh { ctx.callbacks.debug(0, "[bgm-recovery] scheduled channels=21 per_tick=3", false); }
                return Ok(());
            }
        }
        engine.borrow::<crate::lua_engine::EngineApi>()?.ctx.lock().unwrap().bgm_recovery.cancel();
        original.call::<()>((engine, params))
    })?;
    state.set("wrapper", wrapper.clone())?;
    lua.set_named_registry_value(REGISTRY, state)?;
    lua.globals().set("bgmFadeUpBacklog", wrapper)
}

pub(crate) fn is_installed(lua: &mlua::Lua) -> bool {
    let check = || -> mlua::Result<bool> {
        let state = lua.named_registry_value::<mlua::Table>(REGISTRY)?;
        let wrapper = state.get::<mlua::Function>("wrapper")?;
        let helper = state.get::<mlua::Function>("helper")?;
        let current = lua.globals().get::<mlua::Function>("bgmFadeUpBacklog")?;
        let filters = lua.globals().get::<mlua::Table>("tagFilters")?;
        Ok(current.to_pointer() == wrapper.to_pointer()
            && filters.get::<mlua::Function>("se_sound_info")?.to_pointer() == helper.to_pointer())
    };
    check().unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn install_for_test(lua: &mlua::Lua, file: &str) {
    let original = lua.globals().get("bgmFadeUpBacklog").unwrap();
    let helper = lua.globals().get::<mlua::Table>("tagFilters").unwrap().get("se_sound_info").unwrap();
    install(lua, original, helper, file.into()).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags::var_handler::SoundChannelInfo;

    fn speaking() -> SoundInfoSnapshot {
        SoundInfoSnapshot { bgm: None, se: vec![SoundChannelInfo {
            id: "100.0".into(), playing: true, gain: 1000, pan: 0,
        }] }
    }

    #[test]
    fn recovery_spreads_scan_and_coalesces_without_starvation() {
        let mut job = Scheduler::default();
        let mut vars = VariableStore::new();
        let silent = SoundInfoSnapshot::default();
        for id in 100..=120 { vars.set(&format!("snd.se.{id}.0"), "0|0|1000|0|voice".into()); }
        job.request(10, "system/audio.iet");
        assert!(job.tick(10, &silent, &mut vars).is_none());
        assert!(vars.get("snd.se.100.0").is_some());
        for frame in 11..=16 {
            job.request(frame, "system/audio.iet");
            assert!(job.tick(frame, &silent, &mut vars).is_none());
            assert!(job.tick(frame, &silent, &mut vars).is_none());
            assert!(vars.get(&format!("snd.se.{}.0", 100 + (frame - 10) * 3)).is_some());
        }
        let token = job.tick(17, &silent, &mut vars).expect("seven ticks complete 21 channels");
        assert!(job.tick(18, &silent, &mut vars).is_none());
        assert_eq!(job.finish(token, &silent, &vars).as_deref(), Some("system/audio.iet"));
        assert!(job.finish(token, &silent, &vars).is_none());
    }

    #[test]
    fn recovery_rechecks_earlier_channels_and_rejects_cancelled_tokens() {
        let mut job = Scheduler::default();
        let mut vars = VariableStore::new();
        let silent = SoundInfoSnapshot::default();
        job.request(0, "audio");
        let token = (1..=7).find_map(|f| job.tick(f, &silent, &mut vars)).unwrap();
        assert!(job.finish(token, &speaking(), &vars).is_none());
        job.request(8, "audio");
        let old = (9..=15).find_map(|f| job.tick(f, &silent, &mut vars)).unwrap();
        job.cancel();
        job.request(15, "audio");
        let new = (16..=22).find_map(|f| job.tick(f, &silent, &mut vars)).unwrap();
        assert!(job.finish(old, &silent, &vars).is_none());
        assert_eq!(job.finish(new, &silent, &vars).as_deref(), Some("audio"));
    }

    #[test]
    fn recovery_allows_stopping_voice_but_preserves_its_flags() {
        let mut job = Scheduler::default();
        let mut vars = VariableStore::new();
        vars.set("snd.se.100.0", "1|0|1000|0|voice".into());
        job.request(0, "audio");
        let token = (1..=7).find_map(|f| job.tick(f, &speaking(), &mut vars)).unwrap();
        assert!(vars.get("snd.se.100.0").is_some());
        assert_eq!(job.finish(token, &speaking(), &vars).as_deref(), Some("audio"));
    }
}
