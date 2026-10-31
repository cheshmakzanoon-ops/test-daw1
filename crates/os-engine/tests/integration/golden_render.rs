// Copyright (C) 2026 ItsAlphaNeon
// SPDX-License-Identifier: GPL-3.0-or-later
// This file is part of Lemon Studio: https://github.com/ItsAlphaNeon/Lemon-Studio

//! Golden renders: a corpus of projects the engine plays whole, with a fingerprint of every 8 ticks committed beside
//! this test. A change that moves a single sample fails it, which is what lets the engine rework (one sampler voice
//! path, one automation pipeline, edits without a rebuild) be proven bit-identical instead of re-listened by ear.
//!
//! The cases use no files: a sampler without a sample plays the built-in tone, and the hosted cases play a sine
//! instrument and a gain effect of the test's own, so the corpus runs anywhere (no audio device, no writable temporary directory) and is
//! deterministic from run to run. The test renders every case twice and compares the two runs first, so a corpus case
//! that stops being deterministic fails here rather than in a confusing fingerprint diff.
//!
//! Fingerprints live in `tests/golden/renders.txt`, one line per chunk of 8 ticks:
//!
//! ```text
//! # case<TAB>chunk<TAB>frames<TAB>hash
//! pattern_sampler<TAB>0<TAB>12288<TAB>0123456789abcdef
//! ```
//!
//! `hash` is a 64-bit FNV-1a of the chunk's interleaved stereo `f32` bit patterns and `frames` its frame count.
//! Regenerate the file after an intended change with
//! `OS_GOLDEN_UPDATE=1 cargo test -p os-engine --test integration golden` and read the diff: every changed line is
//! audio that changed.
//!
//! spec: audio/timing.md, audio/mixer.md, audio/pattern-controls.md, audio/song-structure.md

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use os_engine::effects::peak_ctrl;
use os_engine::{Engine, HostedEffect, Instrument, PlayMode, SampleLibrary, SharedEffect, SharedInstrument};
use os_model::{Arrangement, AutoPoint, Automation, Channel, ChannelKind, Clip, ClipSource, ControllerLink, ControlPoint, MixerTrack, Note, Pattern, Project, SlotPlugin};

/// The rate every case renders at.
const RATE: u32 = 48_000;
/// Ticks per fingerprint line: long enough to keep the file small, short enough to point at where a difference starts.
const CHUNK_TICKS: usize = 8;
/// The committed fingerprints (`crates/os-engine/tests/golden/renders.txt`).
const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/renders.txt");

/// One corpus case: a project, how it is played, and optionally a level edit taken in place halfway through.
struct Case {
    name: &'static str,
    project: Project,
    mode: PlayMode,
    ticks: usize,
    instruments: Vec<(usize, SharedInstrument)>,
    /// Hosted effects, as (mixer track, slot, instance), set before the first tick.
    effects: Vec<(usize, usize, SharedEffect)>,
    /// The levels to take in with `Engine::apply_levels` after the first `ticks / 2` ticks.
    level_edit: Option<Project>,
}

/// One fingerprint line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    case: String,
    chunk: usize,
    frames: usize,
    hash: u64,
}

/// A 64-bit FNV-1a over the samples' bit patterns: any changed sample changes the fingerprint.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn samples(&mut self, samples: &[f32]) {
        for s in samples {
            self.bytes(&s.to_bits().to_le_bytes());
        }
    }
}

