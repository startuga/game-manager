//! A minimal HTTPS `GET` over WinHTTP, Windows' own HTTP client: it uses the system's proxy
//! settings and certificate store, with no extra crates. Blocking: call it off the UI thread.

use crate::{Error, Result};
use windows::Win32::{
    HINTERNET, INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle,
    WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Name resolution, connect, send and receive timeouts.
const TIMEOUT_MS: i32 = 10_000;
/// Larger answers are refused; a wiki page's text is far smaller.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Who is asking: PCGamingWiki asks API users for a descriptive agent with contact details.
pub(crate) fn user_agent() -> String {
    format!(
        "GameManager/{} (+https://github.com/startuga/game-manager)",
        env!("CARGO_PKG_VERSION")
    )
}

/// `GET https://<host><path>`: the body of a `200 OK` answer.
pub(crate) fn get(host: &str, path: &str) -> Result<Vec<u8>> {
    let url = format!("https://{host}{path}");
    let failed = |source| Error::Windows {
        context: url.clone(),
        source,
    };
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
    .map_err(failed)?;
    // SAFETY: `session` is a live session handle.
    unsafe { WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS) }
        .ok()
        .map_err(failed)?;
    let host_name = HSTRING::from(host);
    // SAFETY: a live session handle and a valid wide string.
    let connection = Handle::new(unsafe {
        WinHttpConnect(session.0, &host_name, INTERNET_DEFAULT_HTTPS_PORT as u16, 0)
    })
    .map_err(failed)?;
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
            None,
            WINHTTP_FLAG_SECURE as u32,
        )
    })
    .map_err(failed)?;
    // SAFETY: a live request handle; no extra headers and no body.
    unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }
        .ok()
        .map_err(failed)?;
    // SAFETY: the request was sent; the reserved argument must be null.
    unsafe { WinHttpReceiveResponse(request.0, std::ptr::null()) }
        .ok()
        .map_err(failed)?;

    let mut status = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: with WINHTTP_QUERY_FLAG_NUMBER the status is written as a u32 into `status`, whose
    // size `size` gives.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            (WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER) as u32,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut size,
            None,
        )
    }
    .ok()
    .map_err(failed)?;
    if status != 200 {
        return Err(Error::Http { url, status });
    }

    let mut body = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let mut read = 0u32;
        // SAFETY: `chunk` is writable for the length passed, and `read` is a valid out-pointer.
        unsafe {
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )
        }
        .ok()
        .map_err(failed)?;
        let Some(data) = chunk.get(..read as usize).filter(|data| !data.is_empty()) else {
            return Ok(body);
        };
        body.extend_from_slice(data);
        if body.len() > MAX_BODY {
            return Err(Error::ResponseTooLarge { url });
        }
    }
}

/// A WinHTTP handle, closed when dropped. Locals drop in reverse order, so a request closes
/// before its connection, and the connection before the session.
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
