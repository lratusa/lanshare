//! 文件名清理：把对方给的任意字符串变成能安全放进共享目录的文件名。

const MAX_NAME_BYTES: usize = 200;

/// Windows 文件名里不允许出现的字符，以及控制字符。
fn is_illegal(c: char) -> bool {
    matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
}

/// 看不见、却会改变显示效果的格式字符：双向控制符（能把 “gpj.exe” 显示成 “exe.jpg”）、
/// 零宽字符、BOM、软连字符、标签字符等。
fn is_invisible_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{FEFF}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{2069}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{E0000}'..='\u{E007F}')
}

/// Windows 设备名：不管带不带扩展名都会被系统当成设备。
fn is_reserved(stem: &str) -> bool {
    let upper = stem.trim().to_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ["COM", "LPT"].iter().any(|prefix| {
            upper.strip_prefix(prefix).is_some_and(|rest| {
                let mut chars = rest.chars();
                matches!(
                    (chars.next(), chars.next()),
                    (Some('0'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}'), None)
                )
            })
        })
}

/// 在不切断多字节字符的前提下，把字符串截到最多 `max` 字节。
fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 清理文件名：只取最后一段路径；去掉非法字符和看不见的格式字符；去掉首尾的空格和点
/// （开头的点也去掉：不产生隐藏文件，也不会和临时文件前缀撞上）；Windows 设备名前加 `_`；
/// 按 UTF-8 不超过 200 字节且尽量保留扩展名；清理完是空的就叫 `unnamed`。
pub fn sanitize_filename(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .filter(|&c| !is_illegal(c) && !is_invisible_format(c))
        .collect();
    let trimmed = cleaned.trim().trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() {
        return "unnamed".to_string();
    }
    let stem = trimmed.split('.').next().unwrap_or("");
    let named = if is_reserved(stem) {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    };
    if named.len() <= MAX_NAME_BYTES {
        return named;
    }
    // 太长：保留扩展名截前半部分；扩展名本身离谱地长就当成名字的一部分一起截。
    let (base, ext) = match named.rfind('.') {
        Some(i) if i > 0 && named.len() - i <= MAX_NAME_BYTES / 4 => named.split_at(i),
        _ => (named.as_str(), ""),
    };
    let base = truncate_bytes(base, MAX_NAME_BYTES - ext.len());
    let result = format!("{base}{ext}");
    let result = result.trim_end_matches(['.', ' ']);
    if result.is_empty() {
        "unnamed".to_string()
    } else {
        result.to_string()
    }
}
