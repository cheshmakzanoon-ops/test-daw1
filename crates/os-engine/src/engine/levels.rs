// Copyright (C) 2026 ItsAlphaNeon
// SPDX-License-Identifier: GPL-3.0-or-later
// This file is part of Lemon Studio: https://github.com/ItsAlphaNeon/Lemon-Studio

//! Level edits taken into an engine that is already running.
//!
//! Most edits made while the song plays move levels: a mixer fader, a channel's volume or pan knob, a mute switch, a
//! slot's LED or mix level, a send level, the filter knobs. None of them changes the audio graph, so none of them needs
//! the whole engine rebuilt and swapped in (`Engine::continue_from`). [`levels_only`] proves that nothing else differs
//! by comparing the two projects with exactly those fields blanked out; [`Engine::apply_levels`] then takes the new
//! values where the engine stands, gliding to them as automation does.
//!
//! Any other edit — a note, a clip, a route, an insert, a sample, the tempo — still rebuilds the engine, so a field
//! this module does not name can never be missed here.
//!
//! spec: ui/mixer.md, audio/voice-levels.md "Values from automation", audio/mixer.md "Changes while the song plays"

use os_model::{MixerTrack, Project};

use super::{mix_level, Adjust, Engine};

/// True when `new` differs from `old` in channel or mixer levels only (see the module docs): the fields
/// [`Engine::apply_levels`] writes are all that changed.
pub fn levels_only(old: &Project, new: &Project) -> bool {
    old == new || levels_view(old) == levels_view(new)
}

/// `p` with every field [`Engine::apply_levels`] takes blanked out, so two projects compare equal when nothing else
/// differs. The list is the same one `apply_levels` writes, and the tests keep the two in step.
fn levels_view(p: &Project) -> Project {
    let mut v = p.clone();
    for ch in &mut v.channels {
        ch.enabled = true;
        ch.volume = 0.0;
        ch.pan = 0.0;
        ch.misc.levels = [0.0; 4];
        ch.sound.cutoff = 0;
        ch.sound.resonance = 0;
    }
    for m in &mut v.mixer {
        erase_levels(m);
    }
    v
}

/// Blanks a mixer track's levels: the fader, pan, separation, the switches, the slots' LEDs and mix levels and the send
/// levels. Everything else (its routes, inserts, equaliser and latencies) stays, so a change there is not a level edit.
fn erase_levels(m: &mut MixerTrack) {
    m.enabled = true;
    m.volume = None;
    m.pan = None;
    m.stereo_separation = None;
    m.polarity = false;
    m.swap = false;
    m.slots_enabled = true;
    m.slot_on = [true; 10];
    m.slot_mix = [0.0; 10];
    m.send_levels.clear();
}

