use serde_json::Value;

fn uri_to_path(uri: &str) -> &str {
    uri.strip_prefix("file://").unwrap_or(uri)
}

fn format_location(loc: &Value) -> String {
    let path = loc["uri"].as_str().map(uri_to_path).unwrap_or("?");
    let line = loc["range"]["start"]["line"].as_u64().unwrap_or(0) + 1;
    let col = loc["range"]["start"]["character"].as_u64().unwrap_or(0) + 1;
    format!("{path}:{line}:{col}")
}

fn symbol_kind(kind: u64) -> &'static str {
    match kind {
        1 => "File", 2 => "Module", 3 => "Namespace", 4 => "Package", 5 => "Class",
        6 => "Method", 7 => "Property", 8 => "Field", 9 => "Constructor", 10 => "Enum",
        11 => "Interface", 12 => "Function", 13 => "Variable", 14 => "Constant",
        15 => "String", 16 => "Number", 17 => "Boolean", 18 => "Array", 19 => "Object",
        20 => "Key", 21 => "Null", 22 => "EnumMember", 23 => "Struct", 24 => "Event",
        25 => "Operator", 26 => "TypeParameter", _ => "Unknown",
    }
}

pub fn format_hover(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No hover information found.".into();
    };
    let contents = &result["contents"];
    if let Some(value) = contents.get("value").and_then(|v| v.as_str()) {
        return value.to_owned();
    }
    if let Some(arr) = contents.as_array() {
        return arr
            .iter()
            .map(|c| c.get("value").and_then(|v| v.as_str()).unwrap_or_else(|| c.as_str().unwrap_or("?")))
            .collect::<Vec<_>>()
            .join("\n---\n");
    }
    contents.as_str().unwrap_or("No hover information found.").to_owned()
}

pub fn format_definition(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No definition found.".into();
    };
    if result.is_object() {
        return format_location(result);
    }
    if let Some(arr) = result.as_array() {
        if arr.is_empty() {
            return "No definition found.".into();
        }
        return arr.iter().map(format_location).collect::<Vec<_>>().join("\n");
    }
    "No definition found.".into()
}

pub fn format_references(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No references found.".into();
    };
    let Some(arr) = result.as_array() else {
        return "No references found.".into();
    };
    if arr.is_empty() {
        return "No references found.".into();
    }
    arr.iter().map(format_location).collect::<Vec<_>>().join("\n")
}

pub fn format_symbols(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No symbols found.".into();
    };
    let Some(arr) = result.as_array() else {
        return "No symbols found.".into();
    };
    let mut lines = Vec::new();
    walk_symbols(arr, 0, &mut lines);
    if lines.is_empty() { "No symbols found.".into() } else { lines.join("\n") }
}

fn walk_symbols(symbols: &[Value], indent: usize, lines: &mut Vec<String>) {
    for s in symbols {
        let kind = symbol_kind(s["kind"].as_u64().unwrap_or(0));
        let name = s["name"].as_str().unwrap_or("?");
        let detail = s.get("detail").and_then(|v| v.as_str()).unwrap_or("");
        let line_num = s["range"]["start"]["line"]
            .as_u64()
            .or_else(|| s["location"]["range"]["start"]["line"].as_u64())
            .map(|n| n + 1)
            .unwrap_or(0);
        let prefix = "  ".repeat(indent);
        let detail_str = if detail.is_empty() { String::new() } else { format!(" — {detail}") };
        lines.push(format!("{prefix}{kind} {name}{detail_str} (line {line_num})"));
        if let Some(children) = s.get("children").and_then(|c| c.as_array()) {
            walk_symbols(children, indent + 1, lines);
        }
    }
}

pub fn format_workspace_symbols(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No symbols found.".into();
    };
    let Some(arr) = result.as_array() else {
        return "No symbols found.".into();
    };
    if arr.is_empty() {
        return "No symbols found.".into();
    }
    arr.iter()
        .map(|s| {
            let kind = symbol_kind(s["kind"].as_u64().unwrap_or(0));
            let name = s["name"].as_str().unwrap_or("?");
            let container = s.get("containerName").and_then(|v| v.as_str()).unwrap_or("");
            let loc_str = if s.get("location").is_some() { format_location(&s["location"]) } else { "?".into() };
            let container_str = if container.is_empty() { String::new() } else { format!(" in {container}") };
            format!("{kind} {name}{container_str}  →  {loc_str}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn format_diagnostics(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No diagnostics.".into();
    };
    let items = if result.is_array() {
        result.as_array().unwrap()
    } else if let Some(arr) = result.get("items").and_then(|v| v.as_array()) {
        arr
    } else {
        return "No diagnostics.".into();
    };
    if items.is_empty() {
        return "No diagnostics.".into();
    }
    items
        .iter()
        .map(|d| {
            let sev = match d["severity"].as_u64().unwrap_or(3) {
                1 => "ERROR", 2 => "WARN", 4 => "HINT", _ => "INFO",
            };
            let line = d["range"]["start"]["line"].as_u64().unwrap_or(0) + 1;
            let col = d["range"]["start"]["character"].as_u64().unwrap_or(0) + 1;
            let msg = d["message"].as_str().unwrap_or("");
            format!("[{sev}] line {line}:{col} — {msg}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn completion_kind(kind: u64) -> &'static str {
    match kind {
        1 => "Text", 2 => "Method", 3 => "Function", 4 => "Constructor",
        5 => "Field", 6 => "Variable", 7 => "Class", 8 => "Interface",
        9 => "Module", 10 => "Property", 13 => "Enum", 14 => "Keyword",
        15 => "Snippet", 25 => "TypeParam", _ => "",
    }
}

pub fn format_completion(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No completions.".into();
    };
    let items = if result.is_array() {
        result.as_array().unwrap()
    } else if let Some(arr) = result.get("items").and_then(|v| v.as_array()) {
        arr
    } else {
        return "No completions.".into();
    };
    if items.is_empty() {
        return "No completions.".into();
    }
    let cap = items.len().min(30);
    let mut lines: Vec<String> = items[..cap]
        .iter()
        .map(|item| {
            let label = item["label"].as_str().unwrap_or("?");
            let detail = item.get("detail").and_then(|v| v.as_str()).unwrap_or("");
            let kind = completion_kind(item["kind"].as_u64().unwrap_or(0));
            let kind_str = if kind.is_empty() { String::new() } else { format!("[{kind}] ") };
            let detail_str = if detail.is_empty() { String::new() } else { format!(" — {detail}") };
            format!("{kind_str}{label}{detail_str}")
        })
        .collect();
    if items.len() > 30 {
        lines.push(format!("... and {} more", items.len() - 30));
    }
    lines.join("\n")
}
