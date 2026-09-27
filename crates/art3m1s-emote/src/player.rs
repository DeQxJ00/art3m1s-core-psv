use std::collections::{BTreeMap, VecDeque};

use crate::EmoteModel;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmoteTransform {
    pub scale: [f32; 3],
    pub coord: [f32; 4],
}

impl Default for EmoteTransform {
    fn default() -> Self {
        Self {
            scale: [1.0, 0.0, 0.0],
            coord: [0.0; 4],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariableState {
    pub value: f32,
    pub target: f32,
    pub remaining_frames: f32,
    pub easing: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimelineState {
    pub label: String,
    pub flags: u32,
    pub position: f32,
    pub weight: f32,
    pub target_weight: f32,
    pub remaining_frames: f32,
    pub easing: u32,
}

#[derive(Clone, Debug, PartialEq)]
struct DifferenceTrackState {
    value: f32,
    start: f32,
    target: f32,
    elapsed: f32,
    duration: f32,
    /// Latest authored frame reached by the native timeline cursor.
    cursor: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EmoteCommand {
    SetScale([f32; 3]),
    SetCoord([f32; 4]),
    SetVariable {
        label: String,
        value: f32,
        frames: f32,
        easing: u32,
    },
    PlayTimeline {
        label: String,
        flags: u32,
    },
    FadeInTimeline {
        label: String,
        frames: f32,
        easing: u32,
    },
    FadeOutTimeline {
        label: String,
        frames: f32,
        easing: u32,
    },
    StopTimeline {
        label: String,
    },
    Pass,
    Step,
    Skip,
}

#[derive(Debug, Default)]
pub struct EmotePlayer {
    transform: EmoteTransform,
    variables: BTreeMap<String, VariableState>,
    timelines: BTreeMap<String, TimelineState>,
    difference_tracks: BTreeMap<String, BTreeMap<String, DifferenceTrackState>>,
    ordinary_cursors: BTreeMap<String, Vec<usize>>,
    commands: VecDeque<EmoteCommand>,
}

impl EmotePlayer {
    pub fn transform(&self) -> EmoteTransform {
        self.transform
    }

    pub fn variables(&self) -> &BTreeMap<String, VariableState> {
        &self.variables
    }

    pub fn timelines(&self) -> &BTreeMap<String, TimelineState> {
        &self.timelines
    }

    pub fn set_scale(&mut self, scale: f32, origin_x: f32, origin_y: f32) {
        self.transform.scale = [scale, origin_x, origin_y];
        self.commands
            .push_back(EmoteCommand::SetScale(self.transform.scale));
    }

    pub fn set_coord(&mut self, x: f32, y: f32, z: f32, angle: f32) {
        self.transform.coord = [x, y, z, angle];
        self.commands
            .push_back(EmoteCommand::SetCoord(self.transform.coord));
    }

    pub fn set_variable(&mut self, label: impl Into<String>, value: f32, frames: f32, easing: u32) {
        let label = label.into();
        self.set_variable_target(&label, value, frames, easing);
        self.commands.push_back(EmoteCommand::SetVariable {
            label,
            value,
            frames: frames.max(0.0),
            easing,
        });
    }

    fn set_variable_target(&mut self, label: &str, value: f32, frames: f32, easing: u32) {
        let current = self
            .variables
            .get(label)
            .map(|state| state.value)
            .unwrap_or(0.0);
        let frames = frames.max(0.0);
        self.variables.insert(
            label.to_owned(),
            VariableState {
                value: if frames == 0.0 { value } else { current },
                target: value,
                remaining_frames: frames,
                easing,
            },
        );
    }

    pub fn play_timeline(&mut self, label: impl Into<String>, flags: u32) {
        let label = label.into();
        self.timelines.insert(
            label.clone(),
            TimelineState {
                label: label.clone(),
                flags,
                position: 0.0,
                weight: 1.0,
                target_weight: 1.0,
                remaining_frames: 0.0,
                easing: 0,
            },
        );
        self.commands
            .push_back(EmoteCommand::PlayTimeline { label, flags });
    }

    pub fn play_model_timeline(
        &mut self,
        model: &EmoteModel,
        label: impl Into<String>,
        flags: u32,
    ) {
        let label = label.into();
        if flags & 1 == 0 {
            self.timelines.clear();
            self.difference_tracks.clear();
            self.ordinary_cursors.clear();
        }
        self.play_timeline(label.clone(), flags);
        if let Some(timeline) = model.timelines().get(&label) {
            if timeline.diff {
                self.initialize_difference_timeline(timeline);
            } else {
                self.seek_ordinary_timeline(timeline, 0.0);
            }
        }
    }

    pub fn fade_in_model_timeline(
        &mut self,
        model: &EmoteModel,
        label: impl Into<String>,
        frames: f32,
        easing: u32,
    ) {
        let label = label.into();
        let was_playing = self.timelines.contains_key(&label);
        self.fade_in_timeline(label.clone(), frames, easing);
        if !was_playing
            && let Some(timeline) = model
                .timelines()
                .get(&label)
                .filter(|timeline| timeline.diff)
        {
            self.initialize_difference_timeline_at(timeline, 0.0);
        }
    }

    pub fn fade_in_timeline(&mut self, label: impl Into<String>, frames: f32, easing: u32) {
        self.fade_timeline(label.into(), 1.0, frames, easing, true);
    }

    pub fn fade_out_timeline(&mut self, label: impl Into<String>, frames: f32, easing: u32) {
        self.fade_timeline(label.into(), 0.0, frames, easing, false);
    }

    pub fn stop_timeline(&mut self, label: impl Into<String>) {
        let label = label.into();
        self.timelines.remove(&label);
        self.difference_tracks.remove(&label);
        self.ordinary_cursors.remove(&label);
        self.commands
            .push_back(EmoteCommand::StopTimeline { label });
    }

    pub fn pass(&mut self) {
        self.commands.push_back(EmoteCommand::Pass);
    }

    pub fn step(&mut self) {
        self.commands.push_back(EmoteCommand::Step);
    }

    pub fn skip(&mut self) {
        self.commands.push_back(EmoteCommand::Skip);
    }

    /// Advances script-controlled state and reports whether the rendered pose
    /// can have changed. Callers use this to keep static E-Mote layers frozen
    /// instead of rebuilding the complete motion graph at display refresh
    /// rate.
    pub fn advance(&mut self, frames: f32) -> bool {
        self.advance_inner(frames, None)
    }

    pub fn advance_model(&mut self, model: &EmoteModel, frames: f32) -> bool {
        self.advance_inner(frames, Some(model))
    }

    fn advance_inner(&mut self, frames: f32, model: Option<&EmoteModel>) -> bool {
        let frames = frames.max(0.0);
        let mut changed = false;
        let mut difference_updates = Vec::new();
        let mut ordinary_updates = Vec::new();
        for state in self.timelines.values_mut() {
            let old_position = state.position;
            let next_position = model
                .and_then(|model| model.timelines().get(&state.label))
                .filter(|timeline| timeline.loop_end <= timeline.loop_begin)
                .filter(|timeline| timeline.last_time >= 0.0)
                .map_or(state.position + frames, |timeline| {
                    (state.position + frames).min(timeline.last_time)
                });
            changed |= next_position != state.position;
            state.position = next_position;
            if let Some(model) = model
                && model
                    .timelines()
                    .get(&state.label)
                    .is_some_and(|timeline| timeline.diff)
            {
                difference_updates.push((state.label.clone(), old_position, next_position));
                changed = true;
            } else if model.is_some() {
                ordinary_updates.push((state.label.clone(), old_position, next_position));
            }
            let before = state.weight;
            advance_scalar(
                &mut state.weight,
                state.target_weight,
                &mut state.remaining_frames,
                frames,
            );
            changed |= state.weight != before;
        }
        if let Some(model) = model {
            for (label, old_position, new_position) in ordinary_updates {
                if let Some(timeline) = model.timelines().get(&label) {
                    self.advance_ordinary_timeline(timeline, old_position, new_position);
                }
            }
            for (label, old_position, new_position) in difference_updates {
                if let Some(timeline) = model.timelines().get(&label) {
                    self.advance_difference_timeline(
                        &label,
                        timeline,
                        old_position,
                        new_position,
                        frames,
                    );
                }
            }
        }
        for state in self.variables.values_mut() {
            let before = state.value;
            advance_scalar(
                &mut state.value,
                state.target,
                &mut state.remaining_frames,
                frames,
            );
            changed |= state.value != before;
        }
        let before = self.timelines.len();
        self.timelines
            .retain(|_, timeline| timeline.weight != 0.0 || timeline.target_weight != 0.0);
        self.difference_tracks
            .retain(|label, _| self.timelines.contains_key(label));
        self.ordinary_cursors
            .retain(|label, _| self.timelines.contains_key(label));
        changed | (self.timelines.len() != before)
    }

    pub fn take_commands(&mut self) -> impl Iterator<Item = EmoteCommand> + '_ {
        self.commands.drain(..)
    }

    pub fn active_timeline_samples(
        &self,
        model: &EmoteModel,
    ) -> Vec<(&TimelineState, BTreeMap<String, f32>)> {
        self.timelines
            .values()
            .filter_map(|state| {
                model.timelines().get(&state.label).map(|timeline| {
                    let values = if timeline.diff {
                        self.difference_tracks
                            .get(&state.label)
                            .map(|tracks| {
                                tracks
                                    .iter()
                                    .map(|(label, track)| (label.clone(), track.value))
                                    .collect()
                            })
                            .unwrap_or_else(|| timeline.sample(state.position))
                    } else {
                        timeline.sample(state.position)
                    };
                    (state, values)
                })
            })
            .collect()
    }

    /// Logical values persist after a timeline finishes. Difference tracks
    /// are overlays for evaluation and must not leak into GetVariable.
    pub fn evaluated_variables(&self) -> BTreeMap<String, f32> {
        let mut values: BTreeMap<_, _> = self
            .variables
            .iter()
            .map(|(name, state)| (name.clone(), state.value))
            .collect();
        for (label, tracks) in &self.difference_tracks {
            let Some(timeline) = self.timelines.get(label) else {
                continue;
            };
            for (name, track) in tracks {
                *values.entry(name.clone()).or_default() += track.value * timeline.weight;
            }
        }
        values
    }

    fn issue_ordinary_frame(
        &mut self,
        source: &crate::EmoteTimelineTrack,
        index: usize,
        time: f32,
    ) {
        let frame = &source.frames[index];
        if frame.hold {
            return;
        }
        let duration = source
            .frames
            .get(index + 1)
            .map_or(0.0, |next| (next.frame - time - 1.0).max(0.0));
        self.set_variable_target(
            &source.label,
            frame.value,
            duration,
            frame.easing.unwrap_or(0) as u32,
        );
    }

    fn seek_ordinary_timeline(&mut self, timeline: &crate::EmoteTimeline, time: f32) {
        let mut cursors = vec![usize::MAX; timeline.tracks.len()];
        for (index, source) in timeline.tracks.iter().enumerate() {
            if let Some(cursor) = source.frames.iter().rposition(|frame| frame.frame <= time) {
                cursors[index] = cursor;
                if let Some(command) = source.frames[..=cursor]
                    .iter()
                    .rposition(|frame| !frame.hold)
                {
                    self.issue_ordinary_frame(source, command, time);
                }
            }
        }
        self.ordinary_cursors
            .insert(timeline.label.clone(), cursors);
    }

    fn advance_ordinary_to(&mut self, timeline: &crate::EmoteTimeline, time: f32, inclusive: bool) {
        let mut cursors = self
            .ordinary_cursors
            .remove(&timeline.label)
            .unwrap_or_else(|| vec![usize::MAX; timeline.tracks.len()]);
        for (index, source) in timeline.tracks.iter().enumerate() {
            loop {
                let next = cursors[index].wrapping_add(1);
                let Some(frame) = source.frames.get(next) else {
                    break;
                };
                if !(frame.frame < time || (inclusive && frame.frame == time)) {
                    break;
                }
                self.issue_ordinary_frame(source, next, time);
                cursors[index] = next;
            }
        }
        self.ordinary_cursors
            .insert(timeline.label.clone(), cursors);
    }

    fn advance_ordinary_timeline(&mut self, timeline: &crate::EmoteTimeline, old: f32, new: f32) {
        let span = timeline.loop_end - timeline.loop_begin;
        if span <= 0.0 {
            self.advance_ordinary_to(timeline, new, true);
            return;
        }
        let mut position = if old >= timeline.loop_end {
            timeline.loop_begin + (old - timeline.loop_begin).rem_euclid(span)
        } else {
            old
        };
        let mut remaining = (new - old).max(0.0);
        while remaining > 0.0 {
            let step = remaining.min(timeline.loop_end - position);
            position += step;
            self.advance_ordinary_to(timeline, position, position < timeline.loop_end);
            remaining -= step;
            if position >= timeline.loop_end {
                position = timeline.loop_begin;
                self.seek_ordinary_timeline(timeline, position);
            }
        }
    }

    fn initialize_difference_timeline(&mut self, timeline: &crate::EmoteTimeline) {
        self.initialize_difference_timeline_at(timeline, 0.0);
    }

    fn initialize_difference_timeline_at(
        &mut self,
        timeline: &crate::EmoteTimeline,
        position: f32,
    ) {
        let tracks = timeline
            .tracks
            .iter()
            .filter_map(|track| {
                if track.frames.is_empty() {
                    return None;
                }
                let cursor = track
                    .frames
                    .iter()
                    .rposition(|frame| frame.frame <= position)
                    .unwrap_or(usize::MAX);
                Some((
                    track.label.clone(),
                    DifferenceTrackState {
                        value: 0.0,
                        start: 0.0,
                        target: 0.0,
                        elapsed: 0.0,
                        duration: 0.0,
                        cursor,
                    },
                ))
            })
            .collect();
        self.difference_tracks
            .insert(timeline.label.clone(), tracks);
        let mut tracks = self
            .difference_tracks
            .remove(&timeline.label)
            .unwrap_or_default();
        for source in &timeline.tracks {
            let Some(track) = tracks.get_mut(&source.label) else {
                continue;
            };
            if track.cursor != usize::MAX {
                issue_difference_frame(source, track.cursor, position, track);
            }
        }
        self.difference_tracks
            .insert(timeline.label.clone(), tracks);
    }

    fn advance_difference_timeline(
        &mut self,
        label: &str,
        timeline: &crate::EmoteTimeline,
        old_position: f32,
        new_position: f32,
        delta: f32,
    ) {
        if !delta.is_finite() || delta <= 0.0 {
            return;
        }
        if !self.difference_tracks.contains_key(label) {
            self.initialize_difference_timeline(timeline);
        }
        let Some(mut tracks) = self.difference_tracks.remove(label) else {
            return;
        };

        // Difference timelines issue a timed target only when a frame cursor
        // crosses a keyframe. The target transition is then advanced by the
        // complete host delta, matching the native Timeline Step routine.
        //
        // The timeline position grows monotonically even for looping
        // timelines; fold both endpoints back into the loop before walking,
        // otherwise every call after the first wrap would seek back to
        // loop_begin and replay only its first frame.
        let (old_position, new_position) = if timeline.loop_end > timeline.loop_begin {
            let span = timeline.loop_end - timeline.loop_begin;
            let shift = ((old_position - timeline.loop_begin) / span).floor() * span;
            if shift > 0.0 {
                (old_position - shift, new_position - shift)
            } else {
                (old_position, new_position)
            }
        } else {
            (old_position, new_position)
        };
        let mut cursor = old_position;
        let mut remaining = (new_position - old_position).max(0.0);
        if timeline.loop_end > timeline.loop_begin {
            while remaining > 0.0 {
                let to_end = (timeline.loop_end - cursor).max(0.0);
                if to_end <= f32::EPSILON {
                    seek_difference_timeline(timeline, &mut tracks, timeline.loop_begin);
                    cursor = timeline.loop_begin;
                    continue;
                }
                let step = remaining.min(to_end);
                let endpoint = cursor + step;
                advance_difference_to(
                    timeline,
                    &mut tracks,
                    endpoint,
                    endpoint < timeline.loop_end,
                );
                cursor = endpoint;
                remaining -= step;
                if step >= to_end - f32::EPSILON {
                    seek_difference_timeline(timeline, &mut tracks, timeline.loop_begin);
                    cursor = timeline.loop_begin;
                }
            }
        } else {
            advance_difference_to(timeline, &mut tracks, new_position, true);
        }
        for track in tracks.values_mut() {
            advance_difference_track(track, delta);
        }
        self.difference_tracks.insert(label.to_owned(), tracks);
    }
    fn fade_timeline(
        &mut self,
        label: String,
        target: f32,
        frames: f32,
        easing: u32,
        fade_in: bool,
    ) {
        let frames = frames.max(0.0);
        let state = self
            .timelines
            .entry(label.clone())
            .or_insert_with(|| TimelineState {
                label: label.clone(),
                flags: 0,
                position: 0.0,
                weight: if fade_in { 0.0 } else { 1.0 },
                target_weight: target,
                remaining_frames: frames,
                easing,
            });
        state.target_weight = target;
        state.remaining_frames = frames;
        state.easing = easing;
        if frames == 0.0 {
            state.weight = target;
        }

        self.commands.push_back(if fade_in {
            EmoteCommand::FadeInTimeline {
                label,
                frames,
                easing,
            }
        } else {
            EmoteCommand::FadeOutTimeline {
                label,
                frames,
                easing,
            }
        });
    }
}

fn issue_difference_frame(
    source: &crate::EmoteTimelineTrack,
    index: usize,
    command_time: f32,
    track: &mut DifferenceTrackState,
) {
    let Some(frame) = source.frames.get(index) else {
        return;
    };
    track.cursor = index;
    if frame.hold {
        return;
    }
    track.target = frame.value;
    track.start = track.value;
    track.elapsed = 0.0;
    // Native SetVariableDiff uses the next authored frame minus the current
    // command time and one exclusive tick. The command time matters when a
    // large host step crosses several keyframes in one update.
    track.duration = source
        .frames
        .get(index + 1)
        .map(|next| (next.frame - command_time - 1.0).max(0.0))
        .unwrap_or(0.0);
    if track.duration <= 0.0 {
        track.value = frame.value;
    }
}

fn seek_difference_timeline(
    timeline: &crate::EmoteTimeline,
    tracks: &mut BTreeMap<String, DifferenceTrackState>,
    position: f32,
) {
    for source in &timeline.tracks {
        let Some(track) = tracks.get_mut(&source.label) else {
            continue;
        };
        let cursor = source
            .frames
            .iter()
            .rposition(|frame| frame.frame <= position)
            .unwrap_or(usize::MAX);
        track.cursor = cursor;
        if cursor == usize::MAX {
            continue;
        }
        // A hold marker is a cursor-only frame. Native seek still reissues
        // the latest preceding writable frame at the seek time.
        if let Some(index) = source.frames[..=cursor]
            .iter()
            .rposition(|frame| !frame.hold)
        {
            issue_difference_frame(source, index, position, track);
            track.cursor = cursor;
        }
    }
}

fn advance_difference_to(
    timeline: &crate::EmoteTimeline,
    tracks: &mut BTreeMap<String, DifferenceTrackState>,
    position: f32,
    inclusive: bool,
) {
    for source in &timeline.tracks {
        let Some(track) = tracks.get_mut(&source.label) else {
            continue;
        };
        loop {
            let index = if track.cursor == usize::MAX {
                0
            } else {
                track.cursor + 1
            };
            let Some(frame) = source.frames.get(index) else {
                break;
            };
            let crossed = if inclusive {
                frame.frame <= position
            } else {
                frame.frame < position
            };
            if !crossed {
                break;
            }
            issue_difference_frame(source, index, position, track);
        }
    }
}

fn advance_difference_track(track: &mut DifferenceTrackState, delta: f32) {
    if track.duration <= 0.0 {
        track.value = track.target;
        return;
    }
    track.elapsed = (track.elapsed + delta).min(track.duration);
    let ratio = (track.elapsed / track.duration).clamp(0.0, 1.0);
    // Timeline easing is uncommon for model-wide difference tracks. Linear
    // interpolation keeps this hot path allocation-free and matches the
    // default native easing used by the shipped models.
    track.value = track.start + (track.target - track.start) * ratio;
}

fn advance_scalar(value: &mut f32, target: f32, remaining: &mut f32, frames: f32) {
    if *remaining <= 0.0 {
        *value = target;
        return;
    }
    if frames >= *remaining {
        *value = target;
        *remaining = 0.0;
        return;
    }
    let ratio = frames / *remaining;
    *value += (target - *value) * ratio;
    *remaining -= frames;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_nekomiko_script_command_sequence() {
        let mut player = EmotePlayer::default();
        player.set_scale(0.6, 0.0, 0.0);
        player.set_coord(0.0, 720.0, 0.0, 0.0);
        player.pass();
        player.play_timeline("笑顔_ボイス再生用", 1);
        player.fade_in_timeline("通常待機", 0.0, 0);
        player.set_variable("face_talk", 0.75, 0.0, 0);

        assert_eq!(player.transform().scale, [0.6, 0.0, 0.0]);
        assert_eq!(player.transform().coord, [0.0, 720.0, 0.0, 0.0]);
        assert_eq!(player.variables()["face_talk"].value, 0.75);
        assert_eq!(player.timelines()["通常待機"].weight, 1.0);
        assert_eq!(player.take_commands().count(), 6);
    }

    #[test]
    fn advances_variable_tween_without_using_wall_clock_time() {
        let mut player = EmotePlayer::default();
        player.set_variable("face_talk", 1.0, 10.0, 0);
        player.advance(4.0);
        assert!((player.variables()["face_talk"].value - 0.4).abs() < 0.0001);
        player.advance(6.0);
        assert_eq!(player.variables()["face_talk"].value, 1.0);
    }

    #[test]
    fn parallel_expression_only_writes_at_authored_frames() {
        use crate::{EmoteKeyframe, EmoteTimeline, EmoteTimelineTrack};
        let mut old = EmoteTimeline {
            label: "z-first".into(),
            diff: false,
            last_time: -1.0,
            loop_begin: -1.0,
            loop_end: -1.0,
            tracks: vec![EmoteTimelineTrack {
                label: "face".into(),
                frames: vec![
                    EmoteKeyframe {
                        frame: 0.0,
                        hold: false,
                        value: 8.0,
                        easing: None,
                    },
                    EmoteKeyframe {
                        frame: 20.0,
                        hold: true,
                        value: 0.0,
                        easing: None,
                    },
                ],
            }],
        };
        let mut next = old.clone();
        next.label = "a-second".into();
        next.tracks[0].frames = vec![EmoteKeyframe {
            frame: 0.0,
            hold: false,
            value: 3.0,
            easing: None,
        }];
        let mut player = EmotePlayer::default();
        player.play_timeline(old.label.clone(), 1);
        player.seek_ordinary_timeline(&old, 0.0);
        player.advance(19.0);
        assert_eq!(player.variables()["face"].value, 8.0);
        player.play_timeline(next.label.clone(), 1);
        player.seek_ordinary_timeline(&next, 0.0);
        player.advance_ordinary_timeline(&old, 19.0, 50.0);
        player.advance(31.0);
        assert_eq!(
            player.variables()["face"].value,
            3.0,
            "the older timeline's hold marker must not restore its old pose"
        );
        player.set_variable("face", 5.0, 0.0, 0);
        player.advance_ordinary_timeline(&next, 0.0, 50.0);
        assert_eq!(
            player.variables()["face"].value,
            5.0,
            "a finished timeline must not undo a script setter"
        );
        old.tracks[0].frames.push(EmoteKeyframe {
            frame: 60.0,
            hold: false,
            value: 9.0,
            easing: None,
        });
        player.advance_ordinary_timeline(&old, 50.0, 60.0);
        assert_eq!(
            player.variables()["face"].value,
            9.0,
            "a later authored keyframe may write again"
        );
    }

    #[test]
    fn looping_difference_timeline_keeps_advancing_past_the_wrap() {
        use crate::{EmoteKeyframe, EmoteTimeline, EmoteTimelineTrack};
        let timeline = EmoteTimeline {
            label: "idle".into(),
            diff: true,
            last_time: 300.0,
            loop_begin: 0.0,
            loop_end: 300.0,
            tracks: vec![EmoteTimelineTrack {
                label: "body_UD".into(),
                frames: vec![
                    EmoteKeyframe {
                        frame: 0.0,
                        hold: false,
                        value: 0.0,
                        easing: None,
                    },
                    EmoteKeyframe {
                        frame: 1.0,
                        hold: false,
                        value: -15.0,
                        easing: None,
                    },
                    EmoteKeyframe {
                        frame: 150.0,
                        hold: false,
                        value: 30.0,
                        easing: None,
                    },
                ],
            }],
        };
        let mut player = EmotePlayer::default();
        player.initialize_difference_timeline(&timeline);
        let value = |player: &EmotePlayer| player.difference_tracks["idle"]["body_UD"].value;
        let mut position = 0.0;
        // 跨过第一次回绕后，游标必须继续前进而不是每帧重新 seek 回 loop_begin。
        let mut wrapped_samples = Vec::new();
        for _ in 0..620 {
            let old = position;
            position += 1.0;
            player.advance_difference_timeline("idle", &timeline, old, position, 1.0);
            if position > 310.0 {
                wrapped_samples.push(value(&player));
            }
        }
        let unique: std::collections::BTreeSet<_> = wrapped_samples
            .iter()
            .map(|value| value.to_bits())
            .collect();
        assert!(
            unique.len() > 100,
            "looped difference timeline froze after the wrap: {wrapped_samples:?}"
        );
    }
}
