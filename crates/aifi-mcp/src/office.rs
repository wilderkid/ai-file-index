//! docx / xlsx / pptx 的文字提取，供 `search_content` 使用。
//!
//! 这些格式是 zip 包里的 XML。这里只实现读文字所需的最小子集，不引入通用 zip / XML 库：
//! - zip：读中央目录，只支持 stored（0）与 deflate（8）。zip64、加密、其他压缩方式一律放弃该文件；
//!   不校验 CRC（只读文字，坏数据最多得到乱码）。
//! - XML：只取文字节点，按段落 / 行断行；解码五个预定义实体与数字字符引用；跳过注释，保留 CDATA。
//!
//! "zip 炸弹"防护：条目数、单条解压后大小、整包解压后总大小、输出文字长度都有上限。

use std::collections::HashMap;

const MAX_ENTRIES: usize = 20_000;
/// 单个条目解压后最多这么大。
const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
/// 一个文件里所有条目解压后加起来最多这么大。
const MAX_UNPACKED_BYTES: usize = 48 * 1024 * 1024;
/// 最多读这么多个部件（Word 页眉页脚、工作表、幻灯片）。
const MAX_PARTS: usize = 200;
/// 提取出的文字最多这么长，超出部分丢弃。
const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// 支持的文档类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Docx,
    Xlsx,
    Pptx,
}

impl Kind {
    /// 按扩展名判断。
    pub fn from_path(path: &str) -> Option<Self> {
        let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
        match name.rsplit_once('.')?.1.to_ascii_lowercase().as_str() {
            "docx" | "docm" | "dotx" => Some(Kind::Docx),
            "xlsx" | "xlsm" | "xltx" => Some(Kind::Xlsx),
            "pptx" | "pptm" | "potx" => Some(Kind::Pptx),
            _ => None,
        }
    }
}

/// 提取结果：每行一段文字，`labels[i]` 是第 i 行的位置（`报价!12`、`slide 3`），Word 为空串。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Doc {
    pub text: String,
    pub labels: Vec<String>,
}

impl Doc {
    fn line(&mut self, label: &str, text: &str) {
        let t = text.trim();
        if t.is_empty() || self.text.len() >= MAX_TEXT_BYTES {
            return;
        }
        // 行号要与 labels 对齐：段落内的换行一律变成空格。
        self.text.extend(t.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }));
        self.text.push('\n');
        self.labels.push(label.to_string());
    }
}

/// 提取文字。不是合法的文档、加密或使用不支持的压缩方式时返回 `None`。
pub fn extract(kind: Kind, bytes: &[u8]) -> Option<Doc> {
    let mut zip = Zip { z: bytes, entries: entries(bytes)?, budget: MAX_UNPACKED_BYTES };
    let mut doc = Doc::default();
    match kind {
        Kind::Docx => docx(&mut zip, &mut doc)?,
        Kind::Xlsx => xlsx(&mut zip, &mut doc)?,
        Kind::Pptx => pptx(&mut zip, &mut doc)?,
    }
    Some(doc)
}

// ---- zip ----

struct Entry {
    name: String,
    flags: u16,
    method: u16,
    comp: usize,
    offset: usize,
}

struct Zip<'a> {
    z: &'a [u8],
    entries: Vec<Entry>,
    budget: usize,
}

