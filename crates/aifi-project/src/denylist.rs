//! 隐私黑名单。默认规则见 `docs/privacy.md`。
//!
//! 语义：被拒绝的路径**留在索引里**（保证增量更新正确），但查询、树、最近变更
//! 等所有对外接口都**绝不返回**它们。黑名单不能被 `include_ignored` 覆盖。

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

/// 默认拒绝的目录（任一路径组件序列匹配即拒绝，大小写不敏感）。
/// `%APPDATA%` 等环境变量在服务启动时展开；这里用组件后缀匹配，不依赖具体用户名。
pub const DEFAULT_DENY_DIRS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    r"AppData\Roaming\Microsoft\Credentials",
    r"AppData\Local\Microsoft\Credentials",
    r"AppData\Roaming\Microsoft\Protect",
    r"AppData\Roaming\Microsoft\Vault",
    r"AppData\Local\Microsoft\Vault",
    // 浏览器配置（含 Cookie / 登录数据）
    r"AppData\Local\Google\Chrome\User Data",
    r"AppData\Local\Microsoft\Edge\User Data",
    r"AppData\Roaming\Mozilla\Firefox\Profiles",
    r"AppData\Local\BraveSoftware\Brave-Browser\User Data",
    // 密码管理器
    r"AppData\Roaming\1Password",
    r"AppData\Local\1Password",
    r"AppData\Roaming\Bitwarden",
    r"AppData\Roaming\KeePass",
    r"AppData\Roaming\KeePassXC",
];

/// 默认拒绝的文件名 glob（只匹配文件名，大小写不敏感）。
pub const DEFAULT_DENY_FILE_GLOBS: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "*.pfx",
    "*.p12",
    "id_rsa*",
    "id_ed25519*",
    "id_ecdsa*",
    "*.kdbx",
    ".npmrc",
    ".pypirc",
    ".netrc",
    "_netrc",
    ".git-credentials",
];

/// 已编译的黑名单。
pub struct Denylist {
    dirs: Vec<Vec<String>>,
    files: GlobSet,
}

impl Denylist {
    /// 默认规则。
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_DENY_DIRS, DEFAULT_DENY_FILE_GLOBS).expect("default denylist globs are valid")
    }

    /// 自定义规则（用户配置可在默认之上**追加**，不建议删除默认项）。
    pub fn new(dirs: &[&str], file_globs: &[&str]) -> Result<Self, globset::Error> {
        let dirs = dirs.iter().map(|d| crate::components(d).map(|c| c.to_ascii_lowercase()).collect()).collect();
        let mut b = GlobSetBuilder::new();
        for g in file_globs {
            b.add(glob_ci(g)?);
        }
        Ok(Self { dirs, files: b.build()? })
    }

    /// 是否拒绝该路径（目录或文件）。
    pub fn is_denied(&self, path: &str) -> bool {
        let comps: Vec<String> = crate::components(path).map(|c| c.to_ascii_lowercase()).collect();
        if let Some(name) = comps.last() {
            if self.files.is_match(name) {
                return true;
            }
        }
        self.dirs.iter().any(|pat| !pat.is_empty() && comps.windows(pat.len()).any(|w| w == pat.as_slice()))
    }
}

fn glob_ci(g: &str) -> Result<Glob, globset::Error> {
    GlobBuilder::new(&g.to_ascii_lowercase()).case_insensitive(true).literal_separator(true).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_secret_dirs_and_files() {
        let d = Denylist::with_defaults();
        for p in [
            r"C:\Users\me\.ssh\id_rsa",
            r"C:\Users\me\.SSH\config",
            r"C:\Users\me\.aws\credentials",
            r"C:\Users\me\AppData\Roaming\Microsoft\Credentials\ABC",
            r"C:\Users\me\AppData\Local\Google\Chrome\User Data\Default\Cookies",
            r"C:\src\app\.env",
            r"C:\src\app\.env.local",
            r"C:\src\app\certs\server.PEM",
            r"D:\backup\vault.kdbx",
            r"C:\Users\me\.git-credentials",
        ] {
            assert!(d.is_denied(p), "{p} should be denied");
        }
    }

    #[test]
    fn allows_normal_files() {
        let d = Denylist::with_defaults();
        for p in [
            r"C:\src\app\src\main.rs",
            r"C:\src\app\.envrc.example.md", // 不匹配 ".env" / ".env.*"
            r"C:\src\app\keys.rs",
            r"C:\Users\me\Documents\季度报告.xlsx",
            r"C:\src\ssh-tool\README.md",
        ] {
            assert!(!d.is_denied(p), "{p} should be allowed");
        }
    }
}
