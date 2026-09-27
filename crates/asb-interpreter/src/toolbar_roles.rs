//! Touch-toolbar roles from script declarations; no asset names or fixed IDs.
use crate::Interpreter;
use mlua::{Table, Value};

fn table(value: Value) -> Option<Table> {
    if let Value::Table(t) = value {
        Some(t)
    } else {
        None
    }
}
fn id(value: Value) -> Option<String> {
    let Value::String(s) = value else { return None };
    let s = s.to_str().ok()?;
    (!s.is_empty() && s.len() <= 1024 && !s.contains('\0') && s != "!").then(|| s.to_string())
}

impl Interpreter {
    pub fn query_toolbar_layer_ids(&self) -> Vec<String> {
        let globals = self.lua().globals();
        let Some(init) = globals.raw_get::<Value>("init").ok().and_then(table) else {
            return vec![];
        };
        let Some(root) = init.raw_get::<Value>("mwtabid").ok().and_then(id) else {
            return vec![];
        };
        let mut ids = vec![root.clone()];
        // The draggable hit plane is a sibling of the visual toolbar. Find its
        // owning button group by the background's declared ID, without calling
        // script functions (which may emit events or depend on btn.name).
        if let Some(buttons) = globals.raw_get::<Value>("btn").ok().and_then(table) {
            for (_, value) in buttons.pairs::<Value, Value>().flatten() {
                let Some(group) = table(value) else { continue };
                let Some(prefix) = group.raw_get::<Value>("id").ok().and_then(id) else {
                    continue;
                };
                let Some(p) = group.raw_get::<Value>("p").ok().and_then(table) else {
                    continue;
                };
                let path = |name| {
                    let entry = p.raw_get::<Value>(name).ok().and_then(table)?;
                    let suffix = entry.raw_get::<Value>("id").ok().and_then(id)?;
                    Some(format!("{prefix}{suffix}"))
                };
                if let Some(bg) = path("tb_bg")
                    && (bg == root || bg.strip_prefix(&root).is_some_and(|s| s.starts_with('.')))
                    && let Some(mask) = path("tb_mask")
                    && !ids.contains(&mask)
                {
                    ids.push(mask);
                }
            }
        }
        ids
    }
}