impl Zip<'_> {
    /// 读一个部件并按 UTF-8 解码。找不到、解压失败或超出预算时返回 `None`。
    fn read(&mut self, name: &str) -> Option<String> {
        let e = self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))?;
        if e.flags & 1 != 0 {
            return None; // 加密
        }
        if u32_at(self.z, e.offset)? != 0x0403_4B50 {
            return None;
        }
        let (n, x) = (u16_at(self.z, e.offset + 26)? as usize, u16_at(self.z, e.offset + 28)? as usize);
        let start = e.offset.checked_add(30 + n + x)?;
        let data = self.z.get(start..start.checked_add(e.comp)?)?;
        let cap = MAX_ENTRY_BYTES.min(self.budget);
        let out = match e.method {
            0 if data.len() <= cap => data.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, cap).ok()?,
            _ => return None,
        };
        self.budget -= out.len();
        Some(String::from_utf8_lossy(&out).into_owned())
    }

    /// 名字形如 `{prefix}{N}.xml` 的部件，按 N 排序。
    fn numbered(&self, prefix: &str) -> Vec<(u32, String)> {
        let mut v: Vec<(u32, String)> = self
            .entries
            .iter()
            .filter_map(|e| {
                let l = e.name.to_ascii_lowercase();
                let n = l.strip_prefix(prefix)?.strip_suffix(".xml")?.parse().ok()?;
                Some((n, e.name.clone()))
            })
            .collect();
        v.sort();
        v
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// 读中央目录。结尾记录在最后 22 + 65535（最长注释）字节内。
fn entries(z: &[u8]) -> Option<Vec<Entry>> {
    let last = z.len().checked_sub(22)?;
    let at = (last.saturating_sub(65_535)..=last).rev().find(|&i| u32_at(z, i) == Some(0x0605_4B50))?;
    let count = u16_at(z, at + 10)? as usize;
    let cd_size = u32_at(z, at + 12)? as usize;
    let cd_off = u32_at(z, at + 16)?;
    if count == 0xFFFF || cd_off == u32::MAX || count > MAX_ENTRIES {
        return None; // zip64 或过多条目
    }
    let cd_off = cd_off as usize;
    let cd = z.get(cd_off..cd_off.checked_add(cd_size)?)?;
    let mut out = Vec::with_capacity(count);
    let mut p = 0usize;
    for _ in 0..count {
        if u32_at(cd, p)? != 0x0201_4B50 {
            return None;
        }
        let n = u16_at(cd, p + 28)? as usize;
        let skip = n + u16_at(cd, p + 30)? as usize + u16_at(cd, p + 32)? as usize;
        let name = cd.get(p + 46..p + 46 + n)?;
        out.push(Entry {
            name: String::from_utf8_lossy(name).replace('\\', "/"),
            flags: u16_at(cd, p + 8)?,
            method: u16_at(cd, p + 10)?,
            comp: u32_at(cd, p + 20)? as usize,
            offset: u32_at(cd, p + 42)? as usize,
        });
        p = p.checked_add(46 + skip)?;
    }
    Some(out)
}

// ---- XML ----

enum Ev<'a> {
    /// 本地名（去掉命名空间前缀）与整个标签文本（含属性）。
    Open(&'a str, &'a str),
    Empty(&'a str, &'a str),
    Close(&'a str),
    /// 未解码的文字。
    Text(&'a str),
    /// CDATA 原文。
    Raw(&'a str),
}

fn events<'a>(xml: &'a str, mut f: impl FnMut(Ev<'a>)) {
    let mut rest = xml;
    while let Some(i) = rest.find('<') {
        if i > 0 {
            f(Ev::Text(&rest[..i]));
        }
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.find("-->").map_or("", |e| &r[e + 3..]);
            continue;
        }
        if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let e = r.find("]]>").unwrap_or(r.len());
            f(Ev::Raw(&r[..e]));
            rest = r.get(e + 3..).unwrap_or("");
            continue;
        }
        let Some(end) = rest.find('>') else { return };
        let tag = &rest[1..end];
        rest = &rest[end + 1..];
        if tag.starts_with(['?', '!']) {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            f(Ev::Close(local(name)));
        } else if let Some(t) = tag.strip_suffix('/') {
            f(Ev::Empty(local(t), t));
        } else {
            f(Ev::Open(local(tag), tag));
        }
    }
    if !rest.is_empty() {
        f(Ev::Text(rest));
    }
}

fn local(tag: &str) -> &str {
    let name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
    name.rsplit(':').next().unwrap_or(name)
}

/// 取属性值（未解码）。`key` 前面必须是空白、后面必须是 `=`，避免 `t` 匹配到 `Target`。
fn attr<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = tag[from..].find(key).map(|i| i + from) {
        from = i + key.len();
        if !tag[..i].ends_with(char::is_whitespace) {
            continue;
        }
        let Some(v) = tag[from..].trim_start().strip_prefix('=') else { continue };
        let v = v.trim_start();
        let q = v.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        return v[1..].find(q).map(|e| &v[1..1 + e]);
    }
    None
}

