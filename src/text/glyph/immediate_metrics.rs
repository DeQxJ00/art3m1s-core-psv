use super::*;

impl GlyphTextRenderer {
    /// A layout-only view: font bytes are shared, no atlas or GPU allocation.
    pub(crate) fn measurement_copy(&self) -> Self {
        let mut out = Self::new_internal(true);
        self.sync_measurement_copy(&mut out);
        out
    }

    pub(crate) fn sync_measurement_copy(&self, out: &mut Self) {
        // Queries may edit the mirror. Compare exact layout inputs, retaining
        // allocations for unchanged layers instead of recreating the whole UI.
        macro_rules! refresh {
            ($target:expr, $source:expr) => {
                if $target != $source { $target.clone_from(&$source); }
            };
        }
        out.font = self.font.clone();
        if out.next_font_generation != self.next_font_generation {
            out.fonts.clone_from(&self.fonts);
        }
        out.font_generation = self.font_generation;
        out.next_font_generation = self.next_font_generation;
        out.message_sizes = self.message_sizes;
        out.message_roles.clone_from(&self.message_roles);
        let full = !std::sync::Arc::ptr_eq(&out.measurement_global, &self.measurement_global);
        out.measurement_versions.retain(|id, _| self.state.layers.contains_key(id));
        out.state.layers.retain(|id, _| self.state.layers.contains_key(id));
        for (id, source) in &self.state.layers {
            let same_version = match (self.measurement_versions.get(id), out.measurement_versions.get(id)) {
                (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if !full && same_version && out.state.layers.contains_key(id) { continue; }
            if let Some(version) = self.measurement_versions.get(id) {
                out.measurement_versions.insert(id.clone(), version.clone());
            } else { out.measurement_versions.remove(id); }

            if !out.state.layers.contains_key(id) {
                out.state.layers.insert(id.clone(), MessageLayer::new(id.clone()));
            }
            let layer = out.state.layers.get_mut(id).unwrap();
            layer.left = source.left;
            layer.top = source.top;
            layer.width = source.width;
            layer.height = source.height;
            refresh!(layer.font, source.font);
            refresh!(layer.font_stack, source.font_stack);
            refresh!(layer.text_buffer, source.text_buffer);
            refresh!(layer.indent_initial, source.indent_initial);
            refresh!(layer.indent_actions, source.indent_actions);
            refresh!(layer.indent_options, source.indent_options);
            refresh!(layer.rubies, source.rubies);
            refresh!(layer.open_ruby, source.open_ruby);
            layer.page_tags.clear();
        }
        out.measurement_global = self.measurement_global.clone();
        refresh!(out.state.active_layer, self.state.active_layer);
        refresh!(out.state.layer_stack, self.state.layer_stack);
        refresh!(out.state.default_font, self.state.default_font);
        refresh!(out.state.layout, self.state.layout);
    }

    pub(crate) fn observe_metric_tag(&mut self, tag: &str, p: &HashMap<String, String>) {
        self.mark_active_snapshot_changed();
        let val = |key: &str| p.get(key).map(String::as_str);
        match tag {
            "chgmsg" => self.switch_message_layer(val("id"), val("stack") != Some("0")),
            "/chgmsg" => self.pop_message_layer(),
            "font" => self.apply_font_settings(p),
            "fontinit" => self.font_init(),
            "/font" => self.font_pop(),
            "fontdefault" => self.font_default(p),
            "print" => self.push_text(val("data").unwrap_or(""), false),
            "rp" => self.state.active_layer_mut().clear_page(),
            "rt" => {
                self.state.set_rt_omit_blank_line(val("omitblankline").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0) != 0);
                self.push_line_break();
            }
            "prohibit" => self.state.set_prohibit(
                Some(val("head").unwrap_or(crate::text::render::DEFAULT_PROHIBIT_HEAD)),
                Some(val("foot").unwrap_or(crate::text::render::DEFAULT_PROHIBIT_FOOT))),
            "wordparts" => self.state.set_wordparts(val("parts").unwrap_or(crate::text::render::DEFAULT_WORDPARTS)),
            "indent" => self.state.configure_indent(val("pair").unwrap_or(""),
                val("range").and_then(|v| v.parse().ok()).filter(|v| *v != 0),
                val("nest").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0) != 0,
                val("logicalrange").is_some_and(|v| v != "0")),
            "indentmodify" => self.modify_indent(val("unindent").and_then(|v| v.parse().ok()).unwrap_or(0)),
            _ => return,
        }
        if let Some(face) = self.active_font_face().map(str::to_owned) {
            self.select_cached_font(&face);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "gxm-text-epoch")]
    #[test]
    fn reveal_animation_preserves_message_query_revision() {
        let mut renderer = GlyphTextRenderer::new();
        renderer.switch_message_layer(Some("dialogue"), true);
        renderer.push_line_break();
        let revision = renderer.snapshot_revision();
        renderer.reset_reveal();
        renderer.advance_reveal(17);
        renderer.hide_text();
        renderer.show_text();
        renderer.reveal_all();
        assert_eq!(revision, renderer.snapshot_revision());
        renderer.push_line_break();
        assert_ne!(revision, renderer.snapshot_revision());
    }
    #[test]
    fn measurement_sync_reuses_unchanged_layers_and_restores_query_edits() {
        let mut real = GlyphTextRenderer::new();
        real.switch_message_layer(Some("stable"), true);
        real.state.active_layer_mut().font.face = Some("fixture-font".into());
        let mut preview = real.measurement_copy();
        let original = preview.state.layers["stable"].font.face.as_ref().unwrap().as_ptr();
        real.sync_measurement_snapshot(&mut preview);
        assert_eq!(original, preview.state.layers["stable"].font.face.as_ref().unwrap().as_ptr());
        preview.font_state_mut().layers.get_mut("stable").unwrap().width = 99.0;
        preview.switch_message_layer(Some("temporary"), true);
        real.font_state_mut().active_layer_mut().height = 42.0;
        real.sync_measurement_snapshot(&mut preview);
        assert!(!preview.state.layers.contains_key("temporary"));
        assert_eq!(preview.state.layers["stable"].width, real.state.layers["stable"].width);
        assert_eq!(preview.state.layers["stable"].height, 42.0);
        assert_eq!(preview.state.active_layer, real.state.active_layer);
        assert_eq!(preview.state.layer_stack, real.state.layer_stack);
        real.switch_message_layer(Some("other"), true);
        real.apply_font_settings(&HashMap::from([("size".into(), "21".into())]));
        real.sync_measurement_snapshot(&mut preview);
        assert_eq!(preview.state.layers["other"].font.size, Some(21.0));
        real.font_state_mut().layers.get_mut("stable").unwrap().font.size = Some(17.0);
        real.sync_measurement_snapshot(&mut preview);
        assert_eq!(preview.state.layers["stable"].font.size, Some(17.0));
        preview.observe_metric_tag("font", &HashMap::from([("size".into(), "99".into())]));
        real.sync_measurement_snapshot(&mut preview);
        assert_eq!(preview.state.layers["other"].font.size, Some(21.0));
    }
    #[test]
    #[ignore = "requires ART3M1S_TEST_FONT pointing to a local font fixture"]
    fn immediate_metrics_match_rendered_multiline_text_without_an_atlas() {
        let mut real = GlyphTextRenderer::new();
        real.set_font(&std::fs::read(std::env::var("ART3M1S_TEST_FONT").unwrap()).unwrap()).unwrap();
        real.switch_message_layer(Some("dialogue"), true);
        real.push_text("An unrelated previous message", false);
        let mut preview = real.measurement_copy();
        assert!(preview.state.layers.values().all(|l| l.page_tags.is_empty() && l.scetween.is_empty()));
        for (tag, args) in [
            ("chgmsg", vec![("id", "phone.text")]),
            ("font", vec![("size", "18"), ("width", "130")]),
            ("rp", vec![]), ("print", vec![("data", "Hello there")]),
            ("rt", vec![]), ("print", vec![("data", "Welcome!")]),
        ] { preview.observe_metric_tag(tag, &args.into_iter().map(|(k,v)| (k.into(),v.into())).collect()); }
        real.switch_message_layer(Some("phone.text"), true);
        real.apply_font_settings(&HashMap::from([("size".into(),"18".into()),("width".into(),"130".into())]));
        real.push_page_break(Some(0));
        real.push_text("Hello there", false);
        real.push_line_break();
        real.push_text("Welcome!", false);
        assert_eq!(preview.active_layer_text_metrics(), real.active_layer_text_metrics());
        assert!(preview.active_layer_text_metrics().unwrap().0 > 0.0);
        assert!(preview.atlases.is_empty());
        assert_eq!(real.state.active_layer.as_deref(), Some("phone.text"));
    }
}
