use serde_json::{json, Value};
use std::collections::HashSet;
use crate::servers::ServerProfile;

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

pub fn format_capabilities(caps: &Value) -> String {
    let mut lines = Vec::new();

    // Text sync
    if let Some(sync) = caps.get("textDocumentSync") {
        let kind = match sync.as_u64().or_else(|| sync.get("change").and_then(|v| v.as_u64())) {
            Some(0) => "none",
            Some(1) => "full",
            Some(2) => "incremental",
            _ => "unknown",
        };
        lines.push(format!("textDocumentSync       : {kind}"));
    }

    let bool_cap = |key: &str| caps.get(key).map(|v| !v.is_null() && v != &Value::Bool(false)).unwrap_or(false);

    if bool_cap("hoverProvider") {
        lines.push("hover                  : yes".into());
    }
    if let Some(c) = caps.get("completionProvider").filter(|v| !v.is_null()) {
        let triggers = c.get("triggerCharacters").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let resolve = c.get("resolveProvider").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut detail = Vec::new();
        if !triggers.is_empty() { detail.push(format!("trigger: \"{triggers}\"")); }
        if resolve { detail.push("resolve".into()); }
        let detail_str = if detail.is_empty() { String::new() } else { format!(" ({})", detail.join(", ")) };
        lines.push(format!("completion             : yes{detail_str}"));
    }
    if let Some(c) = caps.get("signatureHelpProvider").filter(|v| !v.is_null()) {
        let triggers = c.get("triggerCharacters").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let detail_str = if triggers.is_empty() { String::new() } else { format!(" (trigger: \"{triggers}\")")};
        lines.push(format!("signatureHelp          : yes{detail_str}"));
    }
    if bool_cap("definitionProvider") {
        lines.push("definition             : yes".into());
    }
    for (key, label) in [
        ("implementationProvider", "implementation         : yes"),
        ("typeDefinitionProvider", "typeDefinition         : yes"),
        ("callHierarchyProvider", "callHierarchy          : yes"),
        ("typeHierarchyProvider", "typeHierarchy          : yes"),
    ] {
        if bool_cap(key) {
            lines.push(label.into());
        }
    }
    if bool_cap("referencesProvider") {
        lines.push("references             : yes".into());
    }
    if bool_cap("documentSymbolProvider") {
        lines.push("documentSymbol         : yes".into());
    }
    if bool_cap("workspaceSymbolProvider") {
        lines.push("workspaceSymbol        : yes".into());
    }
    if let Some(c) = caps.get("codeActionProvider").filter(|v| !v.is_null()) {
        let kinds = c.get("codeActionKinds").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let detail_str = if kinds.is_empty() { String::new() } else { format!(" ({kinds})") };
        lines.push(format!("codeAction             : yes{detail_str}"));
    }
    if bool_cap("documentFormattingProvider") {
        lines.push("formatting             : yes".into());
    }
    if bool_cap("renameProvider") {
        lines.push("rename                 : yes".into());
    }
    if let Some(c) = caps.get("semanticTokensProvider").filter(|v| !v.is_null()) {
        let full = c.get("full").map(|v| !v.is_null() && v != &Value::Bool(false)).unwrap_or(false);
        let range = c.get("range").map(|v| !v.is_null() && v != &Value::Bool(false)).unwrap_or(false);
        let mut modes = Vec::new();
        if full { modes.push("full"); }
        if range { modes.push("range"); }
        let modes_str = if modes.is_empty() { String::new() } else { format!(" ({})", modes.join(", ")) };
        lines.push(format!("semanticTokens         : yes{modes_str}"));
    }
    if bool_cap("inlayHintProvider") {
        lines.push("inlayHints             : yes".into());
    }
    if let Some(c) = caps.get("diagnosticProvider").filter(|v| !v.is_null()) {
        let inter = c.get("interFileDependencies").and_then(|v| v.as_bool()).unwrap_or(false);
        let workspace = c.get("workspaceDiagnostics").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut detail = Vec::new();
        if inter { detail.push("inter-file"); }
        if workspace { detail.push("workspace"); }
        let detail_str = if detail.is_empty() { String::new() } else { format!(" ({})", detail.join(", ")) };
        lines.push(format!("diagnostics            : yes{detail_str}"));
    }
    if let Some(c) = caps.get("executeCommandProvider").filter(|v| !v.is_null()) {
        let cmds = c.get("commands").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        lines.push(format!("executeCommand         : {cmds}"));
    }

    if lines.is_empty() { "No capabilities reported.".into() } else { lines.join("\n") }
}

