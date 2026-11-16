//! Synchronous script queries must include emitted, not-yet-dispatched layers.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use asb_interpreter::Event;

use crate::compositor::{Compositor, TextureInfo};

#[derive(Default)]
pub(super) struct LayerQueryState {
    scene: Compositor,
    pub(super) memory_audio: crate::save::AudioSnapshot,
    pub(super) memory_text: Option<crate::save::MessageTextSnapshot>,
    accepted_revisions: HashMap<String, (u64, u64)>,
    sync_reports: usize,
    dimensions: HashMap<String, Option<(u32, u32)>>,
}

impl LayerQueryState {
    pub fn memory_scene(&self) -> crate::save::MemoryScene {
        crate::save::MemoryScene {
            scene: self.scene.scene_snapshot(),
            audio: self.memory_audio.clone(),
            message_text: self.memory_text.clone(),
        }
    }

    pub fn observe_tag(&mut self, tag: &str, params: &HashMap<String, String>) {
        use asb_interpreter::event::LayerEvent;
        let Some(id) = params.get("id") else { return };
        let id = id.clone();
        let event = match tag {
            "lyc" => LayerEvent::Create {
                id: id.clone(), file: params.get("file").cloned().unwrap_or_default(),
            },
            "lyc2" => LayerEvent::Create2 {
                id: id.clone(), file: params.get("file").cloned().unwrap_or_default(),
                alpha: params.get("alpha").and_then(|v| v.parse().ok()),
            },
            "lydel" => LayerEvent::Delete { id: id.clone() },
            "lyprop" => LayerEvent::SetProperties {
                id: id.clone(), properties: params.iter().filter(|(k, _)| k.as_str() != "id")
                    .map(|(k, v)| (k.clone(), v.clone())).collect(),
            },
            _ => return,
        };
        self.observe(&Event::Layer(event));
        if tag == "lyc" {
            let properties = params.iter()
                .filter(|(k, _)| matches!(k.as_str(), "mask" | "width" | "height" | "color"))
                .map(|(k, v)| (k.clone(), v.clone())).collect();
            self.observe(&Event::Layer(LayerEvent::SetProperties { id, properties }));
        }
    }

    pub fn sync_clock(&mut self, scene: &Compositor, mut cached: impl FnMut(&str) -> Option<TextureInfo>) {
        let ids = self.scene.sync_query_clock_from(scene, &mut self.accepted_revisions);
        for id in ids {
            if let Some(file) = self.scene.scene().get(id).and_then(|layer| layer.file.as_ref())
                && let Some(info) = cached(file)
            {
                self.dimensions.insert(file.clone(), Some((info.width, info.height)));
            }
        }
    }

    pub fn sync(
        &mut self,
        scene: &Compositor,
        mut cached: impl FnMut(&str) -> Option<TextureInfo>,
    ) {
        let started=std::time::Instant::now();
        let copied=self.scene.sync_query_changes_from(scene, &mut self.accepted_revisions);
        let mut live = HashSet::new();
        for layer in scene.scene().all_layers() {
            if let Some(file) = &layer.file {
                live.insert(file.as_str());
                if let Some(info) = cached(file) {
                    self.dimensions
                        .insert(file.clone(), Some((info.width, info.height)));
                }
            }
        }
        self.dimensions
            .retain(|file, _| live.contains(file.as_str()));
        let us=started.elapsed().as_micros();
        if us>=2000 && self.sync_reports<24 {
            self.sync_reports+=1;
            crate::core_info!("[layer-query-sync] copied={} total={} sync_us={} sample={}/24",copied,scene.scene().len(),us,self.sync_reports);
        }
    }

    pub fn observes(event: &Event) -> bool {
        matches!(
            event,
            Event::Layer(_)
                | Event::AllDelete { .. }
                | Event::LayerRename { .. }
                | Event::LayerTween { .. }
                | Event::LayerTweenDelete { .. }
                | Event::Anime { .. }
                | Event::TweenSetStart
                | Event::TweenSetEnd
        )
    }

    pub fn observe(&mut self, event: &Event) {
        // Reuse the compositor reducer, including parent creation, subtree
        // deletion, property merge and rename. Never render or dispatch media.
        if Self::observes(event) {
            self.scene.apply_event(event);
        }
    }

