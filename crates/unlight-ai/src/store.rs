//! Downloaded weights, under `$XDG_DATA_HOME/unlight/models`. A download is untrusted until its
//! hash matches the manifest: it stays under a `.part` name until then, and each model keeps to
//! its own directory.

use crate::{
    cancel::CancelToken,
    download::{self, Progress, Source},
    error::{Error, Result, io_error},
    manifest::Model,
    paths::data_directory,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelStatus {
    Missing,
    /// An earlier download stopped part way and can resume.
    Partial { received: u64 },
    Ready,
}

pub(crate) fn to_hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(io_error(path))?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(io_error(path))?;
    Ok(to_hex(&hasher.finalize()))
}

fn mismatch(path: &Path, model: &Model, actual: String) -> Error {
    Error::HashMismatch { path: path.to_owned(), expected: model.sha256.clone(), actual }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(io_error(path)(error)),
        _ => Ok(()),
    }
}

fn file_length(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|metadata| metadata.len())
}

fn lock_for_download(file: &File, model: &Model) -> Result<()> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(Error::DownloadInProgress(model.name.clone())),
        Err(TryLockError::Error(source)) => Err(Error::Io { path: model.name.clone().into(), source }),
    }
}

#[derive(Clone, Debug)]
pub struct ModelStore {
    root: PathBuf,
}