/// Turn markdown links whose target is an editor `command:` URI into their text:
/// they run a command in an editor, and no command line caller can follow them.
fn strip_command_links(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut rest = md;
    while let Some(i) = rest.find("](command:") {
        let (Some(open), Some(close)) = (rest[..i].rfind('['), rest[i..].find(')').map(|j| i + j)) else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..i]);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Hover text. The profile may tidy it first; whatever editor `command:` links are
/// left become plain text.
pub fn format_hover(resp: &Value, profile: &dyn ServerProfile) -> String {
    strip_command_links(&profile.rewrite_hover(format_hover_raw(resp)))
}

fn format_hover_raw(resp: &Value) -> String {
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

fn dedup_locations(locs: &[Value]) -> Vec<&Value> {
    let mut seen = HashSet::new();
    locs.iter()
        .filter(|loc| {
            let key = (
                loc["uri"].as_str().unwrap_or("").to_string(),
                loc["range"]["start"]["line"].as_u64().unwrap_or(0),
                loc["range"]["start"]["character"].as_u64().unwrap_or(0),
            );
            seen.insert(key)
        })
        .collect()
}

/// Format a `Location | Location[] | LocationLink[]` result.
pub fn format_locations(resp: &Value, empty: &str) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return empty.into();
    };
    let items: Vec<Value> = match result.as_array() {
        Some(a) => a.clone(),
        None => vec![result.clone()],
    };
    // LocationLink → Location (targetUri + targetSelectionRange).
    let locs: Vec<Value> = items
        .into_iter()
        .map(|l| match l.get("targetUri") {
            Some(uri) => json!({"uri": uri, "range": l["targetSelectionRange"]}),
            None => l,
        })
        .collect();
    let unique = dedup_locations(&locs);
    if unique.is_empty() {
        return empty.into();
    }
    unique.into_iter().map(format_location).collect::<Vec<_>>().join("\n")
}

fn format_call_item(item: &Value) -> String {
    let kind = symbol_kind(item["kind"].as_u64().unwrap_or(0));
    let name = item["name"].as_str().unwrap_or("?");
    let detail = item["detail"].as_str().filter(|d| !d.is_empty()).map(|d| format!(" — {d}")).unwrap_or_default();
    let loc = format_location(&json!({"uri": item["uri"], "range": item["selectionRange"]}));
    format!("{kind} {name}{detail}  →  {loc}")
}

