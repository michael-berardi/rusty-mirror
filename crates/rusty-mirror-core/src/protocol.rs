//! Newline-delimited JSON over a same-user unix socket. One request, one
//! response, then the connection closes. Responses always carry `ok`.
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

pub const VERSION: u32 = 1;
pub const MAX_REQUEST: usize = 64 * 1024;
/// Captures travel as base64 PNG, so responses may be large.
pub const MAX_RESPONSE: usize = 64 * 1024 * 1024;
pub const MAX_SCRIPT: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    /// Report whether a mirror is open and what it looks like.
    State,
    /// Snapshot the primary window and open a hidden mirror for `seconds`.
    Open {
        seconds: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    /// Run an async JavaScript function body in the mirror and return its value.
    Eval {
        script: String,
    },
    /// Native renderer snapshot of the hidden mirror, returned as base64 PNG.
    Capture,
    Resize {
        width: u32,
        height: u32,
    },
    /// Move the (still invisible) mirror onto another monitor to test its scale factor.
    Screen {
        index: u32,
    },
    /// Rebuild the mirror on a staged revision, carrying its local state across.
    Refresh {
        revision: String,
    },
    /// Refresh the mirror back to the revision before the current one.
    Rollback,
    /// Navigate the primary window to the mirror's verified revision.
    Apply,
    Close,
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Request::Open {
                seconds,
                width,
                height,
            } => {
                if !(1..=crate::MAX_SECONDS).contains(seconds) {
                    return Err(format!("seconds must be 1..={}", crate::MAX_SECONDS));
                }
                if let (Some(w), Some(h)) = (width, height) {
                    check_size(*w, *h)?;
                } else if width.is_some() != height.is_some() {
                    return Err("give both width and height, or neither".into());
                }
                Ok(())
            }
            Request::Eval { script } if script.is_empty() || script.len() > MAX_SCRIPT => {
                Err(format!("script must be 1..={MAX_SCRIPT} bytes"))
            }
            Request::Resize { width, height } => check_size(*width, *height),
            Request::Screen { index } if *index > 15 => Err("monitor index must be 0..=15".into()),
            Request::Refresh { revision } if !is_revision(revision) => {
                Err("revision must be a 64-character lowercase sha256 digest".into())
            }
            _ => Ok(()),
        }
    }
}

pub fn check_size(width: u32, height: u32) -> Result<(), String> {
    if (200..=7680).contains(&width) && (200..=4320).contains(&height) {
        Ok(())
    } else {
        Err("size must be within 200x200..7680x4320".into())
    }
}

pub fn is_revision(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Wire envelope; `v` lets a future CLI refuse an older app politely.
#[derive(Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    #[serde(flatten)]
    pub request: Request,
}

/// Parse one request line received by the app.
pub fn decode_request(line: &[u8]) -> Result<Request, String> {
    if line.len() > MAX_REQUEST {
        return Err("request exceeds 64 KiB".into());
    }
    let mut value: serde_json::Value =
        serde_json::from_slice(line).map_err(|e| format!("bad request: {e}"))?;
    let v = value
        .as_object_mut()
        .and_then(|o| o.remove("v"))
        .and_then(|v| v.as_u64());
    if v != Some(VERSION as u64) {
        return Err(format!(
            "protocol version {v:?} requested; this app speaks v{VERSION}"
        ));
    }
    // Decoded without the envelope so unknown fields are refused, not ignored.
    let request: Request =
        serde_json::from_value(value.clone()).map_err(|e| format!("bad request: {e}"))?;
    // serde ignores extra keys on unit variants; compare against the canonical form.
    let canonical = serde_json::to_value(&request).map_err(|e| e.to_string())?;
    if let (Some(given), Some(known)) = (value.as_object(), canonical.as_object()) {
        if let Some(extra) = given.keys().find(|k| !known.contains_key(*k)) {
            return Err(format!("bad request: unknown field `{extra}`"));
        }
    }
    request.validate()?;
    Ok(request)
}

/// Client side: send one request and read one response.
pub fn call(
    socket: &Path,
    request: &Request,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    request.validate()?;
    crate::paths::check_socket(socket)?;
    let mut wire = serde_json::to_vec(&Envelope {
        v: VERSION,
        request: request.clone(),
    })
    .map_err(|e| e.to_string())?;
    wire.push(b'\n');
    if wire.len() > MAX_REQUEST {
        return Err("request exceeds 64 KiB".into());
    }
    let mut stream = UnixStream::connect(socket).map_err(|e| {
        format!(
            "cannot reach {} ({e}); is a mirror-enabled build of the app running?",
            socket.display()
        )
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream.write_all(&wire).map_err(|e| e.to_string())?;
    let mut line = Vec::new();
    BufReader::new(stream.take(MAX_RESPONSE as u64 + 1))
        .read_until(b'\n', &mut line)
        .map_err(|e| format!("no response: {e}"))?;
    if line.len() > MAX_RESPONSE {
        return Err("response exceeds limit".into());
    }
    if line.last() != Some(&b'\n') {
        return Err("unterminated response (did the app exit?)".into());
    }
    serde_json::from_slice(&line).map_err(|e| format!("bad response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_validation() {
        let line = br#"{"v":1,"action":"open","seconds":60}"#;
        assert_eq!(
            decode_request(line).unwrap(),
            Request::Open {
                seconds: 60,
                width: None,
                height: None
            }
        );
        assert!(decode_request(br#"{"v":1,"action":"open","seconds":0}"#).is_err());
        assert!(decode_request(br#"{"v":2,"action":"state"}"#).is_err());
        assert!(decode_request(br#"{"v":1,"action":"refresh","revision":"../../etc"}"#).is_err());
        assert!(decode_request(br#"{"v":1,"action":"launch-missiles"}"#).is_err());
        assert!(decode_request(br#"{"v":1,"action":"state","extra":1}"#).is_err());
        let wire = serde_json::to_string(&Envelope {
            v: 1,
            request: Request::Eval {
                script: "return 1".into(),
            },
        })
        .unwrap();
        assert_eq!(wire, r#"{"v":1,"action":"eval","script":"return 1"}"#);
    }
}