    pub fn sync_layer(
        &mut self,
        scene: &Compositor,
        id: &str,
        cached: impl FnOnce(&str) -> Option<TextureInfo>,
    ) {
        self.scene.clock_ms = scene.clock_ms();
        if let Some(layer) = scene.scene().get(id) {
            self.scene.scene.ensure(id);
            *self.scene.scene.get_mut(id).unwrap() = layer.clone();
            if let Some(file) = &layer.file
                && let Some(info) = cached(file)
            {
                self.dimensions
                    .insert(file.clone(), Some((info.width, info.height)));
            }
        } else {
            self.scene.scene.delete(id);
        }
    }

    pub fn get(
        &mut self,
        id: &str,
        mut dimensions: impl FnMut(&str) -> Option<(u32, u32)>,
    ) -> Option<HashMap<String, String>> {
        let layer = self.scene.scene().get(id)?;
        let props = crate::compositor::build::resolved_props(layer, self.scene.clock_ms());
        let needs_dimensions =
            props.clip_rect().is_none() && (props.width.is_none() || props.height.is_none());
        let texture_info = layer
            .file
            .as_deref()
            .filter(|file| needs_dimensions && !file.is_empty())
            .and_then(|file| {
                let size = self
                    .dimensions
                    .entry(file.to_string())
                    .or_insert_with(|| dimensions(file));
                size.map(|(width, height)| TextureInfo { width, height })
            });
        Some(super::events::layer_info_entry(
            layer,
            self.scene.clock_ms(),
            texture_info,
        ))
    }

    pub fn all(
        &mut self,
        mut dimensions: impl FnMut(&str) -> Option<(u32, u32)>,
    ) -> Vec<(String, HashMap<String, String>)> {
        let mut ids: Vec<_> = self
            .scene
            .scene()
            .all_layers()
            .map(|layer| layer.id.clone())
            .collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| self.get(&id, &mut dimensions).map(|info| (id, info)))
            .collect()
    }
}

