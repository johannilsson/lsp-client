# lsp-client

A minimal CLI for querying a Language Server Protocol (LSP) server. Designed to be called by AI applications that need language intelligence (hover, definitions, references, diagnostics, completions) without bundling their own LSP client.

Defaults to Kotlin via [kotlin-lsp](https://github.com/Kotlin/kotlin-lsp). TCP auto-start is kotlin-lsp specific; for other servers use `--stdio` or connect to a manually started server.

## Install

```sh
brew install JetBrains/utils/kotlin-lsp
cargo install --path .
```

## Usage

```sh
lsp-client [OPTIONS] <COMMAND>

Commands:
  hover              <file> <line> <col>
  definition         <file> <line> <col>
  references         <file> <line> <col>
  symbols            <file>
  workspace-symbols  <query>
  diagnostics        <file>
  completion         <file> <line> <col>
  signature-help     <file> <line> <col>
  code-action        <file> <line> <col>
  rename             <file> <line> <col> <new-name>
  semantic-tokens    <file>
  inlay-hints        <file> [--start-line N] [--end-line N]
```

Line and column numbers are 1-based.

```sh
# Human-readable output
lsp-client --root /my/project hover src/Main.kt 42 10

# JSON output (for programmatic use)
lsp-client --root /my/project --json diagnostics src/Main.kt

# stdio transport (spawns server as child process)
lsp-client --stdio hover src/Main.kt 42 10
```

## Options

| Flag | Default | Description |
|---|---|---|
| `--root` | cwd | Project root directory |
| `--host` | `127.0.0.1` | Server host (TCP mode) |
| `--port` | `9999` | Server port (TCP mode) |
| `--server` | `kotlin-lsp` | Server binary to launch |
| `--language-id` | `kotlin` | Language ID for `didOpen` |
| `--stdio` | — | Spawn server over stdio instead of TCP |
| `--json` | — | Emit `{"ok": true, "result": ...}` JSON |
| `--verbose` / `-v` | — | Debug logging to stderr |

## How it works

By default the client connects to a running `kotlin-lsp` server over TCP (`--multi-client` mode). If no server is running it starts one automatically and waits for it to be ready. The warm server is then reused across subsequent calls, keeping latency low.

Use `--stdio` to skip the shared server and spawn a fresh process per call — simpler but slower due to JVM startup time.

## Alternatives

- [valentjn/lsp-cli](https://github.com/valentjn/lsp-cli) — Java/Kotlin, broad LSP command coverage
- [lsp-client/lsp-cli](https://github.com/lsp-client/lsp-client) — Python, designed for AI agents
- [eli0shin/cli-lsp-client](https://github.com/eli0shin/cli-lsp-client) — TypeScript, daemon-based, multi-language
