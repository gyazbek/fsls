//! Just enough of Apple's XML property-list format to read `diskutil info -plist` output.
//! Pure functions, so they are also compiled (and tested) on other platforms.

/// The value after the first `<key>name</key>`: a string, number, or `true`/`false`.
pub fn value(xml: &str, key: &str) -> Option<String> {
    let tag = format!("<key>{key}</key>");
    let rest = xml[xml.find(&tag)? + tag.len()..].trim_start();
    if rest.starts_with("<true/>") {
        return Some("true".into());
    }
    if rest.starts_with("<false/>") {
        return Some("false".into());
    }
    if rest.starts_with("<string/>") {
        return Some(String::new());
    }
    for t in ["string", "integer", "real"] {
        if let Some(r) = rest.strip_prefix(&format!("<{t}>")) {
            let end = r.find(&format!("</{t}>"))?;
            return Some(unescape(&r[..end]));
        }
    }
    None
}

/// The values of every `<key>name</key>`, e.g. each entry of an `APFSPhysicalStores` array.
pub fn values(xml: &str, key: &str) -> Vec<String> {
    let tag = format!("<key>{key}</key>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&tag) {
        rest = &rest[i..];
        out.extend(value(rest, key));
        rest = &rest[tag.len()..];
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// A parsed property-list node (for nested output such as `diskutil list -plist`).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Dict(Vec<(String, Value)>),
    Array(Vec<Value>),
    Str(String),
    Int(i64),
    Bool(bool),
    /// `<real>`, `<data>`, `<date>`: not needed, kept as a placeholder.
    Other,
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn array(&self) -> &[Value] {
        match self {
            Value::Array(v) => v,
            _ => &[],
        }
    }

    /// `node.get(key)` as a string, or "".
    pub fn str_of(&self, key: &str) -> &str {
        self.get(key).and_then(Value::str).unwrap_or("")
    }
}

/// Parse a whole `<plist>` document. `None` if it is not well formed.
pub fn parse(xml: &str) -> Option<Value> {
    let mut rest = &xml[xml.find("<plist")?..];
    rest = &rest[rest.find('>')? + 1..];
    node(&mut rest)
}

/// Parse one element at the front of `s`, advancing past it.
fn node(s: &mut &str) -> Option<Value> {
    // Split at ASCII '<' and '>' only: slicing at byte 1 would panic on a multi-byte char.
    let r = s.trim_start().strip_prefix('<')?;
    let close = r.find('>')?;
    let tag = &r[..close];
    *s = &r[close + 1..];
    // Self-closing forms: <true/> <false/> <string/> <dict/> <array/>
    if let Some(name) = tag.strip_suffix('/') {
        return Some(match name {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "string" => Value::Str(String::new()),
            "dict" => Value::Dict(Vec::new()),
            "array" => Value::Array(Vec::new()),
            _ => Value::Other,
        });
    }
    match tag {
        "dict" => {
            let mut items = Vec::new();
            loop {
                *s = s.trim_start();
                if let Some(r) = s.strip_prefix("</dict>") {
                    *s = r;
                    return Some(Value::Dict(items));
                }
                let key = text(s, "key")?;
                items.push((key, node(s)?));
            }
        }
        "array" => {
            let mut items = Vec::new();
            loop {
                *s = s.trim_start();
                if let Some(r) = s.strip_prefix("</array>") {
                    *s = r;
                    return Some(Value::Array(items));
                }
                items.push(node(s)?);
            }
        }
        "string" | "integer" | "real" | "data" | "date" => {
            let end = format!("</{tag}>");
            let i = s.find(&end)?;
            let body = unescape(&s[..i]);
            *s = &s[i + end.len()..];
            Some(match tag {
                "string" => Value::Str(body),
                "integer" => body.trim().parse().map(Value::Int).unwrap_or(Value::Other),
                _ => Value::Other,
            })
        }
        _ => None,
    }
}

/// `<key>text</key>` at the front of `s`.
fn text(s: &mut &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let r = s.strip_prefix(open.as_str())?;
    let i = r.find(&close)?;
    let out = unescape(&r[..i]);
    *s = &r[i + close.len()..];
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>APFSPhysicalStores</key>
	<array>
		<dict>
			<key>APFSPhysicalStore</key>
			<string>disk0s2</string>
		</dict>
		<dict>
			<key>APFSPhysicalStore</key>
			<string>disk1s2</string>
		</dict>
	</array>
	<key>BusProtocol</key>
	<string>Apple Fabric</string>
	<key>Ejectable</key>
	<false/>
	<key>MediaName</key>
	<string>APPLE SSD &amp; Co</string>
	<key>Size</key>
	<integer>500277792768</integer>
	<key>SolidState</key>
	<true/>
	<key>TotalSize</key>
	<integer>1</integer>
	<key>VolumeName</key>
	<string/>
</dict>
</plist>"#;

    #[test]
    fn reads_values() {
        assert_eq!(value(INFO, "BusProtocol").as_deref(), Some("Apple Fabric"));
        assert_eq!(value(INFO, "MediaName").as_deref(), Some("APPLE SSD & Co"));
        assert_eq!(value(INFO, "Size").as_deref(), Some("500277792768"));
        assert_eq!(value(INFO, "SolidState").as_deref(), Some("true"));
        assert_eq!(value(INFO, "Ejectable").as_deref(), Some("false"));
        assert_eq!(value(INFO, "VolumeName").as_deref(), Some(""));
        assert_eq!(value(INFO, "Missing"), None);
        assert_eq!(values(INFO, "APFSPhysicalStore"), ["disk0s2", "disk1s2"]);
    }

    #[test]
    fn parses_tree() {
        let v = parse(INFO).expect("well formed");
        assert_eq!(v.str_of("BusProtocol"), "Apple Fabric");
        assert_eq!(v.str_of("MediaName"), "APPLE SSD & Co");
        assert_eq!(v.get("Size").and_then(Value::int), Some(500277792768));
        assert_eq!(v.get("SolidState"), Some(&Value::Bool(true)));
        assert_eq!(v.str_of("VolumeName"), "");
        let stores: Vec<&str> = v
            .get("APFSPhysicalStores")
            .map(Value::array)
            .unwrap_or_default()
            .iter()
            .map(|d| d.str_of("APFSPhysicalStore"))
            .collect();
        assert_eq!(stores, ["disk0s2", "disk1s2"]);
        assert_eq!(parse("<plist><dict><key>a</key></dict></plist>"), None);
        assert_eq!(parse("not a plist"), None);
        // Used to panic: '>' right after the root tag, and a multi-byte char after '<'.
        assert_eq!(parse("<plist>>"), None);
        assert_eq!(parse("<plist><é>"), None);
    }

    #[test]
    fn never_panics_on_malformed_input() {
        for s in crate::testutil::inputs(INFO, 3000) {
            let _ = parse(&s);
            let _ = value(&s, "BusProtocol");
            let _ = values(&s, "APFSPhysicalStore");
        }
    }
}