pub(super) fn asset_dimensions(
    paths: &super::magic_path::MagicPathTable,
    file: &str,
) -> Option<(u32, u32)> {
    if file.starts_with("image/text/") {
        return None;
    }
    let resolved = super::magic_path::resolve_path(paths, file);
    // Match the texture provider's lookup order, but only read image headers.
    for path in crate::native_texture::candidates(&resolved) {
        for limit in [4096, 65536, 1048576] {
            let Some(bytes) = crate::ffi::request_asset_range(&path, 0, limit) else {
                break;
            };
            let short = bytes.len() < limit;
            if let Some(size)=crate::native_texture::header_dimensions(&bytes){return Some(size);}
            if let Ok(reader) = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()
                && let Ok(size) = reader.into_dimensions()
            {
                return Some(size);
            }
            if short {
                break;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use asb_interpreter::event::LayerEvent;

    #[test]
    #[ignore = "requires external audited position Lua block"]
    fn scoped_position_query_matches_audited_calculation() {
        use asb_interpreter::{Interpreter, InterpreterConfig};
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex};
        let source = std::fs::read_to_string(std::env::var("ART3M1S_POSITION_QUERY_AUDIT").unwrap()).unwrap();
        let run = |fast: bool, missing: u8, left: i32, reverse: &str, existing: bool| {
            let state = Arc::new(Mutex::new(LayerQueryState::default()));
            for (id, props) in [
                ("10.1", vec![("left", "423".to_owned())]),
                ("10.1.0.0.1.a.1", vec![("width", if missing == 4 { "0" } else { "513" }.into()), ("height", "807".into())]),
                ("10.1.0.0.1", vec![("left", left.to_string()), ("top", "-31".into())]),
                ("10.1.0.0.0", vec![("visible", "1".into())]),
            ] {
                if missing == 1 || (missing == 2 && id.ends_with(".a.1")) || (missing == 3 && id == "10.1.0.0.0") { continue; }
                let mut s = state.lock().unwrap();
                s.observe(&Event::Layer(LayerEvent::Create { id: id.into(), file: String::new() }));
                s.observe(&Event::Layer(LayerEvent::SetProperties { id: id.into(),
                    properties: props.into_iter().map(|(k, v)| (k.into(), v)).collect() }));
            }
            // These unrelated assets must never be probed by the scoped path.
            for n in 0..60 {
                state.lock().unwrap().observe(&Event::Layer(LayerEvent::Create {
                    id: format!("90.{n}"), file: format!("unrelated/{n}"),
                }));
            }
            let mut it = Interpreter::new(InterpreterConfig::default());
            it.set_engine_callbacks(Box::new(super::super::callbacks::FfiCallbacks {
                input: Default::default(), magic_paths: Default::default(), layer_info: Arc::clone(&state),
                png_comments: Default::default(), savepath: "save".into(), volumes: Default::default(),
                debug_skip_active: Default::default(), script_status: Default::default(),
                script_status_request: Arc::new(std::sync::atomic::AtomicU16::new(crate::runtime::NO_SCRIPT_STATUS_REQUEST)),
                emote: Default::default(),
            }));
            it.load_script("position", &format!("[lua]\n{source}\n[/lua]\n[stop]\n")).unwrap();
            if !fast { it.lua().load(&source).exec().unwrap(); }
            for (k, v) in [("g.screen_width", "960"), ("g.screen_height", "544"),
                ("fg_eyelv", "34"), ("f_id", "10.1"), ("fg_name", "actor"),
                ("fg_pos.actor", if existing { "277" } else { "0" }),
                ("mode", "0"), ("now", "now"), ("reverse", reverse),
                ("pos_rout", if existing { "960" } else { "0" })] {
                it.set_variable(k, asb_interpreter::Value::String(v.into()));
            }
            it.lua().load("getPosCalc(__engine)").exec().unwrap();
            let globals: BTreeMap<_, _> = it.variables().iter_local().map(|(k, v)| (k.clone(), v.to_string())).collect();
            let queried: BTreeMap<_, _> = it.variables().iter_temp()
                .filter(|(k, _)| ["10.1", "10.1.0.0.1.a.1", "10.1.0.0.1", "10.1.0.0.0"].iter()
                    .any(|id| k.strip_prefix(&format!("l.li.{id}.")).is_some_and(|prop| !prop.contains('.'))))
                .map(|(k, v)| (k.clone(), v.to_string())).collect();
            let tags = Arc::new(Mutex::new(Vec::new()));
            let emitted = Arc::clone(&tags);
            it.set_callback(move |event| {
                if let Event::Layer(LayerEvent::SetProperties { id, properties }) = event {
                    emitted.lock().unwrap().push((id, properties.into_iter().collect::<BTreeMap<_, _>>()));
                } else { panic!("unexpected position event: {event:?}"); }
                asb_interpreter::CallbackResult::Continue
            });
            it.drain_queued_tags_only().unwrap();
            let tags = tags.lock().unwrap().clone();
            if fast {
                assert!(state.lock().unwrap().dimensions.is_empty(), "must not probe unrelated assets");
            } else {
                assert_eq!(state.lock().unwrap().dimensions.len(), 60, "baseline enumerates unrelated assets");
            }
            (globals, queried, tags)
        };
        for missing in 0..5 {
            for left in [-53, 0, 47] {
                for reverse in ["none", "1"] {
                    for existing in [false, true] {
                        assert_eq!(run(false, missing, left, reverse, existing), run(true, missing, left, reverse, existing),
                            "missing={missing} left={left} reverse={reverse} existing={existing}");
                    }
                }
            }
        }
    }

    #[test]
    fn lua_creation_and_next_lua_query_work_in_one_interpreter_run() {
        use asb_interpreter::{CallbackResult, Interpreter, InterpreterConfig};
        use std::sync::{Arc, Mutex};

        let state = Arc::new(Mutex::new(LayerQueryState::default()));
        state
            .lock()
            .unwrap()
            .dimensions
            .insert("body".into(), Some((1024, 760)));
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_engine_callbacks(Box::new(super::super::callbacks::FfiCallbacks {
            input: Default::default(),
            magic_paths: Default::default(),
            layer_info: Arc::clone(&state),
            png_comments: Default::default(),
            savepath: "save".into(),
            volumes: Default::default(),
            debug_skip_active: Default::default(),
            script_status: Default::default(),
            script_status_request: std::sync::Arc::new(std::sync::atomic::AtomicU16::new(crate::runtime::NO_SCRIPT_STATUS_REQUEST)),
            emote: Default::default(),
        }));
        interpreter.set_callback(move |event| {
            state.lock().unwrap().observe(&event);
            CallbackResult::Continue
        });
        interpreter
            .load_script(
                "main",
                r#"
[lua]
function createCharacter(e)
    e:tag{"lyc", id="10.1.6.0.0.1.a.1", file="body"}
    e:tag{"var", name="immediate", system="get_layer_info", id="10.1.6.0.0.1.a.1", writelocal=1}
    e:tag{"var", name="bar_left", data=(960-tonumber(e:var("immediate.width"))*2)/2}
    e:tag{"var", name="button_center", data=tonumber(e:var("immediate.width"))/2}
    e:tag{"lyprop", id="10.1.6.0.0.1.a.1", left=35, clip="0,0,480,40"}
    e:tag{"var", name="clipped", system="get_layer_info", id="10.1.6.0.0.1.a.1"}
    e:tag{"var", name="clipped_center", data=tonumber(e:var("clipped.width"))/2}
    e:enqueueTag{"lyprop", id="10.1.6.0.0.1.a.1", left=900}
    e:tag{"var", name="before_deferred", system="get_layer_info", id="10.1.6.0.0.1.a.1"}
end
function placeCharacter(e)
    e:tag{"var", name="q", system="get_layer_info", style="map"}
    e:tag{"var", name="observed_height", data=e:var("q.10.1.6.0.0.1.a.1.height")}
end
[/lua]
[calllua function="createCharacter"]
[calllua function="placeCharacter"]
[stop]
"#,
            )
            .unwrap();
        interpreter.start("main", "").unwrap();
        interpreter.run().unwrap();
        for (name, expected) in [("bar_left", "-544"), ("button_center", "512"),
            ("clipped_center", "240"), ("before_deferred.left", "35")] {
            assert_eq!(interpreter.variables().get(name).unwrap().to_string(), expected, "{name}");
        }
        assert_eq!(
            interpreter
                .variables()
                .get("observed_height")
                .unwrap()
                .to_string(),
            "40"
        );
    }

    #[test]
    fn queries_include_pending_create_props_rename_and_delete_without_rendering() {
        let mut state = LayerQueryState::default();
        let mut actual = Compositor::new();
        state.sync(&actual, |_| None);
        let create = Event::Layer(LayerEvent::Create {
            id: "10.1.6.0.0.1.a.1".into(),
            file: "body".into(),
        });
        state.observe(&create);
        assert!(actual.scene().get("10.1.6").is_none());
        let info = state
            .get("10.1.6.0.0.1.a.1", |_| Some((1024, 760)))
            .unwrap();
        assert_eq!(info["width"], "1024");
        assert_eq!(info["height"], "760");
        let props = Event::Layer(LayerEvent::SetProperties {
            id: "10.1.6".into(),
            properties: HashMap::from([
                ("left".into(), "640".into()),
                ("top".into(), "-40".into()),
            ]),
        });
        state.observe(&props);
        assert_eq!(state.get("10.1.6", |_| None).unwrap()["top"], "-40");
        assert!(
            state
                .all(|_| panic!("cached dimensions"))
                .iter()
                .any(|(id, _)| id == "10.1.6.0.0.1.a.1")
        );
        actual.apply_event(&create);
        actual.apply_event(&props);
        state.sync(&actual, |_| None);
        state.observe(&Event::LayerRename {
            id: "10.1.6".into(),
            to: "10.2.6".into(),
        });
        assert!(state.get("10.1.6", |_| None).is_none());
        assert_eq!(
            state
                .get("10.2.6.0.0.1.a.1", |_| panic!("cached dimensions"))
                .unwrap()["width"],
            "1024"
        );
        state.observe(&Event::Layer(LayerEvent::Delete {
            id: "10.2.6".into(),
        }));
        assert!(state.get("10.2.6.0.0.1.a.1", |_| None).is_none());
        state.sync(&Compositor::new(), |_| None);
        assert!(state.dimensions.is_empty());
    }
}
