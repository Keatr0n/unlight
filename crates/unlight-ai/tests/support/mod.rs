//! Builds tiny real ONNX files, so the ONNX Runtime path can be tested with no downloaded
//! weights.

use sha2::{Digest, Sha256};
use unlight_ai::{Model, Network, OutputScale, RestorationSpec, SegmentationSpec, Task};

const INTS: u64 = 7;
const INT: u64 = 2;
const FLOAT: u64 = 1;

fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn number(field: u64, value: u64, out: &mut Vec<u8>) {
    varint(field << 3, out);
    varint(value, out);
}

fn bytes(field: u64, payload: &[u8], out: &mut Vec<u8>) {
    varint(field << 3 | 2, out);
    varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

fn attribute(name: &str, kind: u64, field: u64, value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    bytes(1, name.as_bytes(), &mut out);
    number(field, value, &mut out);
    number(20, kind, &mut out);
    out
}

fn value_info(name: &str, dims: [u64; 4]) -> Vec<u8> {
    let mut shape = Vec::new();
    for dim in dims {
        let mut dimension = Vec::new();
        number(1, dim, &mut dimension);
        bytes(1, &dimension, &mut shape);
    }
    let mut tensor = Vec::new();
    number(1, FLOAT, &mut tensor);
    bytes(2, &shape, &mut tensor);
    let mut kind = Vec::new();
    bytes(1, &tensor, &mut kind);
    let mut out = Vec::new();
    bytes(1, name.as_bytes(), &mut out);
    bytes(2, &kind, &mut out);
    out
}

/// A model of one node, from a 1×3×N×N image to an output of the given channel count.
fn single_node(node: &[u8], side: u64, output_channels: u64) -> Vec<u8> {
    let mut graph = Vec::new();
    bytes(1, node, &mut graph);
    bytes(2, b"test", &mut graph);
    bytes(11, &value_info("image", [1, 3, side, side]), &mut graph);
    bytes(12, &value_info("result", [1, output_channels, side, side]), &mut graph);

    let mut opset = Vec::new();
    number(2, 13, &mut opset);
    let mut model = Vec::new();
    number(1, 8, &mut model);
    bytes(7, &graph, &mut model);
    bytes(8, &opset, &mut model);
    model
}

fn node(operation: &[u8]) -> Vec<u8> {
    let mut node = Vec::new();
    bytes(1, b"image", &mut node);
    bytes(2, b"result", &mut node);
    bytes(4, operation, &mut node);
    node
}

/// Turns an image into a mask of its brightness.
pub fn brightness_model(side: u64) -> Vec<u8> {
    let mut mean = node(b"ReduceMean");
    bytes(5, &attribute("axes", INTS, 8, 1), &mut mean);
    bytes(5, &attribute("keepdims", INT, 3, 1), &mut mean);
    single_node(&mean, side, 1)
}

/// Leaves an image exactly as it was, which exposes any seam between tiles.
pub fn identity_model(side: u64) -> Vec<u8> {
    single_node(&node(b"Identity"), side, 3)
}

/// Brightens an image by a known amount.
pub fn sqrt_model(side: u64) -> Vec<u8> {
    single_node(&node(b"Sqrt"), side, 3)
}

fn describe(bytes: &[u8], task: Task, network: Network) -> Model {
    Model {
        name: "generated".into(),
        title: "Generated".into(),
        task,
        url: "https://example.com/generated.onnx".into(),
        size: bytes.len() as u64,
        sha256: Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect(),
        licence: "MIT".into(),
        licence_url: "https://example.com/LICENSE".into(),
        network,
    }
}

pub fn segmentation(bytes: &[u8], side: u32) -> Model {
    let spec = SegmentationSpec {
        input_size: side,
        mean: [0.0; 3],
        std: [1.0; 3],
        output: OutputScale::Probability,
    };
    describe(bytes, Task::Subject, Network::Segmentation(spec))
}

pub fn restoration(bytes: &[u8], tile: u32, overlap: u32) -> Model {
    describe(bytes, Task::Denoise, Network::Restoration(RestorationSpec { tile, overlap }))
}

/// True when ONNX Runtime can be found. `nix flake check` and the dev shell both provide it.
pub fn runtime_available() -> bool {
    let available = std::env::var_os("ORT_DYLIB_PATH").is_some();
    if !available {
        eprintln!("skipping: ORT_DYLIB_PATH isn't set");
    }
    available
}