impl Engine {
    /// Takes `project`'s channel and mixer levels into the running engine, in place: the faders, pans, switches, slot
    /// LEDs and mix levels, send levels and the channels' volume, pan and filter knobs. The caller must have
    /// [`levels_only`] say that nothing else changed; `false` means the layout does not line up and the engine has to
    /// be rebuilt (the transport then does that).
    ///
    /// The values are the ones [`Engine::build`] works out from the project, written where it writes them: ringing
    /// voices ramp to a channel's new gain, and the mixer's gains glide to the new fader matrix
    /// (spec: audio/mixer.md "Changes while the song plays"). A level that automation drives is rewritten by it on the
    /// next tick, as after a rebuild.
    pub fn apply_levels(&mut self, project: &Project) -> bool {
        if project.channels.len() != self.channels.len() {
            return false;
        }
        // An empty mixer is one unity track.
        let tracks = if project.mixer.is_empty() { 1 } else { project.mixer.len() };
        if tracks != self.tracks.len() {
            return false;
        }
        for (ch, c) in project.channels.iter().zip(self.channels.iter_mut()) {
            let adjust = Adjust::of(ch);
            c.enabled = ch.enabled;
            c.volume = if ch.enabled { crate::laws::channel_gain(ch.volume) * f64::from(adjust.mult) } else { 0.0 };
            c.pan = ch.pan;
            // The filter knobs' positions and the level adjustments (spec: audio/voice-levels.md "The channel's
            // levels"): what the channel stores now, as a rebuild would take them.
            c.adjust = adjust;
            c.init_adjust = adjust;
            c.vol_v = ch.volume;
            c.init_v = ch.volume;
            c.init = (c.volume, ch.pan, c.init.2);
        }
        for (m, t) in project.mixer.iter().zip(self.tracks.iter_mut()) {
            t.vol = (m.volume.unwrap_or(0.8) * 16000.0).round() as i32;
            t.pan = (m.pan.unwrap_or(0.0) * 6400.0).round() as i32;
            t.sep = (m.stereo_separation.unwrap_or(0.0) * 64.0).round() as i32;
            t.on = m.enabled;
            t.swap = m.swap;
            t.polarity = m.polarity;
            t.slots_enabled = m.slots_enabled;
            for k in 0..10 {
                t.set_slot_on(k, m.slot_on[k]);
                t.set_slot_mix(k, mix_level(m.slot_mix[k]));
            }
            // A route without a level record sends at unity (spec: flp/engine-events.md "The mixer").
            for r in &mut t.routes {
                r.gain = m
                    .send_levels
                    .iter()
                    .find(|(d, _)| usize::from(*d) == r.dest)
                    .map_or(1.0, |&(_, l)| os_dsp::mixer::send_gain((l * 16000.0).round() as i32));
            }
            // The new targets; the gains glide there from where they are (as after `continue_from`).
            t.matrix = t.compute_matrix();
            t.store();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::SampleLibrary;
    use crate::PlayMode;
    use os_model::{Channel, ChannelKind, Note, Pattern};

    fn channel(index: u16) -> Channel {
        Channel {
            id: os_model::Id::NONE,
            index,
            kind: ChannelKind::Sampler,
            name: None,
            plugin: None,
            plugin_state: None,
            color: None,
            enabled: true,
            volume: 0.78125,
            pan: 0.0,
            pitch_cents: 0,
            mixer_track: 0,
            sample_path: None, // plays the built-in tone
            layer_children: vec![],
            envelopes: Default::default(),
            rack: Default::default(),
            misc: Default::default(),
            sound: Default::default(),
        }
    }

    fn base() -> Project {
        Project {
            channels: vec![channel(0)],
            mixer: vec![MixerTrack::new(0), MixerTrack::new(1)],
            patterns: vec![Pattern { number: 1, ..Pattern::default() }],
            ..Project::default()
        }
    }

    /// Every field `apply_levels` writes is blanked by `levels_view`: an edit to it alone stays a level edit.
    #[test]
    fn channel_and_mixer_levels_are_level_edits() {
        let a = base();
        let mut b = a.clone();
        b.channels[0].volume = 0.5;
        b.channels[0].pan = -0.25;
        b.channels[0].enabled = false;
        b.channels[0].misc.levels = [0.1, -0.2, 0.3, 0.4];
        b.channels[0].sound.cutoff = 200;
        b.channels[0].sound.resonance = 40;
        b.mixer[0].volume = Some(0.3);
        b.mixer[0].pan = Some(0.5);
        b.mixer[0].stereo_separation = Some(-0.5);
        b.mixer[0].enabled = false;
        b.mixer[0].swap = true;
        b.mixer[0].polarity = true;
        b.mixer[0].slots_enabled = false;
        b.mixer[0].slot_on[2] = false;
        b.mixer[0].slot_mix[2] = 0.25;
        b.mixer[0].send_levels = vec![(1, 0.5)];
        assert!(levels_only(&a, &b), "levels only");
        assert_eq!(a, base(), "the comparison leaves the projects as they were");
    }

    /// Anything else — the schedule, a route, the equaliser, the sample, the tempo, the layout — rebuilds.
    #[test]
    fn everything_else_is_not_a_level_edit() {
        let a = base();
        let mut b = a.clone();
        b.patterns[0].notes.push(Note::step(0, 0));
        assert!(!levels_only(&a, &b), "a note");
        let mut b = a.clone();
        b.mixer[0].routes = vec![1];
        assert!(!levels_only(&a, &b), "a route");
        let mut b = a.clone();
        b.mixer[0].eq[0][0] = 100;
        assert!(!levels_only(&a, &b), "the equaliser");
        let mut b = a.clone();
        b.channels[0].sample_path = Some("kick.wav".into());
        assert!(!levels_only(&a, &b), "the sample");
        let mut b = a.clone();
        b.tempo = Some(100.0);
        assert!(!levels_only(&a, &b), "the tempo");
        let mut b = a.clone();
        b.channels.push(channel(1));
        assert!(!levels_only(&a, &b), "a channel");
        let mut b = a.clone();
        b.mixer[0].latency_out = 12.0;
        assert!(!levels_only(&a, &b), "a latency");
        assert!(levels_only(&a, &a), "nothing changed");
    }

    /// The point of the fast path: an engine that takes the levels in place renders exactly what one rebuilt from the
    /// same project and continued from its position does.
    #[test]
    fn a_level_edit_plays_like_a_rebuild() {
        let mut p = Project {
            ppq: 96,
            tempo: Some(130.0),
            channels: vec![channel(0), channel(1)],
            mixer: vec![MixerTrack::new(0), MixerTrack { routes: vec![0], ..MixerTrack::new(1) }],
            patterns: vec![Pattern {
                number: 1,
                notes: vec![
                    Note { channel: 0, position: 0, length: 96, key: 60, ..Note::step(0, 0) },
                    Note { channel: 1, position: 0, length: 96, key: 67, ..Note::step(1, 0) },
                ],
                ..Pattern::default()
            }],
            ..Project::default()
        };
        let before = p.clone();
        let mut library = SampleLibrary::new(None);
        let mut live = Engine::new(&p, &mut library, 48000, PlayMode::Pattern(1));
        let mut old = Engine::new(&p, &mut library, 48000, PlayMode::Pattern(1));
        let (mut a, mut b) = (Vec::new(), Vec::new());
        live.render_ticks(48, &mut a);
        old.render_ticks(48, &mut b);
        assert_eq!(a, b, "the two engines start out the same");
        // Levels moved while the song plays: a channel's volume, pan and mute, a fader, a send.
        p.channels[0].volume = 0.4;
        p.channels[0].pan = -0.3;
        p.channels[1].enabled = false;
        p.mixer[1].volume = Some(0.5);
        p.mixer[1].pan = Some(0.25);
        p.mixer[1].send_levels = vec![(0, 0.7)];
        assert!(levels_only(&before, &p), "levels only");
        assert!(live.apply_levels(&p), "applied in place");
        let mut rebuilt = Engine::new(&p, &mut library, 48000, PlayMode::Pattern(1));
        rebuilt.continue_from(&mut old);
        let (mut c, mut d) = (Vec::new(), Vec::new());
        live.render_ticks(48, &mut c);
        rebuilt.render_ticks(48, &mut d);
        assert_eq!(c, d, "in place and rebuilt engines render the same");
    }
}