fn unescape_into(s: &str, out: &mut String) {
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').filter(|&e| e <= 12).and_then(|e| {
            let c = match &rest[1..e] {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                ent => ent
                    .strip_prefix('#')
                    .and_then(|n| match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok(),
                        None => n.parse().ok(),
                    })
                    .and_then(char::from_u32),
            }?;
            Some((c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
}

/// Word / PowerPoint：`<w:t>` / `<a:t>` 里的文字，`</w:p>` / `</a:p>` 断行。
fn paragraphs(xml: &str, doc: &mut Doc, label: &str) {
    let mut cur = String::new();
    let mut in_t = 0u32;
    events(xml, |ev| match ev {
        Ev::Open("t", _) => in_t += 1,
        Ev::Close("t") => in_t = in_t.saturating_sub(1),
        Ev::Text(s) if in_t > 0 => unescape_into(s, &mut cur),
        Ev::Raw(s) if in_t > 0 => cur.push_str(s),
        Ev::Empty("tab", _) => cur.push('\t'),
        Ev::Empty("br" | "cr", _) => cur.push(' '),
        Ev::Close("p") => {
            doc.line(label, &cur);
            cur.clear();
        }
        _ => {}
    });
    doc.line(label, &cur);
}

/// 正文在前，其后是页眉、页脚、脚注、批注等（`word/` 下的其他 XML；没有 `<w:t>` 的部件不产生文字）。
fn docx(zip: &mut Zip<'_>, doc: &mut Doc) -> Option<()> {
    let mut parts: Vec<String> = zip
        .entries
        .iter()
        .map(|e| e.name.clone())
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.strip_prefix("word/").is_some_and(|r| r.ends_with(".xml") && !r.contains('/'))
        })
        .collect();
    parts.sort_by_key(|n| (!n.eq_ignore_ascii_case("word/document.xml"), n.to_ascii_lowercase()));
    if !parts.first().is_some_and(|n| n.eq_ignore_ascii_case("word/document.xml")) {
        return None;
    }
    for p in parts.iter().take(MAX_PARTS) {
        if let Some(xml) = zip.read(p) {
            paragraphs(&xml, doc, "");
        }
    }
    Some(())
}

fn pptx(zip: &mut Zip<'_>, doc: &mut Doc) -> Option<()> {
    let slides = zip.numbered("ppt/slides/slide");
    if slides.is_empty() {
        return None;
    }
    for (n, path) in slides.into_iter().take(MAX_PARTS) {
        if let Some(xml) = zip.read(&path) {
            paragraphs(&xml, doc, &format!("slide {n}"));
        }
    }
    Some(())
}

/// Excel：每行一行文字（单元格用制表符分隔），位置标成 `工作表!行号`。
fn xlsx(zip: &mut Zip<'_>, doc: &mut Doc) -> Option<()> {
    let shared = zip.read("xl/sharedStrings.xml").map(|x| shared_strings(&x)).unwrap_or_default();
    let sheets = sheet_list(zip);
    if sheets.is_empty() {
        return None;
    }
    for (name, path) in sheets.into_iter().take(MAX_PARTS) {
        if let Some(xml) = zip.read(&path) {
            sheet(&xml, &shared, &name, doc);
        }
    }
    Some(())
}

/// `<si>` 一项一个字符串；富文本的多个 `<t>` 拼在一起；跳过 `<rPh>` 注音。
fn shared_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut in_t, mut in_rph) = (0u32, 0u32);
    events(xml, |ev| match ev {
        Ev::Open("si", _) => cur.clear(),
        Ev::Close("si") => out.push(std::mem::take(&mut cur)),
        Ev::Empty("si", _) => out.push(String::new()),
        Ev::Open("rPh", _) => in_rph += 1,
        Ev::Close("rPh") => in_rph = in_rph.saturating_sub(1),
        Ev::Open("t", _) => in_t += 1,
        Ev::Close("t") => in_t = in_t.saturating_sub(1),
        Ev::Text(s) if in_t > 0 && in_rph == 0 => unescape_into(s, &mut cur),
        Ev::Raw(s) if in_t > 0 && in_rph == 0 => cur.push_str(s),
        _ => {}
    });
    out
}