/// Format the result of `LspSession::calls`.
pub fn format_calls(resp: &Value, dir: crate::session::CallDirection) -> String {
    use crate::session::CallDirection;
    if let Some(msg) = resp["error"]["message"].as_str() {
        return format!("Error: {msg}");
    }
    let Some(roots) = resp["result"].as_array().filter(|a| !a.is_empty()) else {
        return "No function at this position.".into();
    };
    let mut lines = Vec::new();
    for root in roots {
        lines.push(format_call_item(&root["item"]));
        let calls = root["calls"].as_array().cloned().unwrap_or_default();
        if calls.is_empty() {
            lines.push(match dir {
                CallDirection::Incoming => "  (no callers found)".into(),
                CallDirection::Outgoing => "  (no calls found)".into(),
            });
        }
        for call in calls {
            let (arrow, other) = match dir {
                CallDirection::Incoming => ("←", &call["from"]),
                CallDirection::Outgoing => ("→", &call["to"]),
            };
            let n = call["fromRanges"].as_array().map_or(0, |a| a.len());
            let times = if n > 1 { format!(" ({n} call sites)") } else { String::new() };
            lines.push(format!("  {arrow} {}{times}", format_call_item(other)));
        }
    }
    lines.join("\n")
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
    dedup_locations(arr).into_iter().map(format_location).collect::<Vec<_>>().join("\n")
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

/// Workspace symbols, each declaration once, and only those the profile wants shown.
pub fn format_workspace_symbols(resp: &Value, profile: &dyn ServerProfile) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No symbols found.".into();
    };
    let Some(arr) = result.as_array() else {
        return "No symbols found.".into();
    };
    if arr.is_empty() {
        return "No symbols found.".into();
    }
    // A server can list the same declaration more than once, e.g. once per source set
    // that contains it. Show each (name, kind, location) once.
    let mut seen = std::collections::HashSet::new();
    arr.iter()
        .filter(|s| profile.keep_workspace_symbol(s))
        .filter(|s| seen.insert((s["name"].to_string(), s["kind"].to_string(), s["location"].to_string())))
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

/// Diagnostics as text.
///
/// The diagnostics in a `textDocument/diagnostic` (or published) answer.
pub fn diagnostic_items(resp: &Value) -> Vec<&Value> {
    let result = &resp["result"];
    let items = result.as_array().or_else(|| result.get("items").and_then(|v| v.as_array()));
    items.map(|a| a.iter().collect()).unwrap_or_default()
}

/// `source` is the text of the file, for profiles that need it to decide what to
/// show. The profile may leave diagnostics out (kotlin-lsp: `FunctionName` warnings
/// on `@Composable` functions, which Compose names in UpperCase on purpose);
/// `show_all` shows everything the server reported.
pub fn format_diagnostics(resp: &Value, source: Option<&str>, show_all: bool, profile: &dyn ServerProfile) -> String {
    let items: Vec<&Value> =
        diagnostic_items(resp).into_iter().filter(|d| show_all || profile.keep_diagnostic(d, source)).collect();
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

pub fn format_signature_help(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No signature help found.".into();
    };
    let Some(sigs) = result.get("signatures").and_then(|v| v.as_array()) else {
        return "No signature help found.".into();
    };
    if sigs.is_empty() {
        return "No signature help found.".into();
    }
    let active_sig = result.get("activeSignature").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let active_param = result.get("activeParameter").and_then(|v| v.as_u64());

    sigs.iter().enumerate().map(|(i, sig)| {
        let label = sig["label"].as_str().unwrap_or("?");
        let marker = if i == active_sig { "▶ " } else { "  " };
        let mut out = format!("{marker}{label}");
        if i == active_sig
            && let Some(params) = sig.get("parameters").and_then(|v| v.as_array()) {
                let idx = active_param
                    .or_else(|| sig.get("activeParameter").and_then(|v| v.as_u64()))
                    .unwrap_or(0) as usize;
                if let Some(param) = params.get(idx) {
                    let plabel = match &param["label"] {
                        Value::String(s) => s.clone(),
                        Value::Array(a) => {
                            // [startChar, endChar] offsets into the signature label
                            let s = a.first().and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            let e = a.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            label.get(s..e).unwrap_or("?").to_owned()
                        }
                        _ => String::new(),
                    };
                    if !plabel.is_empty() {
                        out.push_str(&format!("\n  active param: {plabel}"));
                    }
                }
            }
        if let Some(doc) = sig.get("documentation") {
            let text = doc.get("value").and_then(|v| v.as_str())
                .or_else(|| doc.as_str())
                .unwrap_or("");
            if !text.is_empty() {
                out.push_str(&format!("\n  {text}"));
            }
        }
        out
    }).collect::<Vec<_>>().join("\n")
}

