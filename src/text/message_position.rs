//! Per-project host presentation; never serialized into script or save state.
use asb_interpreter::MessageLayerIds;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessagePosition {
    pub enabled: bool,
    pub hide_subtitle: bool,
    pub dialogue: [i32; 2],
    pub subtitle: [i32; 2],
}
impl MessagePosition {
    pub fn valid(self) -> bool {
        self.dialogue.into_iter().chain(self.subtitle).all(|v| (-500..=500).contains(&v))
    }
    pub(super) fn layer(self, id: &str, roles: Option<&MessageLayerIds>) -> (bool, [i32; 2]) {
        let (body, sub) = if let Some(roles) = roles {
            // An ambiguous mapping must never hide the main text or speaker name.
            if roles.name.as_deref() == Some(id) { return (false, [0, 0]); }
            let body = roles.dialogue.as_deref() == Some(id);
            (body, !body && roles.subtitle.as_deref() == Some(id))
        } else {
            match id.rsplit_once(".mw.").map(|(_, role)| role) {
                Some("adv" | "adv_adv") => (true, false),
                Some("sub" | "adv_sub") => (false, true),
                _ => (false, false),
            }
        };
        let offset = if !self.enabled { [0, 0] } else if body { self.dialogue }
            else if sub { self.subtitle } else { [0, 0] };
        (sub && self.hide_subtitle, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn position_defaults_isolation_and_ambiguous_roles() {
        let settings = MessagePosition { enabled: true, hide_subtitle: true,
            dialogue: [5, -20], subtitle: [-10, 30] };
        let mut roles = MessageLayerIds { name: Some("speaker".into()),
            dialogue: Some("body".into()), subtitle: Some("sub".into()) };
        assert_eq!(settings.layer("body", Some(&roles)), (false, [5, -20]));
        assert_eq!(settings.layer("sub", Some(&roles)), (true, [-10, 30]));
        for id in ["speaker", "body.child", "backlog", "100.mw.adv"] {
            assert_eq!(settings.layer(id, Some(&roles)), (false, [0, 0]));
        }
        assert_eq!(MessagePosition::default().layer("sub", Some(&roles)), (false, [0, 0]));
        assert_eq!(MessagePosition { enabled: false, ..settings }.layer("sub", Some(&roles)), (true, [0, 0]));
        roles.subtitle = roles.dialogue.clone();
        assert_eq!(settings.layer("body", Some(&roles)), (false, [5, -20]));
        assert_eq!(settings.layer("1.80.mw.adv_sub", None), (true, [-10, 30]));
        assert_eq!(settings.layer("1.80.mw.adv_name", None), (false, [0, 0]));
        assert_eq!(settings.layer("1.80.mw.adv_sub", Some(&MessageLayerIds::default())), (false, [0, 0]));
        assert!(settings.valid());
        assert!(!MessagePosition { dialogue: [501, 0], ..settings }.valid());
    }
}
