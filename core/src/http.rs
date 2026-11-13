//! Minimal HTTPS `GET`s over WinHTTP, Windows' own HTTP client: it uses the system's proxy
//! settings and certificate store, and follows redirects, with no extra crates. `get` reads small
//! answers into memory, `download` streams a file to disk, `etag` asks only for the headers.
//! Blocking: call them off the UI thread.
//!
//! The process has one WinHTTP session (`session`), opened by the first request and kept, with
//! the proxy settings and timeouts on it. Each request opens its own connection and request
//! handles on it, while WinHTTP keeps the session's connections to a host open and reuses them,
//! so the covers and version lists asked of one server at start share their TLS handshakes.

use crate::{Error, Result};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::OnceLock;
use windows::Win32::{
    HINTERNET, INTERNET_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_ETAG, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle,
    WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

/// `INTERNET_DEFAULT_HTTPS_PORT`, which windows-rs generates only with WinINet's header.
const HTTPS_PORT: INTERNET_PORT = 443;
/// Name resolution, connect, send and receive timeouts.
const TIMEOUT_MS: i32 = 10_000;
/// Larger answers are refused; a wiki page's text or a release's JSON is far smaller.
const MAX_BODY: usize = 8 * 1024 * 1024;
/// Larger downloads are refused; a DXVK build is under 50 MB, Streamline's release archive about
/// 280 MB.
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

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

/// `HEAD <url>`: the answer's `ETag`, which changes when the file does (GitHub's and GitLab's
/// archives have one); `None` when it has none.
pub(crate) fn etag(url: &str) -> Result<Option<String>> {
    Ok(Response::request(url, w!("HEAD"))?.header(WINHTTP_QUERY_ETAG as u32))
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

/// The process's WinHTTP session (see the module's doc). When two threads open one at once, the
/// first kept is used and the other closes.
fn session() -> windows::core::Result<&'static Session> {
    static SESSION: OnceLock<Session> = OnceLock::new();
    if let Some(session) = SESSION.get() {
        return Ok(session);
    }
    let opened = Session::open()?;
    Ok(SESSION.get_or_init(|| opened))
}

/// A WinHTTP session, with the system's proxy and the timeouts.
struct Session(Handle);

// SAFETY: WinHTTP session handles may be used from any thread and by several at once; the handle
// is only closed when the value drops, after its last use.
unsafe impl Send for Session {}
// SAFETY: as above; nothing here changes the session after it is opened.
unsafe impl Sync for Session {}

impl Session {
    fn open() -> windows::core::Result<Self> {
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
        })?;
        // SAFETY: `session` is a live session handle.
        unsafe { WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS) }
            .ok()?;
        Ok(Self(session))
    }
}

/// A `200 OK` answer being read. Fields drop in order, so the request closes before its
/// connection.
struct Response {
    url: String,
    request: Handle,
    _connection: Handle,
}

impl Response {
    fn open(url: &str) -> Result<Self> {
        Self::request(url, w!("GET"))
    }

    fn request(url: &str, method: PCWSTR) -> Result<Self> {
        let (host, path) = split_url(url).ok_or_else(|| Error::InvalidUrl(url.to_owned()))?;
        let session = session().map_err(Error::windows(url))?;
        let host_name = HSTRING::from(host);
        // SAFETY: the process's session handle, never closed, and a valid wide string.
        let connection =
            Handle::new(unsafe { WinHttpConnect(session.0.0, &host_name, HTTPS_PORT, 0) })
                .map_err(Error::windows(url))?;
        let object = HSTRING::from(path);
        // SAFETY: a live connection handle and valid wide strings; a null version, referrer and
        // accept-type list mean HTTP/1.1, none and any.
        let request = Handle::new(unsafe {
            WinHttpOpenRequest(
                connection.0,
                method,
                &object,
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null_mut(),
                WINHTTP_FLAG_SECURE as u32,
            )
        })
        .map_err(Error::windows(url))?;
        // SAFETY: a live request handle; no extra headers and no body.
        unsafe { WinHttpSendRequest(request.0, None, 0, None, 0, 0, 0) }
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
        })
    }

    /// A response header as text (`WINHTTP_QUERY_ETAG`), if the answer has it.
    fn header(&self, query: u32) -> Option<String> {
        let mut size = 0u32;
        // SAFETY: asks only for the size, in bytes; it fails by design, or when there is no such
        // header. A null name and index mean the header by its query and its first value.
        let _ = unsafe {
            WinHttpQueryHeaders(
                self.request.0,
                query,
                PCWSTR::null(),
                None,
                &mut size,
                std::ptr::null_mut(),
            )
        };
        if size == 0 {
            return None;
        }
        let mut text = vec![0u16; (size as usize).div_ceil(2)];
        // SAFETY: `text` holds `size` bytes, the size WinHTTP asked for.
        unsafe {
            WinHttpQueryHeaders(
                self.request.0,
                query,
                PCWSTR::null(),
                Some(text.as_mut_ptr().cast()),
                &mut size,
                std::ptr::null_mut(),
            )
        }
        .ok()
        .ok()?;
        let value = String::from_utf16_lossy(&text[..(size as usize / 2).min(text.len())]);
        Some(value.trim_end_matches('\0').to_owned()).filter(|value| !value.is_empty())
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

    #[test]
    fn the_process_has_one_session() {
        let first = session().unwrap();
        let from_another_thread = std::thread::spawn(|| session().unwrap().0.0 as usize)
            .join()
            .unwrap();
        assert!(std::ptr::eq(first, session().unwrap()));
        assert_eq!(first.0.0 as usize, from_another_thread);
    }
}
