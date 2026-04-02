# lsp-client

A minimal CLI for querying a Language Server Protocol (LSP) server. Designed to be called by AI applications that need language intelligence (hover, definitions, references, diagnostics, completions) without bundling their own LSP client.

## Install

```sh
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
  start              Start a persistent daemon for this workspace
  status             Show daemon status (PID, socket, uptime)
  stop               Stop the daemon
```

Line and column numbers are 1-based.

## Daemon workflow (recommended)

Start a daemon once per workspace. Subsequent calls auto-connect.

```sh
# Start daemon
lsp-client start --server <binary> --language-id <id> [--root /path/to/project]

# Query (auto-connects to daemon; --root defaults to cwd)
lsp-client symbols src/Foo.kt
lsp-client hover src/Foo.kt 42 10

# Check status
lsp-client status

# Stop
lsp-client stop
```

## Options

| Flag | Default | Description |
|---|---|---|
| `--root` | cwd | Project root directory |
| `--server` | — | Server binary to launch (required without a running daemon) |
| `--language-id` | — | Language ID for `didOpen` (required without a running daemon) |
| `--timeout` | — | Max time to wait for a response (e.g. `10s`, `2m`) |
| `--host` | `127.0.0.1` | Server host (TCP mode) |
| `--port` | `9999` | Server port (TCP mode) |
| `--no-server-stdio-flag` | — | Don't pass `--stdio` to the server process |
| `--json` | — | Emit `{"ok": true, "result": ...}` JSON |
| `--verbose` / `-v` | — | Debug logging to stderr |

## Server-specific setup

### kotlin-lsp (Kotlin, TCP mode)

kotlin-lsp runs as a persistent TCP server, so daemon mode is optional — it auto-starts on the first call.

```sh
brew install JetBrains/utils/kotlin-lsp

# One-shot (auto-starts server on first call)
lsp-client --server kotlin-lsp --language-id kotlin --root /my/project symbols src/Main.kt

# Or with daemon
lsp-client start --server kotlin-lsp --language-id kotlin --root /my/project
lsp-client symbols src/Main.kt
```

### sourcekit-lsp (Swift)

sourcekit-lsp communicates over stdio and starts fresh per invocation, so the daemon is strongly recommended to avoid paying the startup cost on every call.

```sh
# sourcekit-lsp uses stdio by default — don't pass --stdio to the process
lsp-client start --server sourcekit-lsp --no-server-stdio-flag --language-id swift --root /my/project
lsp-client symbols Sources/App.swift
lsp-client --timeout 15s hover Sources/App.swift 10 5
```

### rust-analyzer

```sh
lsp-client start --server rust-analyzer --no-server-stdio-flag --language-id rust --root /my/project
lsp-client diagnostics src/main.rs
```

## How it works

Query commands auto-detect a running daemon for the project root. If a daemon is running, the client connects to it over a Unix socket — no flags needed. If no daemon is found, the client falls back to TCP (requires `--server`).

The daemon pays the LSP server startup and indexing cost once, then serves all subsequent calls from the warm session.

## Known limitations

**Android projects:** kotlin-lsp currently only supports JVM-only Kotlin Gradle projects out of the box. `workspace-symbols` returns empty results for Android projects. File-level commands (`symbols`, `hover`, `diagnostics`, etc.) work fine. Tracking issues: [#26](https://github.com/Kotlin/kotlin-lsp/issues/26), [#88](https://github.com/Kotlin/kotlin-lsp/issues/88).

## Alternatives

- [valentjn/lsp-cli](https://github.com/valentjn/lsp-cli) — Java/Kotlin, broad LSP command coverage
- [lsp-client/lsp-cli](https://github.com/lsp-client/lsp-client) — Python, designed for AI agents
- [eli0shin/cli-lsp-client](https://github.com/eli0shin/cli-lsp-client) — TypeScript, daemon-based, multi-language
