//! 请求分发（与传输无关）。M6 实现；保持纯逻辑以便在 Linux 上做单元测试。

use aifi_protocol::{Capability, ErrorCode, PROTOCOL_VERSION, Request, Response};

/// 服务端请求处理器，持有全部卷索引与项目信息（M6 填充字段）。
pub struct Handler {}

impl Handler {
    /// 当前实现的能力。内容搜索（`Content`）在 v2 之前不得出现在这里。
    pub fn capabilities() -> Vec<Capability> {
        vec![Capability::Names, Capability::Pinyin]
    }

    /// 处理一条请求。版本不匹配时返回 `VersionMismatch`。
    #[allow(dead_code)]
    pub fn handle(&self, req: Request) -> Response {
        match req {
            Request::Hello { protocol_version, .. } if protocol_version != PROTOCOL_VERSION => Response::Error {
                code: ErrorCode::VersionMismatch,
                message: format!("server speaks protocol {PROTOCOL_VERSION}, client sent {protocol_version}"),
            },
            Request::Hello { .. } => Response::Hello {
                protocol_version: PROTOCOL_VERSION,
                capabilities: Self::capabilities(),
                service_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Request::FindFiles(_) => todo!("M6: scope resolution + aifi_query::search_volume over all volumes"),
            Request::ListProjectTree(_) => todo!("M6: tree from index, gitignore + denylist applied"),
            Request::ListProjects(_) => todo!("M6: project registry"),
            Request::RecentChanges(_) => todo!("M6: ring buffer of recent USN events"),
            Request::IndexStatus(_) => todo!("M6: per-volume status"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_negotiates_version() {
        let h = Handler {};
        match h.handle(Request::Hello { protocol_version: PROTOCOL_VERSION, client: "test".into() }) {
            Response::Hello { capabilities, .. } => assert!(!capabilities.contains(&Capability::Content)),
            r => panic!("{r:?}"),
        }
        match h.handle(Request::Hello { protocol_version: 99, client: "test".into() }) {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::VersionMismatch),
            r => panic!("{r:?}"),
        }
    }
}
