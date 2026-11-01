# unlight

is a raw photo editor that's basically Lightroom Classic, except it runs on NixOS. I wanted Lightroom on my machine, that isn't a thing, so now this exists.

Also many apologies, because Claude wrote basically all of this. I gave it the brief, picked the cameras and the licence, and it went off with a pile of agents. tbh I wanted a raw editor that works more than I wanted to learn how to port an X-Trans demosaic to WGSL.

It's Rust, wgpu and egui, and every edit is stored as a recipe so your raws never get touched.

## Running

It's a flake, so just run

```shell
nix run .
```

If you don't want any of the AI stuff (fair enough), this gives you the same app with no AI code and no ONNX Runtime in it at all

```shell
nix build .#without-ai
```

You also get `unlight-cli`, which does most of it without a window. `info`, `render`, `export`, `merge`, `bench` and `compare-preview` are the good ones.

---

If you're not on nix, you'll need Vulkan, LibRaw, lensfun, lcms2, libjxl and libavif, plus whatever else is in `flake.nix`, then

```shell
cargo run --release -p unlight-app --bin unlight
```

I've never actually tried it outside of nix tho, so good luck.

No GPU? It'll run on Mesa's llvmpipe if you point `VK_DRIVER_FILES` at lavapipe's ICD file. It works, but it's not what you'd call fast.

## What's in it

**Library:** folders, collections, smart collections, keywords, ratings, flags, colour labels, filters, grid, loupe, compare and survey views, import, XMP sidecars and virtual copies.

**Develop:**

- White balance, the usual Basic sliders, Auto tone, parametric and point curves, HSL, black and white, colour grading and calibration
- Sharpening, noise reduction, Texture, Clarity and Dehaze
- Lens profiles, chromatic aberration, defringe, perspective, Upright and Guided Upright
- Crop and straighten, post-crop vignette and grain
- Brush, linear, radial, luminance range and colour range masks
- Healing, clone and red-eye
- History, snapshots, presets, copy and sync settings, before and after
- Soft proofing with printer ICC profiles and a gamut warning

**Photo Merge:** HDR and panorama. You get a linear `.ulr` file that develops like a normal raw, so you can still push it around afterwards.

**Export:** JPEG, TIFF, PNG, JPEG XL and AVIF, in batches, with presets, resizing, output sharpening, watermarks and file naming.

## Shortcuts

They're mostly the Lightroom ones, so your fingers should already know them.

`g`: **g**rid

`e`: loupe

`d`: **d**evelop

`p` / `x` / `u`: **p**ick, reject, **u**nflag

`0` to `5`: rating

`6` to `9`: colour label

`r`: crop

`q`: heal

`m`: **m**asks (`o` shows the mask overlay)

`j`: clipping warning

`s`: **s**oft proof (`shift+s` for the gamut warning)

`ctrl+u`: Auto tone

`ctrl+h` / `ctrl+m`: merge to **H**DR / pano

## Cameras

Anything LibRaw 0.22 can decode should load, but these are the ones that have been tested on real files

| Camera | Notes |
|---|---|
| Sony ARW | Corrections come from the makernotes |
| Canon CR3, CR2 | cRAW works too |
| Nikon NEF | HE and HE* can't be decoded. You get the embedded preview and a clear error, never garbage |
| Fujifilm RAF | Proper X-Trans demosaic |
| Pixel DNG | GainMap and warp opcodes get applied |
| Panasonic RW2 | Distortion is corrected by default, because Micro Four Thirds lenses kinda rely on it |

## Camera profiles

Adobe's profiles can't be shipped with this for obvious reasons, but if you've got any `.dcp` files just chuck them in `~/.local/share/unlight/profiles` and pick one from the Profile menu in Basic. You don't need to restart. Without one you get Unlight's own default look, which was fitted against camera JPEGs.

## AI

All optional, all local, nothing leaves your machine. You get Select Subject, Select Sky and AI Denoise running on ONNX Runtime, and the models only get downloaded when you ask for them.

