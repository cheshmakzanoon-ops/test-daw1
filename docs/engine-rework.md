# Engine rework: how it closed

The engine rework (sampler voice, automation) closed under one rule: **the render path that ships stays the render
path**. The golden renders in `crates/os-engine/tests/golden/renders.txt` are the reference, and every decision below
keeps every fingerprint byte-identical. No user-visible behaviour changed.

## What changed structurally

- The engine sampler's kernel (`read_engine`, the 13.5-frame Blackman sinc) moved from `engine.rs` into
  `os_dsp::resample`, beside the hosted sampler's `read`. Both voice paths now take their interpolation from one module,
  which also documents exactly how the two kernels differ.
- `Engine::apply_levels` applies channel and mixer level edits in place instead of rebuilding the audio graph, with the
  render proven identical.
- The golden corpus covers the sampler, song clips and sends, pattern control points, in-place level edits, hosted
  instruments, automation clips, controller links, plugin-parameter automation and every non-plugin automation target.
  It is the gate for any further engine change.

## Decision: the two resampling kernels stay

The engine's kernel and Lemon Sampler's kernel differ in window shape and width, sinc argument and edge handling
(`os_dsp::resample::read_engine` documents the difference next to `read`). Moving either path onto the other's kernel
changes how shipped projects sound, and no golden may be re-blessed to make that compile. Both kernels are reachable,
so neither is retired or gated.

Reopening this needs a product decision: pick one kernel, accept the new sound, regenerate the golden renders and renew
the sampler's own expectations.

## Decision: plugin parameters keep sub-tick sampling

Automation clips on plugin parameters are sampled every `PARAM_STEP` (64) frames through `Curve::value_at` and the
clip's LFO, while every other target is walked once per tick and handed on one tick ahead. Routing parameters through
the tick walk would move their values, and the goldens, so `run_param_automation` stays as it is.

This is intentional, not deferred debt: the two laws exist because plugin parameters need frame-accurate curves while
the other targets need FL's tick walk. Reopening it likewise needs a product decision and regenerated goldens.

## What would reopen either decision

- Adopt one resampling kernel for both paths, as an intended sound change.
- Adopt one automation sampling law for every target, likewise as an intended sound change.
