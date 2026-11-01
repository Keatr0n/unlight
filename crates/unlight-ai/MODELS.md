# Model choices

The bar: Unlight is GPL-3.0 and may be distributed, so weights marked non-commercial or
research-only are out, and so are file formats that can run code (pickle). The weights are never
shipped: `models.toml` lists a URL and a hash, and each user downloads the file on request.

Licences below were read from the source on 2026-10-31. Training data was checked too, because a
permissive licence on weights doesn't settle what the data they came from allows. This is an
engineering record, not legal advice.

## Chosen

| Task | Model | Size | Weights licence | Training data |
|---|---|---|---|---|
| Select Subject | U²-Net (`u2net`) | 176 MB | Apache-2.0, the repo's `LICENSE` | DUTS-TR. Its page says rights are reserved by its authors and states no non-commercial limit |
| Select Sky | U²-Net sky (`skyseg`) | 176 MB | MIT, the repo's `LICENSE` | **Undisclosed** |
| AI Denoise | NAFNet-SIDD width 64 (`nafnet-sidd`) | 468 MB | MIT, the repo's `LICENSE` | SIDD, which its site puts under MIT |

Open questions on these, none of them blocking:

- **`u2net` and `nafnet-sidd` are third-party ONNX conversions.** Upstream publishes only `.pth`
  files, which are pickles and unacceptable here. The conversions come from rembg's release and
  deepghs' repository. Their hashes are pinned, but nobody upstream vouches for them.
- **`skyseg` has the weakest provenance.** The author trained it and linked a 167 MB ONNX file from
  an MIT repo, but it is only on Baidu. The copy here is a third party's re-host, which also says
  MIT. It couldn't be compared with the original. Its training set isn't stated, and the author
  said in 2021 that the high-precision model was used in their product, before linking it later
  that year. No alternative met the bar, see below.

## Why BiRefNet isn't the default

BiRefNet lite was the first choice: MIT, 224 MB, and a first-party ONNX whose SHA-256 matches
`onnx-community/BiRefNet_lite-ONNX`. It is a little better than U²-Net, which missed the lantern on
the Canon 5D IV frame.

Its weights were trained on data including DIS5K (`DIS5K-TR`, `DIS-TEs`), and the
[DIS5K terms](https://github.com/xuebinqin/DIS/blob/main/DIS5K-Dataset-Terms-of-Use.pdf) say the
dataset is "for non-commercial use in research or educational purpose" and that "commercial use of
this dataset is prohibited even after copying, editing, processing or any operations". Whether that
reaches weights that are themselves MIT is a legal question. It also took 3.2 s per run against a
3 s target, where U²-Net takes a fraction of a second, and the guided filter closes most of the
quality gap. If you decide the licence is fine, this restores it:

```toml
[[model]]
name = "birefnet-lite"
title = "BiRefNet lite (general)"
task = "subject"
url = "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx"
size = 224005088
sha256 = "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333"
licence = "MIT"
licence_url = "https://github.com/ZhengPeng7/BiRefNet/blob/main/LICENSE"

[model.network]
kind = "segmentation"
input_size = 1024
mean = [0.485, 0.456, 0.406]
std = [0.229, 0.224, 0.225]
output = "logits"
```

Put it first among the subject models, since `Manifest::for_task` takes the first match.

## Everything else checked

| Candidate | Finding | Verdict |
|---|---|---|
| RMBG 1.4 and 2.0 | Bria's licence, non-commercial without a paid agreement | Disqualified |
| EdgeSAM | S-Lab License 1.0, non-commercial | Disqualified |
| SegFormer (NVIDIA) | Section 3.3: "the Work and any derivative works" are "for research or evaluation purposes only". Covers the Xenova and onnx-community exports | Disqualified |
| MaskFormer | CC BY-NC 4.0, and so are its ONNX exports | Disqualified |
| `Realcat/skywater_seg` | Tagged MIT, but it is SegFormer MiT-B2 fine-tuned, so a derivative of the above unless proven otherwise | Disqualified |
| IS-Net | The README licenses only "our code and evaluation metric". Nothing covers the weights, which were trained on DIS5K | Not usable |
| MODNet | Apache-2.0 for code and models, but portraits only | Not suitable |
| SAM 2 | Apache-2.0 including checkpoints, but needs a prompt, so it can't select a subject by itself | Later, for click-to-select |
| Mask2Former | MIT, but no ONNX export was found | Not tested |
| SCUNet | Apache-2.0, 91 MB. Kept more fine detail than NAFNet on real ISO 12800 crops, but twice as slow. Trained on synthesised noise, and its base image sets weren't checked | The better picture if its data checks out |
| Restormer | MIT, but third-party ONNX copies disagree about their licence (OpenRAIL or MIT) | Not tested |

## Runtime

ONNX Runtime on the CPU, from nixpkgs (1.27.1, in the binary cache), through `ort` with
`load-dynamic`. Nothing links it or downloads it at build time, and it is opened only when a tool
is first used, so a machine without it gets an ordinary error.

The others weren't used because:

- **GPU inference:** amdgpu's 2 s job timeout applies here and wgpu is single-queue, so a long
  compute job risks a reset on the same device as the desktop. ONNX Runtime's WebGPU provider
  isn't in the nixpkgs build, and enabling it means building Dawn.
- **ncnn:** in nixpkgs, but it needs its own Rust binding and models converted to its format, which
  no one hosts for these weights.
- **`burn` and `wonnx`:** they need operator coverage these graphs may not get and would pull in
  a second wgpu. Neither was tried, because the CPU path met the target.
