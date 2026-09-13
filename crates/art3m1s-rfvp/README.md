# art3m1s-rfvp

RFVP integration boundary and backend-neutral frame adapter for Art3m1s.

The base crate has no dependency on `rfvp`, `art3m1s-core`, wgpu, FFI, or a
concrete GPU backend. Its protocol and `DrawList` conversion types can be
tested independently. The optional `rfvp-fork` feature adds direct access to
the maintained RFVP fork, and `host-runtime` adds the host-facing runtime used
by `art3m1s-core` for mounting resources, driving frames, forwarding input and
audio commands, and presenting through `art3m1s-render`.

Current mapping:

- `DrawImage` uses `vertices` as authoritative geometry.
- Axis-aligned images become a regular clipped quad.
- Rotated or warped images become a triangle-list `DrawMesh`.
- `SetClip` / `ClearClip` become `DrawCommand::clip_bounds`; texture UV
  cropping remains `ClipRect`.
- `Normal`, `Add`, `Sub`, and `Mul` map to `Alpha`, `Add`,
  `NativeReverseSubtract`, and `Multiply`.
- RFVP's per-draw `Nearest` / `Linear` filter is preserved. The adapter marks
  nearest sprites with the built-in `sprite-nearest` identity, and each shared
  backend selects the matching sampler without treating it as a runtime shader.
- `DrawGlyph` converts its destination rectangle and optional source rectangle.
- `DrawSolid` uses a pre-bound 1x1 white texture.
- `HitProxyTable` is returned unchanged to the host.

Explicit first-version limits:

- per-vertex colors must be uniform;
- `effect_id != 0` is rejected;
- negative clip or draw extents are rejected;

The `host-runtime` path consumes ordered texture create/update/destroy records,
validates their bounds and formats, and uploads them through `GpuBackend`.
Repeated same-size creates carry monotonically increasing generations; a
generation of 0 is treated as "untracked" and always re-uploaded, so streamed
content (dialogue text slots, dynamically recomposited UI textures) never
freezes on a stale generation. Presented frames are hashed (adapted commands
plus texture generations); identical frames skip GPU submission entirely, and
changed frames are repainted through `GpuBackend::render_damage` with a
conservative damage rect (positional diff, 2px inflation, full repaint past
80% coverage or on structural changes).

`RfvpHostRuntime` also exposes, over the fork's runtime ABI:

- full input: keyboard (arrows/enter/space/esc/tab/shift/ctrl/F1-F12),
  pointer move/buttons (left/right), wheel, touch, focus;
- disk-persistent saves rooted at the ABI `save_root` (default
  `<game>/save`), including `rfvp_global.bin`;
- pull-based events (`RfvpHostEvent::TextTranslation`) plus
  `set_text_replacements` (JSON blob), `set_text_translation_enabled`
  (also toggles the event queue), and `submit_text_translation` for
  asynchronous results;
- a rolling profiler (`set_profiler_enabled` / `profiler_snapshot_json`)
  emitting the same JSON schema as the core `RuntimeProfiler`.

The smoke binaries (`host-runtime-smoke` / `winit-smoke` features) accept
`RFVP_SMOKE_CLICK` / `RFVP_SMOKE_RCLICK` (`frame,x,y;...`),
`RFVP_SMOKE_KEY` (`frame,vk;...`), `RFVP_SMOKE_HIT_PROXIES`,
`RFVP_SMOKE_PROFILER`, and `RFVP_SMOKE_TRANSLATE` (auto-answers translation
requests with a marker string). `rfvp_frame_dump` prints raw ABI frames and
can dump texture pixels (`RFVP_DUMP_FRAMES`, `RFVP_DUMP_TEXTURE`).

Run validation with:

```sh
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
```