It's CPU only because a long GPU job can trip amdgpu's 2 second timeout and take your desktop with it, and I'd rather not. That does mean AI Denoise takes about 5 and a half minutes on a 24 MP photo, so it's export and CLI only.

The licensing rabbit hole for each model is in [MODELS.md](crates/unlight-ai/MODELS.md). The short version is BiRefNet got left out because its training data bans commercial use.

## Is it fast tho

Yeah, pretty much. This is on a Ryzen AI 9 HX 370 with the Radeon 890M, so just the iGPU, on a quiet machine. The slider numbers are after 5 minutes of heat soak.

| Thing | Time |
|---|---|
| Dragging a slider on a 61 MP photo (GPU, p95) | 4.2 ms |
| Same but with every tool switched on | 9.9 ms |
| Adding 16 masks on top | +1.25 ms |
| Exporting a 61 MP photo | 1.6 s |
| Exporting a 100 MP photo | 3.3 s |
| Next photo, once it's preloaded | about 7 ms |
| Holding the arrow key, until it's editable (p95) | 470 ms |
| Batch exporting 1000 photos | 24 min, no GPU resets |

Those are GPU times, I haven't measured slider to actual screen on a real display yet.

## What's still not great

- The default look isn't quite there. It's meant to land near the camera's own JPEG and it's still further off than I'd like, and the Canon M6 II comes out too bright.
- Panoramas cap out at 16384 px because a photo has to fit in one texture, so ten 61 MP frames come out at 25% scale.
- Photo Merge has only been tested on synthetic sets made from real raws, plus some real 8-bit photos. It hasn't seen an actual raw bracket or raw pano yet.
- Healing uses round spots, so you won't be removing any cars.
- Auto tone is pretty conservative. It fixes badly exposed photos but barely touches good ones.
- With heavy sharpening or noise reduction turned off, the preview can sit slightly off the export.
- No HDR or wide gamut display output, it's all sRGB on screen.
- Hair in Select Subject is unproven, because nobody has run it on a portrait yet.
- It matches what Lightroom's controls do, not its exact pixels. Adobe's process isn't public, so that's as close as it gets.

## Working on it

```shell
nix develop
nix flake check
```

The flake check needs no sample files and runs the GPU tests on llvmpipe. Heads up that nix only sees tracked files, so if you add a shader and forget to `git add` it, `cargo build` will work and `nix build` won't.

Tests that need real raws skip themselves unless `UNLIGHT_SAMPLES` points at the files from `scripts/fetch-samples.sh`. There are also two slow ones you have to ask for, ideally on a quiet machine

```shell
export UNLIGHT_SAMPLES=~/.cache/unlight-samples
cargo test --release -p unlight-app --test latency -- --ignored --nocapture
cargo test --release -p unlight-app --test soak -- --ignored --nocapture
```

`unlight-cli hang-test` hangs the GPU on purpose to test recovery. It can take your whole desktop session down with it, so it makes you pass a flag saying you understand that. I still haven't run it, the normal tests just simulate losing the device.

If you change how an existing recipe renders, bump `PROCESS_VERSION` in `unlight-recipe` so old edits keep looking the same.

```
crates/
  unlight-raw-sys/      LibRaw bindings
  unlight-raw/          decoding, metadata, DNG opcodes, the .ulr format
  unlight-recipe/       versioned edit recipes
  unlight-color/        matrices, DCP, proofing colour
  unlight-lens/         lens profile resolution
  unlight-lensfun-sys/  lensfun bindings
  unlight-engine/       wgpu passes, scheduler, shaders
  unlight-merge/        HDR and panorama, CPU only
  unlight-output/       file writers, ICC, metadata
  unlight-catalog/      SQLite catalog, thumbnails, XMP
  unlight-ai/           optional local models
  unlight-cli/          headless tools and benchmarks
  unlight-app/          the UI, and the only crate that touches egui
```

## Licence

GPL-3.0. The Inter font is under the SIL Open Font License.
