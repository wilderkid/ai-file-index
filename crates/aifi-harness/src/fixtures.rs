//! 常用测试数据集。新增 fixture 时请同时覆盖：中文/多音字名、超长路径、
//! 嵌套项目、各种 .gitignore、敏感文件（必须被黑名单挡住）。

use crate::FakeVolume;

/// 中文与多音字文件名。
pub const CHINESE_NAMES: &[&str] = &[
    r"Users\me\Documents\2026Q3季度报告-终版.xlsx",
    r"Users\me\Documents\银行流水.pdf",
    r"Users\me\Documents\行动计划.md",
    r"Users\me\Desktop\会议纪要（重要）.docx",
    r"Users\me\Pictures\长城.jpg",
];

/// 敏感文件：任何查询都不得返回。
pub const SECRET_PATHS: &[&str] = &[
    r"Users\me\.ssh\id_rsa",
    r"Users\me\.ssh\id_ed25519.pub",
    r"Users\me\.aws\credentials",
    r"Users\me\AppData\Roaming\Microsoft\Credentials\DFBE70A7E5CC19A3",
    r"src\shop\.env",
    r"src\shop\.env.production",
    r"src\shop\certs\tls.key",
];

/// 典型开发机：嵌套 monorepo、Rust/Python/.NET 项目、node_modules、target。
pub fn dev_machine() -> FakeVolume {
    let mut v = FakeVolume::new("C:");
    for p in [
        r"src\shop\.git\",
        r"src\shop\.gitignore",
        r"src\shop\package.json",
        r"src\shop\packages\web\package.json",
        r"src\shop\packages\web\src\App.tsx",
        r"src\shop\packages\web\node_modules\react\index.js",
        r"src\shop\packages\api\pyproject.toml",
        r"src\shop\packages\api\app\main.py",
        r"src\shop\packages\api\.venv\Lib\site-packages\x.py",
        r"src\aifi\Cargo.toml",
        r"src\aifi\src\main.rs",
        r"src\aifi\target\debug\aifi.exe",
        r"src\legacy\Legacy.sln",
        r"src\legacy\Legacy\Program.cs",
        r"Windows\System32\notepad.exe",
    ] {
        v.add_path(p);
    }
    let long = format!(r"src\deep\{}\file.txt", ["very_long_directory_name_segment"; 12].join("\\"));
    v.add_path(&long);
    for p in CHINESE_NAMES.iter().chain(SECRET_PATHS) {
        v.add_path(p);
    }
    v
}