pub fn format_code_actions(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No code actions available.".into();
    };
    let Some(actions) = result.as_array() else {
        return "No code actions available.".into();
    };
    if actions.is_empty() {
        return "No code actions available.".into();
    }
    actions.iter().map(|a| {
        let title = a.get("title").and_then(|v| v.as_str())
            .or_else(|| a.get("command").and_then(|c| c.get("title")).and_then(|v| v.as_str()))
            .unwrap_or("?");
        let kind = a.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let kind_str = if kind.is_empty() { String::new() } else { format!(" [{kind}]") };
        format!("{title}{kind_str}")
    }).collect::<Vec<_>>().join("\n")
}

pub fn format_rename(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No rename result.".into();
    };
    let mut lines = Vec::new();

    // documentChanges (preferred) or changes map
    if let Some(doc_changes) = result.get("documentChanges").and_then(|v| v.as_array()) {
        for change in doc_changes {
            let path = change["textDocument"]["uri"].as_str().map(uri_to_path).unwrap_or("?");
            if let Some(edits) = change.get("edits").and_then(|v| v.as_array()) {
                for edit in edits {
                    let line = edit["range"]["start"]["line"].as_u64().unwrap_or(0) + 1;
                    let col = edit["range"]["start"]["character"].as_u64().unwrap_or(0) + 1;
                    let new_text = edit["newText"].as_str().unwrap_or("");
                    lines.push(format!("{path}:{line}:{col}  →  {new_text}"));
                }
            }
        }
    } else if let Some(changes) = result.get("changes").and_then(|v| v.as_object()) {
        for (uri, edits) in changes {
            let path = uri_to_path(uri);
            if let Some(edits) = edits.as_array() {
                for edit in edits {
                    let line = edit["range"]["start"]["line"].as_u64().unwrap_or(0) + 1;
                    let col = edit["range"]["start"]["character"].as_u64().unwrap_or(0) + 1;
                    let new_text = edit["newText"].as_str().unwrap_or("");
                    lines.push(format!("{path}:{line}:{col}  →  {new_text}"));
                }
            }
        }
    }

    if lines.is_empty() { "No changes.".into() } else { lines.join("\n") }
}

// Fallback legend, used only when the server's initialize response carries
// none (the order kotlin-lsp 262.x declared).  The server's own legend from
// `semanticTokensProvider.legend` takes precedence.
const TOKEN_TYPES: &[&str] = &[
    "namespace", "class", "enum", "interface", "struct", "typeParameter",
    "type", "parameter", "variable", "property", "enumMember", "event",
    "function", "method", "macro", "keyword", "modifier", "comment",
    "string", "number", "regexp", "operator", "decorator",
];
const TOKEN_MODIFIERS: &[&str] = &[
    "declaration", "definition", "readonly", "static", "deprecated",
    "abstract", "async", "modification", "documentation", "defaultLibrary",
];

pub fn format_semantic_tokens(resp: &Value, caps: &Value) -> String {
    let legend = &caps["semanticTokensProvider"]["legend"];
    let from_legend = |key: &str, fallback: &[&str]| -> Vec<String> {
        match legend[key].as_array() {
            Some(a) => a.iter().map(|v| v.as_str().unwrap_or("unknown").to_string()).collect(),
            None => fallback.iter().map(|s| s.to_string()).collect(),
        }
    };
    let token_types = from_legend("tokenTypes", TOKEN_TYPES);
    let token_modifiers = from_legend("tokenModifiers", TOKEN_MODIFIERS);

    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No semantic tokens.".into();
    };
    let Some(data) = result.get("data").and_then(|v| v.as_array()) else {
        return "No semantic tokens.".into();
    };
    if data.is_empty() {
        return "No semantic tokens.".into();
    }

    let nums: Vec<u64> = data.iter().filter_map(|v| v.as_u64()).collect();
    if !nums.len().is_multiple_of(5) {
        return format!("Unexpected token data length: {}", nums.len());
    }

    let mut lines = Vec::new();
    let (mut cur_line, mut cur_start) = (0u64, 0u64);

    for chunk in nums.chunks(5) {
        let (delta_line, delta_start, length, token_type, token_mods) =
            (chunk[0], chunk[1], chunk[2], chunk[3] as usize, chunk[4]);

        cur_line += delta_line;
        cur_start = if delta_line > 0 { delta_start } else { cur_start + delta_start };

        let type_name = token_types.get(token_type).map_or("unknown", |s| s.as_str());
        let mods: Vec<&str> = (0..token_modifiers.len().min(64))
            .filter(|&i| token_mods & (1 << i) != 0)
            .map(|i| token_modifiers[i].as_str())
            .collect();
        let mods_str = if mods.is_empty() { String::new() } else { format!(" ({})", mods.join(", ")) };
        lines.push(format!(
            "line {}:{} len={} — {type_name}{mods_str}",
            cur_line + 1, cur_start + 1, length
        ));
    }
    lines.join("\n")
}