impl ModelStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn open_default() -> Result<Self> {
        Ok(Self::new(data_directory()?.join("models")))
    }

    fn directory(&self, model: &Model) -> PathBuf {
        self.root.join(&model.name)
    }

    pub fn path(&self, model: &Model) -> PathBuf {
        self.directory(model).join(format!("{}.onnx", model.sha256))
    }

    fn partial_path(&self, model: &Model) -> PathBuf {
        self.directory(model).join(format!("{}.part", model.sha256))
    }

    /// Goes by file size alone, so it is cheap enough to call while drawing. `load` is what
    /// proves a file is good.
    pub fn status(&self, model: &Model) -> ModelStatus {
        if file_length(&self.path(model)) == Some(model.size) {
            return ModelStatus::Ready;
        }
        match file_length(&self.partial_path(model)) {
            Some(received) if received > 0 => ModelStatus::Partial { received },
            _ => ModelStatus::Missing,
        }
    }

    /// Fetches a model the user asked for, picking up any earlier partial download. The file
    /// only takes its final name once its hash matches.
    pub fn download(
        &self,
        model: &Model,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<PathBuf> {
        let directory = self.directory(model);
        fs::create_dir_all(&directory).map_err(io_error(&directory))?;
        let path = self.path(model);
        if self.already_valid(model)? {
            return Ok(path);
        }

        let partial = self.partial_path(model);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&partial)
            .map_err(io_error(&partial))?;
        lock_for_download(&file, model)?;

        let existing = file.metadata().map_err(io_error(&partial))?.len();
        if existing > model.size {
            file.set_len(0).map_err(io_error(&partial))?;
        }
        if existing != model.size {
            let source = Source { url: &model.url, total: model.size };
            let fetched = download::fetch(&source, &mut file, &partial, on_progress, cancel);
            if matches!(fetched, Err(Error::TooLarge { .. })) {
                remove_if_present(&partial)?;
            }
            fetched?;
        }
        file.sync_all().map_err(io_error(&partial))?;

        let actual = hash_file(&partial)?;
        if actual != model.sha256 {
            remove_if_present(&partial)?;
            return Err(mismatch(&partial, model, actual));
        }
        fs::rename(&partial, &path).map_err(io_error(&path))?;
        Ok(path)
    }

    /// True if the model is in place and intact. A damaged copy is thrown away for re-download.
    fn already_valid(&self, model: &Model) -> Result<bool> {
        let path = self.path(model);
        if !path.exists() {
            return Ok(false);
        }
        if hash_file(&path)? == model.sha256 {
            return Ok(true);
        }
        remove_if_present(&path)?;
        Ok(false)
    }

    /// Reads a model and checks its hash. The caller gets the very bytes that were hashed, so
    /// the file can't change between the check and the load.
    pub fn load(&self, model: &Model) -> Result<Vec<u8>> {
        let path = self.path(model);
        let bytes = fs::read(&path).map_err(|error| match error.kind() {
            ErrorKind::NotFound => Error::ModelMissing(model.name.clone()),
            _ => io_error(&path)(error),
        })?;
        let actual = to_hex(&Sha256::digest(&bytes));
        if actual != model.sha256 {
            return Err(mismatch(&path, model, actual));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::manifest::{Network, OutputScale, SegmentationSpec, Task};

    pub fn model_for(bytes: &[u8], url: &str) -> Model {
        Model {
            name: "test-model".into(),
            title: "Test".into(),
            task: Task::Subject,
            url: url.into(),
            size: bytes.len() as u64,
            sha256: to_hex(&Sha256::digest(bytes)),
            licence: "MIT".into(),
            licence_url: "https://example.com/LICENSE".into(),
            network: Network::Segmentation(SegmentationSpec {
                input_size: 8,
                mean: [0.0; 3],
                std: [1.0; 3],
                output: OutputScale::Logits,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::model_for, *};
    use std::{
        io::{BufRead, BufReader, Write},
        net::{SocketAddr, TcpListener, TcpStream},
        sync::{Arc, Mutex},
        thread,
    };
    use tempfile::TempDir;

    /// What the test server does with each successive request.
    #[derive(Clone, Copy)]
    enum Behaviour {
        Serve,
        /// Sends this many body bytes of a longer response, then hangs up.
        DropAfter(usize),
        IgnoreRange,
        Status(u16),
    }

    #[derive(Default)]
    struct Log {
        ranges: Vec<Option<u64>>,
    }

    struct TestServer {
        address: SocketAddr,
        log: Arc<Mutex<Log>>,
    }

    impl TestServer {
        fn start(content: Vec<u8>, plan: Vec<Behaviour>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let log = Arc::new(Mutex::new(Log::default()));
            let shared = Arc::clone(&log);
            thread::spawn(move || {
                let mut steps = plan.into_iter();
                for stream in listener.incoming() {
                    let behaviour = steps.next().unwrap_or(Behaviour::Serve);
                    respond(stream.unwrap(), &content, behaviour, &shared);
                }
            });
            Self { address, log }
        }

        fn url(&self) -> String {
            format!("http://{}/model.onnx", self.address)
        }

        fn ranges(&self) -> Vec<Option<u64>> {
            self.log.lock().unwrap().ranges.clone()
        }
    }

    fn requested_range(stream: &TcpStream) -> Option<u64> {
        let mut range = None;
        for line in BufReader::new(stream).lines() {
            let line = line.unwrap();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                range = value.trim_end_matches('-').parse().ok();
            }
        }
        range
    }

    fn respond(mut stream: TcpStream, content: &[u8], behaviour: Behaviour, log: &Mutex<Log>) {
        let range = requested_range(&stream);
        log.lock().unwrap().ranges.push(range);
        let total = content.len();
        let head = |status: &str, length: usize, extra: String| {
            format!("HTTP/1.1 {status}\r\nContent-Length: {length}\r\n{extra}Connection: close\r\n\r\n")
        };
        let (header, body): (String, &[u8]) = match (behaviour, range) {
            (Behaviour::Status(code), _) => (head(&format!("{code} Nope"), 0, String::new()), &[]),
            (Behaviour::DropAfter(bytes), _) => {
                (head("200 OK", total, String::new()), &content[..bytes])
            }
            (Behaviour::Serve, Some(start)) if start as usize >= total => {
                (head("416 Range Not Satisfiable", 0, String::new()), &[])
            }
            (Behaviour::Serve, Some(start)) => {
                let extra = format!("Content-Range: bytes {start}-{}/{total}\r\n", total - 1);
                (head("206 Partial Content", total - start as usize, extra), &content[start as usize..])
            }
            (Behaviour::Serve | Behaviour::IgnoreRange, _) => {
                (head("200 OK", total, String::new()), content)
            }
        };
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(body);
    }

    fn content() -> Vec<u8> {
        (0..300_000u32).map(|index| (index % 251) as u8).collect()
    }

    fn download(store: &ModelStore, model: &Model) -> Result<PathBuf> {
        store.download(model, &mut |_| {}, &CancelToken::new())
    }

    fn store_in(directory: &TempDir) -> ModelStore {
        ModelStore::new(directory.path())
    }

    #[test]
    fn a_download_lands_under_its_final_name_only_once_complete() {
        let (directory, bytes) = (TempDir::new().unwrap(), content());
        let server = TestServer::start(bytes.clone(), vec![Behaviour::Serve]);
        let (store, model) = (store_in(&directory), model_for(&bytes, &server.url()));

        assert_eq!(store.status(&model), ModelStatus::Missing);
        let mut seen = Vec::new();
        let path = store.download(&model, &mut |step| seen.push(step), &CancelToken::new()).unwrap();

        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(store.status(&model), ModelStatus::Ready);
        assert!(!store.partial_path(&model).exists());
        assert_eq!(seen.last(), Some(&Progress { done: bytes.len() as u64, total: bytes.len() as u64 }));
    }

    #[test]
    fn an_interrupted_download_keeps_its_bytes_and_resumes_from_them() {
        let (directory, bytes) = (TempDir::new().unwrap(), content());
        let server = TestServer::start(bytes.clone(), vec![Behaviour::DropAfter(100_000), Behaviour::Serve]);
        let (store, model) = (store_in(&directory), model_for(&bytes, &server.url()));

        assert!(download(&store, &model).is_err());
        assert!(!store.path(&model).exists(), "a partial file must never look finished");
        assert!(matches!(store.status(&model), ModelStatus::Partial { received } if received > 0));

        let path = download(&store, &model).unwrap();

        assert_eq!(fs::read(path).unwrap(), bytes);
        let resumed_from = server.ranges()[1].expect("the second attempt should ask for a range");
        assert!(resumed_from >= 65_536, "resumed from {resumed_from}");
    }

    #[test]
    fn a_server_that_ignores_the_range_restarts_cleanly() {
        let (directory, bytes) = (TempDir::new().unwrap(), content());
        let server =
            TestServer::start(bytes.clone(), vec![Behaviour::DropAfter(100_000), Behaviour::IgnoreRange]);
        let (store, model) = (store_in(&directory), model_for(&bytes, &server.url()));

        assert!(download(&store, &model).is_err());
        let path = download(&store, &model).unwrap();

        assert_eq!(fs::read(path).unwrap(), bytes, "appending to a restarted body would corrupt it");
    }

    #[test]
    fn a_tampered_download_is_rejected_and_never_published() {
        let (directory, bytes) = (TempDir::new().unwrap(), content());
        let mut tampered = bytes.clone();
        tampered[1234] ^= 0xFF;
        let server = TestServer::start(tampered, vec![Behaviour::Serve]);
        let (store, model) = (store_in(&directory), model_for(&bytes, &server.url()));

        assert!(matches!(download(&store, &model), Err(Error::HashMismatch { .. })));

        assert!(!store.path(&model).exists());
        assert!(!store.partial_path(&model).exists(), "bad bytes must not be resumed from");
    }

    #[test]
    fn a_response_longer_than_the_manifest_says_is_rejected() {
        let (directory, bytes) = (TempDir::new().unwrap(), content());
        let longer = [bytes.as_slice(), &[0; 100_000]].concat();
        let server = TestServer::start(longer, vec![Behaviour::Serve]);
        let (store, model) = (store_in(&directory), model_for(&bytes, &server.url()));

        assert!(matches!(download(&store, &model), Err(Error::TooLarge { .. })));

        assert!(!store.path(&model).exists());
        assert!(!store.partial_path(&model).exists());
    }

    #[test]
    fn an_error_status_keeps_any_partial_data_and_publishes_nothing() {
        let (directory, bytes) = (TempDir::new().unwrap(), content());
        let server = TestServer::start(bytes.clone(), vec![Behaviour::Status(503)]);
        let (store, model) = (store_in(&directory), model_for(&bytes, &server.url()));

        assert!(matches!(download(&store, &model), Err(Error::Download { .. })));

        assert!(!store.path(&model).exists());
    }

    #[test]
    fn cancelling_stops_the_download_and_keeps_it_resumable() {
        let (directory, bytes) = (TempDir::new().unwrap(), content());
        let server = TestServer::start(bytes.clone(), vec![Behaviour::Serve, Behaviour::Serve]);
        let (store, model) = (store_in(&directory), model_for(&bytes, &server.url()));
        let cancel = CancelToken::new();

        let stopper = cancel.clone();
        let result = store.download(
            &model,
            &mut |step| {
                if step.done > 0 {
                    stopper.cancel();
                }
            },
            &cancel,
        );

        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(!store.path(&model).exists());
        assert!(matches!(store.status(&model), ModelStatus::Partial { .. }));
        assert_eq!(fs::read(download(&store, &model).unwrap()).unwrap(), bytes);
    }

    #[test]
    fn a_file_altered_after_download_is_refused_at_load_time() {
        let (directory, bytes) = (TempDir::new().unwrap(), content());
        let server = TestServer::start(bytes.clone(), vec![Behaviour::Serve]);
        let (store, model) = (store_in(&directory), model_for(&bytes, &server.url()));
        let path = download(&store, &model).unwrap();
        assert_eq!(store.load(&model).unwrap(), bytes);

        let mut altered = bytes;
        altered[0] ^= 1;
        fs::write(&path, altered).unwrap();

        assert_eq!(store.status(&model), ModelStatus::Ready, "status only looks at the size");
        assert!(matches!(store.load(&model), Err(Error::HashMismatch { .. })));
    }

    #[test]
    fn a_damaged_copy_is_replaced_by_the_next_download() {
        let (directory, bytes) = (TempDir::new().unwrap(), content());
        let server = TestServer::start(bytes.clone(), vec![Behaviour::Serve, Behaviour::Serve]);
        let (store, model) = (store_in(&directory), model_for(&bytes, &server.url()));
        let path = download(&store, &model).unwrap();
        fs::write(&path, b"not the model").unwrap();

        download(&store, &model).unwrap();

        assert_eq!(store.load(&model).unwrap(), bytes);
    }

    #[test]
    fn a_missing_model_is_a_clear_error_rather_than_a_crash() {
        let directory = TempDir::new().unwrap();
        let model = model_for(b"anything", "http://127.0.0.1:9/never");
        let error = store_in(&directory).load(&model).unwrap_err();
        assert!(matches!(error, Error::ModelMissing(_)));
        assert_eq!(error.to_string(), "test-model isn't downloaded yet");
    }

    #[test]
    fn two_downloads_of_one_model_cannot_interleave() {
        let (directory, bytes) = (TempDir::new().unwrap(), content());
        let server = TestServer::start(bytes.clone(), vec![Behaviour::Serve]);
        let (store, model) = (store_in(&directory), model_for(&bytes, &server.url()));
        fs::create_dir_all(store.directory(&model)).unwrap();
        let holder = File::create(store.partial_path(&model)).unwrap();
        holder.lock().unwrap();

        assert!(matches!(download(&store, &model), Err(Error::DownloadInProgress(_))));
    }
}
