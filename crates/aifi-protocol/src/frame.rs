//! JSON Lines 帧编解码（与传输无关，便于在 Linux 上测试）。

use serde::{Serialize, de::DeserializeOwned};

use crate::MAX_FRAME_BYTES;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame exceeds {MAX_FRAME_BYTES} bytes")]
    TooLarge,
    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),
}

/// 编码为一行（以 `\n` 结尾）。
pub fn encode_line<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let mut v = serde_json::to_vec(msg)?;
    if v.len() + 1 > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    v.push(b'\n');
    Ok(v)
}

/// 解码一行（允许带或不带结尾换行 / `\r\n`）。
pub fn decode_line<T: DeserializeOwned>(line: &[u8]) -> Result<T, FrameError> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
    Ok(serde_json::from_slice(trimmed)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[test]
    fn request_roundtrip_and_wire_shape() {
        let req = RequestEnvelope {
            id: 7,
            request: Request::FindFiles(FindFilesParams {
                query: "jdbg".into(),
                cwd: Some(r"C:\src\demo".into()),
                ..Default::default()
            }),
        };
        let bytes = encode_line(&req).unwrap();
        assert_eq!(*bytes.last().unwrap(), b'\n');
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["method"], "find_files");
        assert_eq!(v["params"]["scope"], "project");
        let back: RequestEnvelope = decode_line(&bytes).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn missing_optional_fields_default() {
        let line = br#"{"id":1,"method":"find_files","params":{"query":"main.rs"}}"#;
        let r: RequestEnvelope = decode_line(line).unwrap();
        match r.request {
            Request::FindFiles(p) => {
                assert_eq!(p.scope, Scope::Project);
                assert_eq!(p.mode, MatchMode::Auto);
                assert_eq!(clamp_limit(p.limit), DEFAULT_LIMIT);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_capability_is_tolerated() {
        let line = br#"{"id":0,"type":"hello","data":{"protocol_version":1,"capabilities":["names","pinyin","semantic_v9"],"service_version":"0.1.0"}}"#;
        let r: ResponseEnvelope = decode_line(line).unwrap();
        match r.response {
            Response::Hello { capabilities, .. } => {
                assert_eq!(capabilities, vec![Capability::Names, Capability::Pinyin, Capability::Unknown]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn limit_clamped() {
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(1_000_000)), MAX_LIMIT);
    }

    #[test]
    fn oversized_frame_rejected() {
        let big = vec![b' '; MAX_FRAME_BYTES + 1];
        assert!(matches!(decode_line::<serde_json::Value>(&big), Err(FrameError::TooLarge)));
    }
}