pub fn format_inlay_hints(resp: &Value) -> String {
    let Some(result) = resp.get("result").filter(|v| !v.is_null()) else {
        return "No inlay hints.".into();
    };
    let Some(hints) = result.as_array() else {
        return "No inlay hints.".into();
    };
    if hints.is_empty() {
        return "No inlay hints.".into();
    }
    hints.iter().map(|h| {
        let line = h["position"]["line"].as_u64().unwrap_or(0) + 1;
        let col = h["position"]["character"].as_u64().unwrap_or(0) + 1;
        let label = match &h["label"] {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts.iter()
                .filter_map(|p| p.get("value").and_then(|v| v.as_str()))
                .collect::<Vec<_>>().join(""),
            _ => "?".into(),
        };
        let kind = match h.get("kind").and_then(|v| v.as_u64()) {
            Some(1) => " [type]",
            Some(2) => " [param]",
            _ => "",
        };
        format!("line {line}:{col}{kind}  {label}")
    }).collect::<Vec<_>>().join("\n")
}

fn completion_kind(kind: u64) -> &'static str {
    match kind {
        1 => "Text", 2 => "Method", 3 => "Function", 4 => "Constructor",
        5 => "Field", 6 => "Variable", 7 => "Class", 8 => "Interface",
        9 => "Module", 10 => "Property", 13 => "Enum", 14 => "Keyword",
        15 => "Snippet", 25 => "TypeParam", _ => "",
    }
}