/// A channel as the model builds one: a sampler with no sample file, which plays the engine's built-in tone.
fn channel(index: u16, mixer_track: u16) -> Channel {
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
        mixer_track,
        sample_path: None,
        layer_children: vec![],
        envelopes: Default::default(),
        rack: Default::default(),
        misc: Default::default(),
        sound: Default::default(),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The corpus. A case that is added here needs its fingerprints generated (see the module docs) and reviewed.

/// Two sampler channels over two mixer tracks, notes at different keys, velocities and lengths.
fn pattern_sampler() -> Project {
    let mut p = Project {
        ppq: 96,
        tempo: Some(130.0),
        channels: vec![channel(0, 0), channel(1, 1)],
        mixer: vec![MixerTrack::new(0), MixerTrack::new(1)],
        patterns: vec![Pattern {
            number: 1,
            notes: vec![
                Note { length: 24, key: 60, velocity: 100, ..Note::step(0, 0) },
                Note { length: 36, key: 67, velocity: 80, ..Note::step(0, 24) },
                Note { length: 48, key: 72, velocity: 112, ..Note::step(1, 12) },
                Note { length: 12, key: 55, velocity: 64, ..Note::step(1, 60) },
            ],
            ..Pattern::default()
        }],
        ..Project::default()
    };
    p.channels[1].pan = 0.35;
    p.mixer[1].volume = Some(0.7);
    p.mixer[1].pan = Some(-0.4);
    p
}

/// Song mode: two pattern clips on the playlist, three mixer tracks, one of them sending to the master, one switched
/// off, and separation and panning on the way.
fn song_clips_and_sends() -> Project {
    let mut p = Project {
        ppq: 96,
        tempo: Some(140.0),
        channels: vec![channel(0, 0), channel(1, 1), channel(2, 2)],
        mixer: vec![MixerTrack::new(0), MixerTrack::new(1), MixerTrack::new(2)],
        patterns: vec![
            Pattern {
                number: 1,
                notes: vec![
                    Note { length: 24, key: 60, velocity: 100, ..Note::step(0, 0) },
                    Note { length: 48, key: 64, velocity: 90, ..Note::step(1, 24) },
                ],
                ..Pattern::default()
            },
            Pattern {
                number: 2,
                notes: vec![
                    Note { length: 36, key: 67, velocity: 96, ..Note::step(0, 0) },
                    Note { length: 72, key: 52, velocity: 110, ..Note::step(2, 12) },
                ],
                ..Pattern::default()
            },
        ],
        arrangements: vec![Arrangement {
            id: os_model::Id::NONE,
            number: 1,
            name: None,
            clips: vec![Clip::new(0, 96, 0, ClipSource::Pattern(1)), Clip::new(96, 96, 1, ClipSource::Pattern(2))],
            tracks: vec![],
            markers: vec![],
        }],
        current_arrangement: Some(1),
        ..Project::default()
    };
    p.mixer[1].routes = vec![0];
    p.mixer[1].send_levels = vec![(0, 0.5)];
    p.mixer[1].stereo_separation = Some(0.5);
    p.mixer[2].enabled = false;
    p
}

/// Pattern control points (the piano roll's event panel): volume, pan and pitch, across the notes' span.
fn control_points() -> Project {
    let mut p = pattern_sampler();
    p.patterns[0].controls = vec![
        ControlPoint::new(0, 0, ControlPoint::VOLUME, 12800),
        ControlPoint::new(16, 0, ControlPoint::VOLUME, 6400),
        ControlPoint::new(32, 0, ControlPoint::PAN, 12800),
        ControlPoint::new(48, 0, ControlPoint::PITCH, 1200),
        ControlPoint::new(64, 0, ControlPoint::VOLUME, 12800),
    ];
    p
}

/// A level edit applied where the engine stands: the same project with channel and mixer levels moved.
fn level_edit_in_place() -> (Project, Project) {
    let p = pattern_sampler();
    let mut edited = p.clone();
    edited.channels[0].volume = 0.4;
    edited.channels[0].pan = -0.3;
    edited.channels[1].enabled = false;
    edited.mixer[1].volume = Some(0.5);
    edited.mixer[1].pan = Some(0.2);
    (p, edited)
}

/// A hosted instrument's channel beside a sampler: the notes reach an instance of the test's own sine.
fn hosted_sine() -> (Project, SharedInstrument) {
    let mut p = pattern_sampler();
    p.channels[0].kind = ChannelKind::Plugin;
    p.channels[0].plugin = Some("Test Sine".into());
    p.channels[0].pan = -0.2;
    let inst: SharedInstrument = Arc::new(Mutex::new(Sine::default()));
    (p, inst)
}

/// A deterministic generator: a sine per held key, cut at its note off, 0.1 of full scale.
struct Sine {
    /// (key, phase, velocity)
    voices: Vec<(u8, f32, f32)>,
    /// Parameter 0: a gain on the output, 0..1.
    level: f32,
    /// Parameter values still to take, at their frame in the next `process` call.
    pending: Vec<(u32, f64)>,
}

impl Default for Sine {
    fn default() -> Sine {
        Sine { voices: Vec::new(), level: 1.0, pending: Vec::new() }
    }
}

impl Instrument for Sine {
    fn note_on(&mut self, _offset: u32, key: u8, velocity: f32) {
        self.voices.retain(|v| v.0 != key);
        self.voices.push((key, 0.0, velocity));
    }

    fn note_off(&mut self, _offset: u32, key: u8) {
        self.voices.retain(|v| v.0 != key);
    }

    fn process(&mut self, rate: u32, left: &mut [f32], right: &mut [f32]) {
        let tau = std::f32::consts::TAU;
        let mut events = std::mem::take(&mut self.pending).into_iter().peekable();
        for i in 0..left.len() {
            // The values automation handed on before this call, at their own frames (a plugin's own smoothing is the
            // plugin's; this one steps at once).
            while let Some(&(at, level)) = events.peek() {
                if at as usize <= i {
                    self.level = level as f32;
                    events.next();
                } else {
                    break;
                }
            }
            let mut s = 0.0;
            for v in &mut self.voices {
                let hz = 440.0 * 2f32.powf((f32::from(v.0) - 69.0) / 12.0);
                s += v.2 * 0.1 * v.1.sin();
                v.1 = (v.1 + tau * hz / rate as f32) % tau;
            }
            left[i] = s * self.level;
            right[i] = s * self.level;
        }
    }

    fn active(&self) -> bool {
        !self.voices.is_empty()
    }

    fn reset(&mut self) {
        self.voices.clear();
    }

    fn param(&mut self, offset: u32, index: u32, value: f64) {
        if index == 0 {
            self.pending.push((offset, value));
        }
    }
}

/// A slot effect with one parameter: parameter 0 scales the track, so a clip moving it is audible in the render.
struct Gain {
    level: f32,
    pending: Vec<(u32, f64)>,
}

impl Default for Gain {
    fn default() -> Gain {
        Gain { level: 1.0, pending: Vec::new() }
    }
}

impl HostedEffect for Gain {
    fn process_fx(&mut self, _rate: u32, left: &mut [f32], right: &mut [f32]) {
        let mut events = std::mem::take(&mut self.pending).into_iter().peekable();
        for i in 0..left.len() {
            while let Some(&(at, level)) = events.peek() {
                if at as usize <= i {
                    self.level = level as f32;
                    events.next();
                } else {
                    break;
                }
            }
            left[i] *= self.level;
            right[i] *= self.level;
        }
    }

    fn param_fx(&mut self, offset: u32, index: u32, value: f64) {
        if index == 0 {
            self.pending.push((offset, value));
        }
    }

    fn param_range_fx(&self, index: usize) -> Option<(f64, f64)> {
        (index == 0).then_some((0.0, 1.0))
    }
}

/// Song mode with an automation clip: its curve drives a channel's volume and a mixer pan over four beats.
fn automation_clip() -> Project {
    let mut automation = channel(1, 1);
    automation.kind = ChannelKind::Automation;
    // A new clip's range, 0..1 (stored pan 0, volume 12800).
    (automation.pan, automation.volume) = (-1.0, 1.0);
    Project {
        ppq: 96,
        tempo: Some(130.0),
        channels: vec![channel(0, 0), automation],
        mixer: vec![MixerTrack::new(0), MixerTrack::new(1)],
        patterns: vec![Pattern {
            number: 1,
            notes: vec![
                Note { length: 48, key: 60, velocity: 100, ..Note::step(0, 0) },
                Note { length: 48, key: 67, velocity: 90, ..Note::step(0, 48) },
            ],
            ..Pattern::default()
        }],
        arrangements: vec![Arrangement {
            id: os_model::Id::NONE,
            number: 1,
            name: None,
            clips: vec![Clip::new(0, 192, 0, ClipSource::Pattern(1)), Clip::new(48, 144, 1, ClipSource::Channel(1))],
            tracks: vec![],
            markers: vec![],
        }],
        automations: vec![Automation {
            channel: 1,
            targets: vec![
                os_model::target::channel(0, os_model::target::CHANNEL_VOLUME),
                os_model::target::mixer(1, os_model::target::MIXER_PAN),
            ],
            points: vec![AutoPoint::new(0.0, 1.0), AutoPoint::new(4.0, 0.0)],
            header: vec![0; 21],
            tail: vec![],
            lfo: Default::default(),
        }],
        current_arrangement: Some(1),
        ..Project::default()
    }
}

/// A Peak Controller in a mixer slot driving two targets through controller links: its peak straight to a fader with
/// smoothing, and its LFO inverted to a channel's volume.
fn controller_links() -> Project {
    let mut p = pattern_sampler();
    p.mixer.push(MixerTrack::new(2));
    p.mixer[1].slots[0] = Some(SlotPlugin {
        id: os_model::Id::NONE,
        name: "Fruity Peak Controller".into(),
        state: peak_ctrl::Settings::default().to_state(),
    });
    let mut fader = ControllerLink::new(0x7000 + 64, 1, os_model::target::mixer(2, os_model::target::MIXER_VOLUME));
    fader.bytes[12] |= 0x02; // Smoothing on.
    let mut volume = ControllerLink::new(0x7000 + 64, 2, os_model::target::channel(0, os_model::target::CHANNEL_VOLUME));
    volume.formula = Some("1-Input".into());
    p.controller_links = vec![fader, volume];
    p
}

/// Song mode with plugin-parameter automation: one clip ramps a hosted instrument's parameter and a slot effect's,
/// so the path that samples a curve every 64 frames (and the one-tick-ahead rule) is pinned in the render itself.
fn plugin_automation() -> (Project, SharedInstrument) {
    let mut automation = channel(1, 0);
    automation.kind = ChannelKind::Automation;
    (automation.pan, automation.volume) = (-1.0, 1.0);
    let mut p = Project {
        ppq: 96,
        tempo: Some(130.0),
        channels: vec![channel(0, 1), automation],
        mixer: vec![MixerTrack::new(0), MixerTrack::new(1)],
        patterns: vec![Pattern {
            number: 1,
            notes: vec![Note { length: 384, key: 60, velocity: 100, ..Note::step(0, 0) }],
            ..Pattern::default()
        }],
        arrangements: vec![Arrangement {
            id: os_model::Id::NONE,
            number: 1,
            name: None,
            clips: vec![Clip::new(0, 384, 0, ClipSource::Pattern(1)), Clip::new(0, 384, 1, ClipSource::Channel(1))],
            tracks: vec![],
            markers: vec![],
        }],
        automations: vec![Automation {
            channel: 1,
            targets: vec![os_model::target::channel_param(0, 0), os_model::target::slot_param(1, 0, 0)],
            points: vec![AutoPoint::new(0.0, 1.0), AutoPoint::new(4.0, 0.2)],
            header: vec![0; 21],
            tail: vec![],
            lfo: Default::default(),
        }],
        current_arrangement: Some(1),
        ..Project::default()
    };
    p.channels[0].kind = ChannelKind::Plugin;
    p.channels[0].plugin = Some("Test Sine".into());
    p.mixer[1].slots[0] = Some(SlotPlugin { id: os_model::Id::NONE, name: "Test Gain".into(), state: vec![] });
    p.mixer[1].routes = vec![0];
    (p, Arc::new(Mutex::new(Sine::default())))
}

/// Song mode with one clip driving every non-plugin target kind: the tempo, a send, two equaliser bands of a track,
/// its stereo separation, and a channel's pan offset and volume multiplier.
fn automation_targets() -> Project {
    let mut automation = channel(1, 0);
    automation.kind = ChannelKind::Automation;
    (automation.pan, automation.volume) = (-1.0, 1.0);
    let mut p = Project {
        ppq: 96,
        tempo: Some(130.0),
        channels: vec![channel(0, 1), automation],
        mixer: vec![MixerTrack::new(0), MixerTrack::new(1)],
        patterns: vec![Pattern {
            number: 1,
            notes: vec![
                Note { length: 96, key: 60, velocity: 100, ..Note::step(0, 0) },
                Note { length: 96, key: 67, velocity: 80, ..Note::step(0, 96) },
            ],
            ..Pattern::default()
        }],
        arrangements: vec![Arrangement {
            id: os_model::Id::NONE,
            number: 1,
            name: None,
            clips: vec![Clip::new(0, 384, 0, ClipSource::Pattern(1)), Clip::new(0, 384, 1, ClipSource::Channel(1))],
            tracks: vec![],
            markers: vec![],
        }],
        automations: vec![Automation {
            channel: 1,
            targets: vec![
                os_model::target::TEMPO,
                os_model::target::send(1, 0),
                os_model::target::mixer(1, os_model::target::EQ_GAIN),
                os_model::target::mixer(1, os_model::target::EQ_FREQ + 1),
                os_model::target::mixer(1, os_model::target::MIXER_SEPARATION),
                os_model::target::channel(0, os_model::target::CHANNEL_PAN_OFFSET),
                os_model::target::channel(0, os_model::target::CHANNEL_VOLUME_MULT),
            ],
            points: vec![AutoPoint::new(0.0, 0.22), AutoPoint::new(2.0, 0.32), AutoPoint::new(4.0, 0.24)],
            header: vec![0; 21],
            tail: vec![],
            lfo: Default::default(),
        }],
        current_arrangement: Some(1),
        ..Project::default()
    };
    p.mixer[1].routes = vec![0];
    p
}

fn cases() -> Vec<Case> {
    let (hooked, sine) = hosted_sine();
    let (base, edited) = level_edit_in_place();
    let (automated, automated_sine) = plugin_automation();
    let gain: SharedEffect = Arc::new(Mutex::new(Gain::default()));
    vec![
        Case { name: "pattern_sampler", project: pattern_sampler(), mode: PlayMode::Pattern(1), ticks: 64, instruments: vec![], effects: vec![], level_edit: None },
        Case { name: "song_clips_and_sends", project: song_clips_and_sends(), mode: PlayMode::Song, ticks: 128, instruments: vec![], effects: vec![], level_edit: None },
        Case { name: "control_points", project: control_points(), mode: PlayMode::Pattern(1), ticks: 72, instruments: vec![], effects: vec![], level_edit: None },
        Case { name: "level_edit_in_place", project: base, mode: PlayMode::Pattern(1), ticks: 64, instruments: vec![], effects: vec![], level_edit: Some(edited) },
        Case { name: "hosted_sine", project: hooked, mode: PlayMode::Pattern(1), ticks: 40, instruments: vec![(0, sine)], effects: vec![], level_edit: None },
        Case { name: "automation_clip", project: automation_clip(), mode: PlayMode::Song, ticks: 160, instruments: vec![], effects: vec![], level_edit: None },
        Case { name: "controller_links", project: controller_links(), mode: PlayMode::Pattern(1), ticks: 48, instruments: vec![], effects: vec![], level_edit: None },
        Case { name: "plugin_automation", project: automated, mode: PlayMode::Song, ticks: 128, instruments: vec![(0, automated_sine)], effects: vec![(1, 0, gain)], level_edit: None },
        Case { name: "automation_targets", project: automation_targets(), mode: PlayMode::Song, ticks: 128, instruments: vec![], effects: vec![], level_edit: None },
    ]
}

// ---------------------------------------------------------------------------------------------------------------
// Rendering and fingerprints.

fn render(case: &Case) -> Vec<Line> {
    let mut library = SampleLibrary::new(None);
    let mut engine = Engine::with_instruments(&case.project, &mut library, RATE, case.mode, &case.instruments);
    for (track, slot, fx) in &case.effects {
        engine.set_effect(*track, *slot, fx.clone());
    }
    let half = case.ticks / 2;
    let mut lines = Vec::new();
    let mut tick = 0;
    while tick < case.ticks {
        let n = CHUNK_TICKS.min(case.ticks - tick);
        let mut out = Vec::new();
        engine.render_ticks(n, &mut out);
        let mut hash = Fnv::new();
        hash.samples(&out);
        lines.push(Line { case: case.name.to_string(), chunk: tick / CHUNK_TICKS, frames: out.len() / 2, hash: hash.0 });
        tick += n;
        if let Some(edit) = case.level_edit.as_ref().filter(|_| tick == half) {
            assert!(engine.apply_levels(edit), "{}: the level edit is taken in place", case.name);
        }
    }
    lines
}

fn write_golden(lines: &[Line]) -> std::io::Result<()> {
    let mut text = String::from(
        "# Golden renders of the engine's corpus. Regenerate with `OS_GOLDEN_UPDATE=1 cargo test -p os-engine --test\n\
         # integration golden` after an intended change, and read the diff: every changed line is audio that changed.\n\
         # case\tchunk\tframes\thash\n",
    );
    for l in lines {
        text.push_str(&format!("{}\t{}\t{}\t{:016x}\n", l.case, l.chunk, l.frames, l.hash));
    }
    let path = PathBuf::from(GOLDEN);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
}

fn parse_golden(text: &str) -> Result<Vec<Line>, String> {
    let mut lines = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let bad = || format!("{}:{}: expected `case<TAB>chunk<TAB>frames<TAB>hash`, got {line:?}", GOLDEN, n + 1);
        if f.len() != 4 {
            return Err(bad());
        }
        lines.push(Line {
            case: f[0].to_string(),
            chunk: f[1].parse().map_err(|_| bad())?,
            frames: f[2].parse().map_err(|_| bad())?,
            hash: u64::from_str_radix(f[3], 16).map_err(|_| bad())?,
        });
    }
    Ok(lines)
}

