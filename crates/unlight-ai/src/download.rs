//! The only code in Unlight that talks to the network, and it only runs when the user asks.

use crate::{
    cancel::CancelToken,
    error::{Error, Result, io_error},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::Duration,
};
use ureq::{Agent, Body, http::Response};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK_BYTES: usize = 1 << 16;

const OK: u16 = 200;
const PARTIAL_CONTENT: u16 = 206;
const RANGE_NOT_SATISFIABLE: u16 = 416;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

pub(crate) struct Source<'a> {
    pub url: &'a str,
    pub total: u64,
}

fn agent() -> Agent {
    Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_per_read(Some(READ_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into()
}

fn failure(url: &str, error: impl ToString) -> Error {
    Error::Download { url: url.to_owned(), message: error.to_string() }
}

fn request(agent: &Agent, url: &str, offset: u64) -> Result<Response<Body>> {
    let mut request = agent.get(url);
    if offset > 0 {
        request = request.header("Range", format!("bytes={offset}-"));
    }
    request.call().map_err(|error| failure(url, error))
}

/// The first byte a 206 response covers, from `Content-Range: bytes 100-199/200`.
fn range_start(response: &Response<Body>) -> Option<u64> {
    let header = response.headers().get("content-range")?.to_str().ok()?;
    header.strip_prefix("bytes ")?.split('-').next()?.parse().ok()
}

/// Makes `file` hold the first `total` bytes of `source`, keeping whatever an earlier attempt
/// already wrote if the server can continue from there. Partial data survives failure and
/// cancellation, so the next attempt resumes.
pub(crate) fn fetch(
    source: &Source<'_>,
    file: &mut File,
    path: &Path,
    on_progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<()> {
    let url = source.url;
    let agent = agent();
    let mut offset = file.metadata().map_err(io_error(path))?.len();
    let mut response = request(&agent, url, offset)?;
    if response.status().as_u16() == RANGE_NOT_SATISFIABLE {
        offset = 0;
        response = request(&agent, url, offset)?;
    }
    match response.status().as_u16() {
        PARTIAL_CONTENT if range_start(&response) == Some(offset) => {}
        OK => offset = 0,
        status => return Err(failure(url, format!("the server answered {status}"))),
    }
    file.set_len(offset).map_err(io_error(path))?;
    file.seek(SeekFrom::Start(offset)).map_err(io_error(path))?;
    let mut body = response.into_body();
    copy_body(&mut body.as_reader(), source, file, path, offset, on_progress, cancel)
}

fn copy_body(
    body: &mut impl Read,
    source: &Source<'_>,
    file: &mut File,
    path: &Path,
    mut received: u64,
    on_progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<()> {
    let Source { url, total } = *source;
    let mut chunk = vec![0; CHUNK_BYTES];
    on_progress(Progress { done: received, total });
    loop {
        cancel.check()?;
        let read = body.read(&mut chunk).map_err(|error| failure(url, error))?;
        if read == 0 {
            break;
        }
        received += read as u64;
        if received > total {
            return Err(Error::TooLarge { url: url.to_owned(), expected: total });
        }
        file.write_all(&chunk[..read]).map_err(io_error(path))?;
        on_progress(Progress { done: received, total });
    }
    if received < total {
        return Err(Error::Truncated { url: url.to_owned(), received, expected: total });
    }
    Ok(())
}
