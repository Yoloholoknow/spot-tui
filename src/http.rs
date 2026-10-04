//! Bounded reads for responses from third-party servers, so a misbehaving or
//! hostile one cannot exhaust memory.

use serde::de::DeserializeOwned;
use std::io::Read;
use std::time::Duration;

/// Timeout for each whole request.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// Cap on a JSON/text body; real lyrics and search responses are far smaller.
pub const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;
/// Cap on a downloaded cover image.
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(TIMEOUT).build()
}

/// Reads at most `max` bytes. A longer body is an error, not a truncation.
pub fn read_limited(response: ureq::Response, max: u64) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    response
        .into_reader()
        .take(max + 1)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() as u64 > max {
        return Err(format!("response larger than {max} bytes"));
    }
    Ok(body)
}

pub fn read_json<T: DeserializeOwned>(response: ureq::Response) -> Result<T, String> {
    serde_json::from_slice(&read_limited(response, MAX_BODY_BYTES)?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(body: &str) -> ureq::Response {
        ureq::Response::new(200, "OK", body).unwrap()
    }

    #[test]
    fn a_body_within_the_limit_is_read_whole() {
        assert_eq!(read_limited(response("hello"), 5).unwrap(), b"hello");
    }

    #[test]
    fn a_body_over_the_limit_is_an_error_not_a_truncation() {
        assert!(read_limited(response("hello!"), 5).is_err());
    }

    #[test]
    fn json_is_parsed_within_the_limit() {
        let v: Vec<u32> = read_json(response("[1,2,3]")).unwrap();
        assert_eq!(v, vec![1, 2, 3]);
    }
}