pub fn format_context(
    hover: &Value,
    definition: &Value,
    references: &Value,
    diagnostics: &Value,
    source: Option<&str>,
    profile: &dyn ServerProfile,
) -> String {
    let mut out = String::new();

    // Hover
    let hover_text = match hover.get("result").filter(|v| !v.is_null()) {
        Some(_) => {
            let s = format_hover(hover, profile);
            if s == "No hover information found." { None } else { Some(s) }
        }
        None => None,
    };
    if let Some(text) = hover_text {
        out.push_str("## Hover\n");
        out.push_str(&text);
        out.push('\n');
    }

    // Definition
    let def_text = match definition.get("result").filter(|v| !v.is_null()) {
        Some(_) => {
            let s = format_definition(definition);
            if s == "No definition found." { None } else { Some(s) }
        }
        None => None,
    };
    if let Some(text) = def_text {
        if !out.is_empty() { out.push('\n'); }
        out.push_str("## Definition\n");
        out.push_str(&text);
        out.push('\n');
    }

    // References
    if let Some(result) = references.get("result").filter(|v| !v.is_null())
        && let Some(arr) = result.as_array().filter(|a| !a.is_empty()) {
            let deduped = dedup_locations(arr);
            let total = deduped.len();
            let cap = total.min(12);
            if !out.is_empty() { out.push('\n'); }
            out.push_str(&format!("## References ({})\n", total));
            for loc in &deduped[..cap] {
                out.push_str(&format_location(loc));
                out.push('\n');
            }
            if total > 12 {
                out.push_str(&format!("... and {} more\n", total - 12));
            }
        }

    // Diagnostics
    let diag_text = match diagnostics.get("result").filter(|v| !v.is_null()) {
        Some(_) => {
            let s = format_diagnostics(diagnostics, source, false, profile);
            if s == "No diagnostics." { None } else { Some(s) }
        }
        None => None,
    };
    if let Some(text) = diag_text {
        if !out.is_empty() { out.push('\n'); }
        out.push_str("## Diagnostics\n");
        out.push_str(&text);
        out.push('\n');
    }

    if out.is_empty() {
        out.push_str("No information available.\n");
    }
    out.push_str("---\n");
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers;
    use serde_json::json;

    #[test]
    fn command_links_become_their_text_and_normal_links_stay() {
        let md = "[run](command:foo?x) and [docs](https://example.com)";
        assert_eq!(strip_command_links(md), "run and [docs](https://example.com)");
        // Not a link at all, or an unfinished one: left as it is.
        assert_eq!(strip_command_links("a ](command:x"), "a ](command:x");
    }

    fn hover(md: &str) -> Value {
        json!({"result": {"contents": {"kind": "markdown", "value": md}}})
    }

    #[test]
    fn hover_links_follow_the_profile_then_the_generic_clean_up() {
        let md = "From [`a.B`](command:jetbrains.navigateToLocation?%5B%22file%3A%2F%2F%2Fx%2FB.java%22%2C5%2C19%5D), [run](command:foo)";
        // kotlin-lsp: its place links become a place; the other link becomes its text.
        let kotlin = servers::by_name("kotlin-lsp").unwrap();
        assert_eq!(format_hover(&hover(md), &*kotlin), "From `a.B` (/x/B.java:6:20), run");
        // Another server: every command link is just its text.
        assert_eq!(format_hover(&hover(md), &*servers::generic()), "From `a.B`, run");
    }

    fn symbol(uri: &str) -> Value {
        json!({"name": "f", "kind": 12, "containerName": "p",
            "location": {"uri": uri, "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 5}}}})
    }

    #[test]
    fn workspace_symbols_are_shown_once_and_the_profile_can_drop_some() {
        let own_jar = "jar:///p/app/build/intermediates/x/classes.jar!/AKt.class";
        let resp = json!({"result": [symbol("file:///a.kt"), symbol("file:///a.kt"), symbol(own_jar)]});
        // Identical entries collapse; a profile that doesn't know the jar keeps it.
        assert_eq!(format_workspace_symbols(&resp, &*servers::generic()).lines().count(), 2);
        // kotlin-lsp's profile drops the project's own compiled jar.
        let out = format_workspace_symbols(&resp, &*servers::by_name("kotlin-lsp").unwrap());
        assert_eq!(out.lines().count(), 1, "{out}");
    }

    fn function_name_warning() -> Value {
        json!({"code": "FunctionName", "severity": 2,
               "message": "Function name 'Foo' should start with a lowercase letter",
               "range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 7}}})
    }

    fn diagnostics(items: Vec<Value>) -> Value {
        json!({"result": {"items": items}})
    }

    #[test]
    fn diagnostics_go_through_the_profile_and_all_shows_everything() {
        let src = "@Composable\nfun Foo() {}\n";
        let resp = diagnostics(vec![function_name_warning()]);
        let kotlin = servers::by_name("kotlin-lsp").unwrap();
        assert_eq!(format_diagnostics(&resp, Some(src), false, &*kotlin), "No diagnostics.");
        assert!(format_diagnostics(&resp, Some(src), true, &*kotlin).contains("Foo"));
        // Another server's diagnostics are shown whatever the source says.
        assert!(format_diagnostics(&resp, Some(src), false, &*servers::generic()).contains("Foo"));
    }

    #[test]
    fn a_kept_diagnostic_next_to_a_hidden_one_is_still_listed() {
        let src = "@Composable\nfun Foo() {}\n";
        let other = json!({"code": "SomethingElse", "severity": 2, "message": "unused",
                           "range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 7}}});
        let resp = diagnostics(vec![function_name_warning(), other]);
        let out = format_diagnostics(&resp, Some(src), false, &*servers::by_name("kotlin-lsp").unwrap());
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("unused"));
    }
}
