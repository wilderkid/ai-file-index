//! # aifi-protocol
//!
//! 服务（管理员权限，持有索引）与客户端（MCP server、CLI）之间的 IPC 协议。
//!
//! - 传输：Windows 命名管道 [`PIPE_NAME`]，ACL 仅允许当前登录用户（见 `aifi-service`）。
//! - 帧格式：**一行一个 JSON**（UTF-8，`\n` 结尾），单帧上限 [`MAX_FRAME_BYTES`]。
//! - 握手：客户端先发 [`Request::Hello`]，服务回 [`Response::Hello`]，
//!   携带 `protocol_version` 与 `capabilities`；客户端据此决定可用功能。
//! - 兼容规则：新增字段必须 `#[serde(default)]`；删除/改义字段必须提升 [`PROTOCOL_VERSION`]。
//!
//! 参数结构体同时派生 `JsonSchema`，`aifi-mcp` 直接用它们生成 MCP 工具 schema，
//! 保证 IPC 与 MCP 两侧字段一致。

pub mod frame;
pub mod types;

pub use frame::{FrameError, decode_line, encode_line};
pub use types::*;

/// 当前协议版本。
pub const PROTOCOL_VERSION: u32 = 1;

/// 命名管道名。
pub const PIPE_NAME: &str = r"\\.\pipe\ai-file-index";

/// 单帧最大字节数（防止恶意/错误客户端撑爆内存）。
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// 单次查询返回条数上限（服务端强制）。
pub const MAX_LIMIT: u32 = 1000;

/// 默认返回条数。
pub const DEFAULT_LIMIT: u32 = 50;
