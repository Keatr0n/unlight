//! ONNX Runtime, loaded when first needed rather than linked, so a build that never uses AI
//! never touches it and a missing library is an ordinary error.

use crate::{
    cancel::CancelToken,
    error::{Error, Result},
};
use ort::{
    environment::Environment,
    session::{RunOptions, Session, builder::GraphOptimizationLevel},
    value::TensorRef,
};
use std::{
    env,
    path::PathBuf,
    sync::{Arc, OnceLock},
    thread,
};

const LIBRARY_VARIABLE: &str = "ORT_DYLIB_PATH";
/// Set by the Nix build, which is what puts the library in the package's closure.
const LIBRARY_AT_BUILD: Option<&str> = option_env!("UNLIGHT_ONNXRUNTIME");

fn failed(error: impl ToString) -> Error {
    Error::Inference(error.to_string())
}

fn library_path() -> std::result::Result<PathBuf, String> {
    env::var_os(LIBRARY_VARIABLE)
        .map(PathBuf::from)
        .or_else(|| LIBRARY_AT_BUILD.map(PathBuf::from))
        .ok_or_else(|| format!("this build has no bundled copy, so set {LIBRARY_VARIABLE} to libonnxruntime.so"))
}

fn start_runtime() -> std::result::Result<Environment, String> {
    let path = library_path()?;
    let builder = ort::init_from(&path).map_err(|error| error.to_string())?;
    builder.with_telemetry(false).build().map_err(|error| error.to_string())
}

fn runtime() -> Result<&'static Environment> {
    static RUNTIME: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();
    RUNTIME.get_or_init(start_runtime).as_ref().map_err(|message| Error::RuntimeMissing(message.clone()))
}

/// One thread per physical core. Measured on a 12-core, 24-thread Ryzen AI 9 HX 370, hyperthreads
/// made BiRefNet slower: 3.2 s at 12 threads, 3.4 s at 24.
fn inference_threads() -> usize {
    thread::available_parallelism().map_or(1, |threads| (threads.get() / 2).max(1))
}

pub(crate) struct Tensor {
    pub shape: Vec<i64>,
    pub values: Vec<f32>,
}

pub struct LoadedModel {
    session: Session,
    input: String,
}

impl LoadedModel {
    /// Builds a session from bytes the store has already verified.
    pub(crate) fn load(bytes: &[u8]) -> Result<Self> {
        let session = Session::builder(runtime()?)
            .map_err(failed)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(failed)?
            .with_intra_threads(inference_threads())
            .map_err(failed)?
            .commit_from_memory(bytes)
            .map_err(failed)?;
        let input = session.inputs().first().map(|input| input.name().to_owned());
        Ok(Self { input: input.ok_or_else(|| failed("the model has no inputs"))?, session })
    }

    /// Runs the network on one image tensor and returns its first output. Cancelling stops it
    /// between graph nodes, so even a long run ends promptly.
    pub(crate) fn run(&mut self, shape: [usize; 4], values: &[f32], cancel: &CancelToken) -> Result<Tensor> {
        cancel.check()?;
        let options = Arc::new(RunOptions::new().map_err(failed)?);
        let terminate = Arc::clone(&options);
        cancel.arm(move || {
            let _ = terminate.terminate();
        });
        let tensor = TensorRef::from_array_view((shape, values)).map_err(failed)?;
        let outcome = self.session.run_with_options(ort::inputs![self.input.as_str() => tensor], &*options);
        cancel.disarm();
        cancel.check()?;
        let outputs = outcome.map_err(failed)?;
        let first = outputs.values().next().ok_or_else(|| failed("the model has no outputs"))?;
        let (shape, values) = first.try_extract_tensor::<f32>().map_err(failed)?;
        Ok(Tensor { shape: shape.to_vec(), values: values.to_vec() })
    }
}