/// 工作表的（显示名, 部件路径），按工作簿里的顺序。工作簿解析失败时退回 `sheetN.xml`。
fn sheet_list(zip: &mut Zip<'_>) -> Vec<(String, String)> {
    let mut rels = HashMap::new();
    if let Some(x) = zip.read("xl/_rels/workbook.xml.rels") {
        events(&x, |ev| {
            if let Ev::Open("Relationship", tag) | Ev::Empty("Relationship", tag) = ev
                && let (Some(id), Some(target)) = (attr(tag, "Id"), attr(tag, "Target"))
            {
                let t = target.trim_start_matches('/');
                let path = if t.to_ascii_lowercase().starts_with("xl/") { t.to_string() } else { format!("xl/{t}") };
                rels.insert(id.to_string(), path);
            }
        });
    }
    let mut out = Vec::new();
    if let Some(wb) = zip.read("xl/workbook.xml") {
        events(&wb, |ev| {
            if let Ev::Open("sheet", tag) | Ev::Empty("sheet", tag) = ev
                && let (Some(name), Some(path)) = (attr(tag, "name"), attr(tag, "r:id").and_then(|id| rels.get(id)))
            {
                let mut n = String::new();
                unescape_into(name, &mut n);
                out.push((n, path.clone()));
            }
        });
    }
    if out.is_empty() {
        out = zip.numbered("xl/worksheets/sheet").into_iter().map(|(n, p)| (format!("sheet{n}"), p)).collect();
    }
    out
}

#[derive(Clone, Copy)]
enum CellType {
    Shared,
    Inline,
    Other,
}

