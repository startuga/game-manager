//! Minimal HTTPS `GET`s over WinHTTP, Windows' own HTTP client: it uses the system's proxy
//! settings and certificate store, and follows redirects, with no extra crates. `get` reads small
//! answers into memory, `download` streams a file to disk. Blocking: call them off the UI thread.

use crate::{Error, Result};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use windows::Win32::{
    HINTERNET, INTERNET_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

/// `INTERNET_DEFAULT_HTTPS_PORT`, which windows-rs generates only with WinINet's header.
const HTTPS_PORT: INTERNET_PORT = 443;
/// Name resolution, connect, send and receive timeouts.
const TIMEOUT_MS: i32 = 10_000;
/// Larger answers are refused; a wiki page's text or a release's JSON is far smaller.
const MAX_BODY: usize = 8 * 1024 * 1024;
/// Larger downloads are refused; a DXVK build is under 50 MB.
const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;

/// Who is asking: PCGamingWiki and GitHub ask API users for a descriptive agent with contact
/// details.
pub(crate) fn user_agent() -> String {
    format!(
        "GameManager/{} (+https://github.com/startuga/game-manager)",
        env!("CARGO_PKG_VERSION")
    )
}

/// `GET <url>`: the body of a `200 OK` answer.
pub(crate) fn get(url: &str) -> Result<Vec<u8>> {
    let response = Response::open(url)?;
    let mut body = Vec::new();
    response.read_each(|data| {
        body.extend_from_slice(data);
        if body.len() > MAX_BODY {
            return Err(Error::ResponseTooLarge {
                url: url.to_owned(),
            });
        }
        Ok(())
    })?;
    Ok(body)
}

/// `GET <url>` into `file`. A failed download leaves no file behind.
pub(crate) fn download(url: &str, file: &Path) -> Result<()> {
    let result = download_to(url, file);
    if result.is_err() {
        let _ = fs::remove_file(file);
    }
    result
}

fn download_to(url: &str, file: &Path) -> Result<()> {
    let response = Response::open(url)?;
    let mut out = BufWriter::new(File::create(file).map_err(Error::io(file))?);
    let mut written = 0u64;
    response.read_each(|data| {
        written += data.len() as u64;
        if written > MAX_DOWNLOAD {
            return Err(Error::ResponseTooLarge {
                url: url.to_owned(),
            });
        }
        out.write_all(data).map_err(Error::io(file))
    })?;
    out.flush().map_err(Error::io(file))
}

/// `https://host/path?query` as `("host", "/path?query")`.
fn split_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        None => (rest, "/"),
    };
    (!host.is_empty()).then_some((host, path))
}

/// A `200 OK` answer being read. Fields drop in order, so the request closes before its
/// connection, and the connection before the session.
struct Response {
    url: String,
    request: Handle,
    _connection: Handle,
    _session: Handle,
}

impl Response {
    fn open(url: &str) -> Result<Self> {
        let (host, path) = split_url(url).ok_or_else(|| Error::InvalidUrl(url.to_owned()))?;
        let agent = HSTRING::from(user_agent());
        // SAFETY: `agent` is a valid wide string for the call; null proxy names mean the system's.
        let session = Handle::new(unsafe {
            WinHttpOpen(
                &agent,
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY as u32,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        })
        .map_err(Error::windows(url))?;
        // SAFETY: `session` is a live session handle.
        unsafe { WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS) }
            .ok()
            .map_err(Error::windows(url))?;
        let host_name = HSTRING::from(host);
        // SAFETY: a live session handle and a valid wide string.
        let connection =
            Handle::new(unsafe { WinHttpConnect(session.0, &host_name, HTTPS_PORT, 0) })
                .map_err(Error::windows(url))?;
        let object = HSTRING::from(path);
        // SAFETY: a live connection handle and valid wide strings; a null version, referrer and
        // accept-type list mean HTTP/1.1, none and any.
        let request = Handle::new(unsafe {
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &object,
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null_mut(),
                WINHTTP_FLAG_SECURE as u32,
            )
        })
        .map_err(Error::windows(url))?;
        // SAFETY: a live request handle; no extra headers and no body.
        unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }
            .ok()
            .map_err(Error::windows(url))?;
        // SAFETY: the request was sent; the reserved argument must be null. Redirects have been
        // followed by now.
        unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }
            .ok()
            .map_err(Error::windows(url))?;

        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: with WINHTTP_QUERY_FLAG_NUMBER the status is written as a u32 into `status`,
        // whose size `size` gives; a null index means the first header.
        unsafe {
            WinHttpQueryHeaders(
                request.0,
                (WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER) as u32,
                PCWSTR::null(),
                Some((&raw mut status).cast()),
                &mut size,
                std::ptr::null_mut(),
            )
        }
        .ok()
        .map_err(Error::windows(url))?;
        if status != 200 {
            return Err(Error::Http {
                url: url.to_owned(),
                status,
            });
        }
        Ok(Self {
            url: url.to_owned(),
            request,
            _connection: connection,
            _session: session,
        })
    }

    /// Hands the body to `each`, a chunk at a time, until it ends or `each` fails.
    fn read_each(&self, mut each: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            // SAFETY: `chunk` is writable for the length passed, and `read` is a valid
            // out-pointer.
            unsafe {
                WinHttpReadData(
                    self.request.0,
                    chunk.as_mut_ptr().cast(),
                    chunk.len() as u32,
                    &mut read,
                )
            }
            .ok()
            .map_err(Error::windows(&self.url))?;
            match chunk.get(..read as usize).filter(|data| !data.is_empty()) {
                Some(data) => each(data)?,
                None => return Ok(()),
            }
        }
    }
}

/// A WinHTTP handle, closed when dropped.
struct Handle(HINTERNET);

impl Handle {
    fn new(handle: HINTERNET) -> windows::core::Result<Self> {
        if handle.is_null() {
            Err(windows::core::Error::from_thread())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from WinHTTP, only this value owns it, and it is closed once.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_https_addresses() {
        assert_eq!(
            split_url("https://api.github.com/repos/doitsujin/dxvk/releases/latest"),
            Some(("api.github.com", "/repos/doitsujin/dxvk/releases/latest"))
        );
        assert_eq!(
            split_url("https://nightly.link/a/b.zip?x=1"),
            Some(("nightly.link", "/a/b.zip?x=1"))
        );
        assert_eq!(split_url("https://example.com"), Some(("example.com", "/")));
        assert_eq!(split_url("http://example.com/"), None);
        assert_eq!(split_url("https:///path"), None);
    }
}