#[test]
fn golden_renders_match_the_committed_fingerprints() {
    let mut actual = Vec::new();
    for case in cases() {
        let once = render(&case);
        let twice = render(&case);
        assert_eq!(once, twice, "{}: the same project renders the same samples twice", case.name);
        actual.extend(once);
    }
    if std::env::var_os("OS_GOLDEN_UPDATE").is_some() {
        write_golden(&actual).unwrap_or_else(|e| panic!("{GOLDEN}: {e}"));
        eprintln!("{}: wrote {} line(s) from {} case(s)", GOLDEN, actual.len(), cases().len());
        return;
    }
    let text = std::fs::read_to_string(GOLDEN)
        .unwrap_or_else(|e| panic!("{GOLDEN}: {e} (create it with OS_GOLDEN_UPDATE=1 cargo test -p os-engine --test integration golden)"));
    let expected = parse_golden(&text).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        expected.len(),
        actual.len(),
        "{}: {} line(s) committed, {} rendered (regenerate with OS_GOLDEN_UPDATE=1)",
        GOLDEN,
        expected.len(),
        actual.len()
    );
    for (want, got) in expected.iter().zip(&actual) {
        assert_eq!((&want.case, want.chunk), (&got.case, got.chunk), "{GOLDEN}: the corpus changed order or cases");
        assert_eq!(
            want.frames, got.frames,
            "{} chunk {}: {} frame(s) rendered, {} expected",
            got.case, got.chunk, got.frames, want.frames
        );
        assert_eq!(
            want.hash, got.hash,
            "{} chunk {}: rendered {:016x}, expected {:016x} — a sample changed; regenerate with OS_GOLDEN_UPDATE=1 \
             and review the diff",
            got.case, got.chunk, got.hash, want.hash
        );
    }
}