fn sheet(xml: &str, shared: &[String], name: &str, doc: &mut Doc) {
    let mut cells: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut row = String::new();
    let mut rows = 0usize;
    let mut ty = CellType::Other;
    let (mut in_v, mut in_is, mut in_t) = (false, false, false);
    events(xml, |ev| match ev {
        Ev::Open("row", tag) => {
            rows += 1;
            row = attr(tag, "r").map_or_else(|| rows.to_string(), str::to_string);
            cells.clear();
        }
        Ev::Close("row") => {
            let line: Vec<&str> = cells.iter().map(String::as_str).filter(|c| !c.is_empty()).collect();
            doc.line(&format!("{name}!{row}"), &line.join("\t"));
            cells.clear();
        }
        Ev::Open("c", tag) => {
            ty = match attr(tag, "t") {
                Some("s") => CellType::Shared,
                Some("inlineStr") => CellType::Inline,
                _ => CellType::Other,
            };
            cur.clear();
        }
        Ev::Close("c") => cells.push(std::mem::take(&mut cur)),
        Ev::Open("v", _) => in_v = true,
        Ev::Close("v") => in_v = false,
        Ev::Open("is", _) => in_is = true,
        Ev::Close("is") => in_is = false,
        Ev::Open("t", _) => in_t = true,
        Ev::Close("t") => in_t = false,
        Ev::Text(s) if in_v => match ty {
            CellType::Shared => {
                if let Some(v) = s.trim().parse::<usize>().ok().and_then(|i| shared.get(i)) {
                    cur.push_str(v);
                }
            }
            CellType::Inline | CellType::Other => unescape_into(s, &mut cur),
        },
        Ev::Text(s) if in_is && in_t => unescape_into(s, &mut cur),
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼一个最小的 zip：`(名字, 内容, 是否 deflate)`。
    fn zip(files: &[(&str, &str, bool)]) -> Vec<u8> {
        let (mut out, mut cd) = (Vec::new(), Vec::new());
        for (name, data, deflate) in files {
            let data = data.as_bytes();
            let comp = if *deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.to_vec() };
            let method: u16 = if *deflate { 8 } else { 0 };
            let off = out.len() as u32;
            out.extend(0x0403_4B50u32.to_le_bytes());
            out.extend([20, 0, 0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0; 8]); // 时间、日期、CRC
            out.extend((comp.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend([0, 0]);
            out.extend(name.as_bytes());
            out.extend(&comp);

            cd.extend(0x0201_4B50u32.to_le_bytes());
            cd.extend([20, 0, 20, 0, 0, 0]);
            cd.extend(method.to_le_bytes());
            cd.extend([0; 8]);
            cd.extend((comp.len() as u32).to_le_bytes());
            cd.extend((data.len() as u32).to_le_bytes());
            cd.extend((name.len() as u16).to_le_bytes());
            cd.extend([0; 12]); // extra、comment 长度，盘号，内部 / 外部属性
            cd.extend(off.to_le_bytes());
            cd.extend(name.as_bytes());
        }
        let cd_off = out.len() as u32;
        out.extend(&cd);
        out.extend(0x0605_4B50u32.to_le_bytes());
        out.extend([0; 4]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((cd.len() as u32).to_le_bytes());
        out.extend(cd_off.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    fn lines(d: &Doc) -> Vec<(&str, &str)> {
        d.text.lines().zip(d.labels.iter().map(String::as_str)).map(|(t, l)| (l, t)).collect()
    }

    #[test]
    fn kind_from_extension() {
        assert_eq!(Kind::from_path(r"E:\客户\报价.XLSX"), Some(Kind::Xlsx));
        assert_eq!(Kind::from_path(r"C:\a.docm"), Some(Kind::Docx));
        assert_eq!(Kind::from_path(r"C:\x.pptx\notes.txt"), None);
        assert_eq!(Kind::from_path(r"C:\a.doc"), None, "旧二进制格式不支持");
    }

    #[test]
    fn docx_paragraphs_entities_and_headers() {
        let body = r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>
            <w:p><w:r><w:t>交期</w:t></w:r><w:r><w:t xml:space="preserve"> 2026-10-19 &amp; 付款 &#x263A;</w:t></w:r></w:p>
            <w:p><w:r><w:t>第二段</w:t><w:br/><w:t>同一段</w:t></w:r></w:p>
            <!-- <w:t>注释里的不算</w:t> -->
            <w:p><w:r><w:instrText>PAGE</w:instrText></w:r></w:p></w:body></w:document>"#;
        let header = r#"<w:hdr xmlns:w="x"><w:p><w:r><w:t>页眉文字</w:t></w:r></w:p></w:hdr>"#;
        let z = zip(&[
            ("word/header1.xml", header, true),
            ("word/document.xml", body, true),
            ("[Content_Types].xml", "<x/>", false),
        ]);
        let d = extract(Kind::Docx, &z).unwrap();
        assert_eq!(d.text, "交期 2026-10-19 & 付款 ☺\n第二段 同一段\n页眉文字\n");
        assert!(d.labels.iter().all(String::is_empty));
    }

    #[test]
    fn xlsx_rows_with_sheet_names_shared_inline_and_numbers() {
        let wb = r#"<workbook xmlns:r="r"><sheets><sheet name="报价 &amp; 订单" sheetId="1" r:id="rId7"/></sheets></workbook>"#;
        let rels =
            r#"<Relationships><Relationship Id="rId7" Type="t" Target="worksheets/sheet3.xml"/></Relationships>"#;
        let sst = r#"<sst><si><t>ACME1234</t></si><si><r><t>TEST01 </t></r><r><t>电池</t></r><rPh><t>でんち</t></rPh></si></sst>"#;
        let ws = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>1234.5</v></c><c r="C1" t="s"><v>99</v></c></row>
            <row r="4"><c r="A4" t="inlineStr"><is><t>备注 &lt;急&gt;</t></is></c><c r="B4" t="s"><v>1</v></c><c r="C4"><f>SUM(B1)</f><v>1234.5</v></c></row>
            <row r="5"/></sheetData></worksheet>"#;
        let z = zip(&[
            ("xl/workbook.xml", wb, true),
            ("xl/_rels/workbook.xml.rels", rels, false),
            ("xl/sharedStrings.xml", sst, true),
            ("xl/worksheets/sheet3.xml", ws, true),
        ]);
        let d = extract(Kind::Xlsx, &z).unwrap();
        assert_eq!(
            lines(&d),
            vec![("报价 & 订单!1", "ACME1234\t1234.5"), ("报价 & 订单!4", "备注 <急>\tTEST01 电池\t1234.5")]
        );
    }

    #[test]
    fn xlsx_without_workbook_falls_back_to_sheet_files() {
        let ws = r#"<worksheet><sheetData><row><c t="inlineStr"><is><t>x</t></is></c></row></sheetData></worksheet>"#;
        let z = zip(&[("xl/worksheets/sheet2.xml", ws, false), ("xl/worksheets/sheet10.xml", ws, false)]);
        let d = extract(Kind::Xlsx, &z).unwrap();
        assert_eq!(lines(&d), vec![("sheet2!1", "x"), ("sheet10!1", "x")], "按编号而不是字典序");
    }

    #[test]
    fn pptx_slides_in_numeric_order() {
        let s = |t: &str| format!(r#"<p:sld><a:p><a:r><a:t>{t}</a:t></a:r></a:p><a:p><a:t>第二行</a:t></a:p></p:sld>"#);
        let (s1, s10, s2) = (s("一"), s("十"), s("二"));
        let z = zip(&[
            ("ppt/slides/slide10.xml", &s10, true),
            ("ppt/slides/slide1.xml", &s1, true),
            ("ppt/slides/slide2.xml", &s2, true),
        ]);
        let d = extract(Kind::Pptx, &z).unwrap();
        let got: Vec<_> = lines(&d).into_iter().filter(|(_, t)| *t != "第二行").collect();
        assert_eq!(got, vec![("slide 1", "一"), ("slide 2", "二"), ("slide 10", "十")]);
    }

    #[test]
    fn bombs_encryption_and_garbage_are_rejected_without_panic() {
        // 解压后超过单条上限的条目：读失败，文档里没有正文 → None。
        let huge = format!("<w:document><w:p><w:t>{}</w:t></w:p></w:document>", "a".repeat(MAX_ENTRY_BYTES + 1));
        assert_eq!(extract(Kind::Docx, &zip(&[("word/document.xml", &huge, true)])).map(|d| d.text.len()), Some(0));

        let mut enc = zip(&[("word/document.xml", "<w:p><w:t>x</w:t></w:p>", false)]);
        let cd = enc.len() - 22 - (46 + "word/document.xml".len());
        enc[cd + 8] |= 1; // 中央目录里的加密标志
        assert_eq!(extract(Kind::Docx, &enc).map(|d| d.text), Some(String::new()));

        assert_eq!(extract(Kind::Docx, b"not a zip"), None);
        assert_eq!(extract(Kind::Xlsx, &zip(&[("word/document.xml", "<x/>", false)])), None);
        let good = zip(&[("word/document.xml", "<w:p><w:t>hello</w:t></w:p>", true)]);
        for cut in 0..good.len() {
            let _ = extract(Kind::Docx, &good[..cut]);
        }
        let mut flipped = good.clone();
        for i in 0..flipped.len() {
            flipped[i] ^= 0xFF;
            let _ = extract(Kind::Docx, &flipped);
            flipped[i] ^= 0xFF;
        }
    }

    #[test]
    fn attr_and_unescape_edge_cases() {
        assert_eq!(attr(r#"c r="A1" s="2" t="s""#, "t"), Some("s"));
        assert_eq!(attr(r#"Relationship Target="a" TargetMode="External""#, "Target"), Some("a"));
        assert_eq!(attr(r#"c r="A1""#, "t"), None);
        let mut s = String::new();
        unescape_into("a &unknown; &#99999999; &amp &lt;b&gt;", &mut s);
        assert_eq!(s, "a &unknown; &#99999999; &amp <b>");
    }
}
