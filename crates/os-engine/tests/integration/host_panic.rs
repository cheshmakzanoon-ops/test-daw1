// Copyright (C) 2026 ItsAlphaNeon
// SPDX-License-Identifier: GPL-3.0-or-later
// This file is part of Lemon Studio: https://github.com/ItsAlphaNeon/Lemon-Studio

//! A hosted plugin that panics while processing is caught at the engine's call site and quarantined: its channel plays
//! silence (an effect slot passes the sound through), the render continues, the instance is not entered again, and
//! `Engine::host_panics` counts it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use os_engine::{Engine, HostedEffect, Instrument, PlayMode, SampleLibrary, SharedInstrument};

const BLOCK: usize = 512;
const BLOCKS: usize = 16;

fn channel(index: u16) -> os_model::Channel {
    os_model::Channel {
        id: os_model::Id::NONE,
        index,
        kind: os_model::ChannelKind::Plugin,
        name: None,
        plugin: Some("Test".into()),
        plugin_state: None,
        color: None,
        enabled: true,
        volume: 0.78125,
        pan: 0.0,
        pitch_cents: 0,
        mixer_track: 0,
        sample_path: None,
        layer_children: vec![],
        envelopes: Default::default(),
        rack: Default::default(),
        misc: Default::default(),
        sound: Default::default(),
    }
}

fn project() -> os_model::Project {
    os_model::Project {
        ppq: 96,
        tempo: Some(130.0),
        channels: vec![channel(0), channel(1)],
        patterns: vec![],
        ..Default::default()
    }
}

/// A healthy instrument: a steady signal, so the surrounding audio is easy to check.
struct Tone;

impl Instrument for Tone {
    fn note_on(&mut self, _offset: u32, _key: u8, _velocity: f32) {}
    fn note_off(&mut self, _offset: u32, _key: u8) {}
    fn process(&mut self, _rate: u32, left: &mut [f32], right: &mut [f32]) {
        left.fill(0.25);
        right.fill(0.25);
    }
}

/// An instrument that panics the first time it is processed, and would again if the engine re-entered it.
struct PanicInst(Arc<AtomicUsize>);

impl Instrument for PanicInst {
    fn note_on(&mut self, _offset: u32, _key: u8, _velocity: f32) {}
    fn note_off(&mut self, _offset: u32, _key: u8) {}
    fn process(&mut self, _rate: u32, _left: &mut [f32], _right: &mut [f32]) {
        self.0.fetch_add(1, Ordering::Relaxed);
        panic!("test instrument panicked");
    }
}

/// A hosted effect that panics the first time it is processed, and would again if the engine re-entered it.
struct PanicFx(Arc<AtomicUsize>);

impl HostedEffect for PanicFx {
    fn process_fx(&mut self, _rate: u32, _left: &mut [f32], _right: &mut [f32]) {
        self.0.fetch_add(1, Ordering::Relaxed);
        panic!("test effect panicked");
    }
}

/// Two plugin channels, a tone on the first; a note starts on both, so the second is audible only if its instrument is.
fn base_engine() -> Engine {
    let p = project();
    let mut library = SampleLibrary::default();
    let mut e = Engine::new(&p, &mut library, 48_000, PlayMode::Pattern(1));
    e.set_instrument(0, Arc::new(Mutex::new(Tone)) as SharedInstrument);
    e.note_on(0, 60, 100);
    e.note_on(1, 64, 100);
    e
}

/// Renders `BLOCKS` blocks, one at a time, and returns them all.
fn render(e: &mut Engine) -> Vec<f32> {
    let mut out = vec![0.0; BLOCK];
    let mut all = Vec::with_capacity(BLOCK * BLOCKS);
    for _ in 0..BLOCKS {
        e.process(&mut out);
        all.extend_from_slice(&out);
    }
    all
}

/// The steady tone is not silent, and both renders agree sample for sample.
fn assert_matches_reference(actual: &[f32], reference: &[f32]) {
    let heard = reference.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    assert!(heard > 0.0, "the healthy channel must sound");
    assert_eq!(actual, reference, "the healthy render must be unchanged");
}

#[test]
fn panicking_instrument_is_quarantined_and_the_render_continues() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = base_engine();
    e.set_instrument(1, Arc::new(Mutex::new(PanicInst(calls.clone()))) as SharedInstrument);
    let mut reference = base_engine();

    let actual = render(&mut e);
    let expected = render(&mut reference);
    assert_matches_reference(&actual, &expected);

    assert_eq!(calls.load(Ordering::Relaxed), 1, "the quarantined instrument must not be entered again");
    assert_eq!(e.host_panics(), 1, "the panic must be counted and observable");
}

#[test]
fn panicking_effect_is_quarantined_and_passes_the_sound_through() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = base_engine();
    e.set_effect(0, 0, Arc::new(Mutex::new(PanicFx(calls.clone()))));
    let mut reference = base_engine();

    let actual = render(&mut e);
    let expected = render(&mut reference);
    assert_matches_reference(&actual, &expected);

    assert_eq!(calls.load(Ordering::Relaxed), 1, "the quarantined effect must not be entered again");
    assert_eq!(e.host_panics(), 1, "the panic must be counted and observable");
}
