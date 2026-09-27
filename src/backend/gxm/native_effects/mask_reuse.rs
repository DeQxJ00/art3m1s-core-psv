//! Reuse only identical native masks within a frame. The host token identifies
//! the physical target and rejects overwritten targets, other depths and frames.
use super::*;

#[derive(Default)]
pub(super) struct MaskReuse {
    commands: Vec<DrawCommand>,
    token: u64,
    size: (u32, u32),
    texture_revision: u64,
}

impl MaskReuse {
    pub fn clear(&mut self) {
        self.commands.clear();
        self.token = 0;
    }

    // Must be immediately followed by group_end (or group_end_cached).
    pub fn draw(&mut self, commands: &[DrawCommand], width: u32, height: u32) -> bool {
        let eligible = !commands.is_empty() && commands.len() <= 64
            && commands.iter().all(|c| c.native_emote.is_some() && c.shader.is_none());
        let revision = unsafe { art3m1s_gxm_texture_revision() };
        if eligible && self.token != 0 && self.size == (width, height)
            && self.texture_revision == revision && self.commands == commands
            && unsafe { art3m1s_gxm_group_mask_reuse(self.token) != 0 }
        {
            return true;
        }
        self.clear();
        if unsafe { art3m1s_gxm_group_mask_begin() } == 0 {
            return false;
        }
        for command in commands {
            if let Some(draw) = encode(command, width, height) {
                unsafe { art3m1s_gxm_draw_effect(&draw) };
            }
        }
        if eligible {
            self.token = unsafe { art3m1s_gxm_group_mask_revision() };
            self.size = (width, height);
            self.texture_revision = revision;
            self.commands.extend_from_slice(commands);
        }
        true
    }
}
