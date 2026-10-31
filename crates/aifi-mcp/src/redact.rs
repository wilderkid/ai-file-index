//! 对返回给 AI 的内容片段做脱敏：把像密码、令牌、私钥的值换成 `***`。
//!
//! 黑名单只挡住已知的密钥**文件**；普通文件（配置、脚本、表格）里写着的明文密码会被 `search_content`
//! 原样返回。这里在片段离开本进程前做一遍保守的替换。与黑名单一样**不可关闭**：
//! 被提示注入的 AI 不能通过参数拿回原文。
//!
//! 规则宁可多遮也不漏：`token: 5` 这类无害文本也会被遮住，代价只是少看到一个值。

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::{Captures, Regex};

const MASK: &str = "***";

/// `password = xxx`、`"api_key": "xxx"`、`密码：xxx`、连接串里的 `Pwd=xxx;` 等：保留键名，只遮值。
/// 带引号的值整段遮住（值里可以有空格）。
static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)((?:password|passwd|pwd|passphrase|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret|credentials?|密码|口令|密钥|令牌)["']?\s*[:=：]\s*)("[^"]*"|'[^']*'|[^\s"',;]+)"#,
    )
    .expect("static regex")
});

/// 有固定前缀的令牌格式：AWS 访问密钥、GitHub、OpenAI / Anthropic 风格的 `sk-`、Slack、JWT。
static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,})",
    )
    .expect("static regex")
});

/// `Authorization: Bearer xxx`。
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(bearer\s+)([A-Za-z0-9._~+/=-]{8,})").expect("static regex"));

/// URL 里的口令：`scheme://user:pass@host` 只遮 `pass`。
static URL_PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(://[^/\s:@]+:)([^@\s/]+)(@)").expect("static regex"));

/// PEM 私钥正文这类长 base64 串。只遮同时含大写、小写和至少 3 个数字的：小写十六进制哈希、UUID、
/// 驼峰命名的 import 路径都不受影响（随机 base64 每 40 个字符平均约 6 个数字）。
static LONG_BASE64: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/]{40,}={0,2}").expect("static regex"));

/// 遮住一行里所有像密钥的值。没有可遮的内容时原样借用，不分配。
pub fn redact(line: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(line);
    let mut apply = |re: &Regex, f: &dyn Fn(&Captures<'_>) -> String| {
        // 回调可能原样返回（例如不像密钥的长串）；只有真的改了才换成新字符串。
        if let Cow::Owned(s) = re.replace_all(&out, |c: &Captures<'_>| f(c)) {
            if s != *out {
                out = Cow::Owned(s);
            }
        }
    };
    apply(&KEY_VALUE, &|c| match c[2].chars().next() {
        Some(q @ ('"' | '\'')) => format!("{}{q}{MASK}{q}", &c[1]),
        _ => format!("{}{MASK}", &c[1]),
    });
    apply(&URL_PASSWORD, &|c| format!("{}{MASK}{}", &c[1], &c[3]));
    apply(&BEARER, &|c| format!("{}{MASK}", &c[1]));
    apply(&TOKENS, &|_| MASK.to_string());
    apply(&LONG_BASE64, &|c| {
        let s = &c[0];
        let mixed = s.bytes().any(|b| b.is_ascii_uppercase())
            && s.bytes().any(|b| b.is_ascii_lowercase())
            && s.bytes().filter(u8::is_ascii_digit).count() >= 3;
        if mixed { MASK.to_string() } else { s.to_string() }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_key_value_pairs_and_keeps_keys() {
        let cases = [
            ("password=hunter2", "password=***"),
            (r#""api_key": "abc123xyz""#, r#""api_key": "***""#),
            ("DB_PASSWORD = 'p@ss w'", "DB_PASSWORD = '***'"),
            ("token = \"\"", "token = \"***\""),
            ("Server=db;User Id=sa;Password=Pa55;Database=x", "Server=db;User Id=sa;Password=***;Database=x"),
            ("邮箱密码：abc12345", "邮箱密码：***"),
            ("client_secret: s3cr3t", "client_secret: ***"),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "{input}");
        }
    }

    #[test]
    fn masks_tokens_urls_and_long_base64() {
        assert_eq!(redact("key AKIAIOSFODNN7EXAMPLE end"), "key *** end");
        assert_eq!(redact("ghp_0123456789abcdefghijABCDEFGHIJ0123456789"), "***");
        assert_eq!(redact("Authorization: Bearer abc.def-123456"), "Authorization: Bearer ***");
        assert_eq!(redact("postgres://admin:topsecret@db:5432/app"), "postgres://admin:***@db:5432/app");
        assert_eq!(redact("MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7"), "***");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        for s in [
            "报价单 ACME1234 交期 2026-10-19",
            "commit 3f9b09e4c1d2a8b7e6f5a4b3c2d1e0f9a8b7c6d5",
            "id 550e8400-e29b-41d4-a716-446655440000",
            "see https://example.com/path?q=1",
            "import Button from 'src/components/SomeLongName/Button2Component'",
            "tokens: 5 passed",
        ] {
            assert!(matches!(redact(s), Cow::Borrowed(_)), "{s} -> {}", redact(s));
        }
    }
}
