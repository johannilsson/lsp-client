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
  query         Run an LSP query against a file or workspace
  session       Manage the persistent LSP session daemon
  capabilities  List capabilities reported by the server
```

### Query subcommands

```
lsp-client query hover             <file> <line> <col>
lsp-client query definition        <file> <line> <col>
lsp-client query references        <file> <line> <col>
lsp-client query symbols           <file>
lsp-client query workspace-symbols <query>
lsp-client query diagnostics       <file>
lsp-client query completion        <file> <line> <col>
lsp-client query signature-help    <file> <line> <col>
lsp-client query code-action       <file> <line> <col>
lsp-client query rename            <file> <line> <col> <new-name>
lsp-client query semantic-tokens   <file>
lsp-client query inlay-hints       <file> [--start-line N] [--end-line N]
```

### Session subcommands

```
lsp-client session start      Start a persistent daemon for this workspace
lsp-client session status     Show daemon status (PID, socket, uptime)
lsp-client session stop       Stop the daemon
lsp-client session wait-ready Wait until the server has finished indexing
```

Line and column numbers are 1-based.

## Daemon workflow (recommended)

Start a daemon once per workspace. Subsequent calls auto-connect.

```sh
# Start daemon
lsp-client session start --server <binary> --language-id <id> [--root /path/to/project]

# Query (auto-connects to daemon; --root defaults to cwd)
lsp-client query symbols src/Foo.kt
lsp-client query hover src/Foo.kt 42 10

# Check status
lsp-client session status

# Stop
lsp-client session stop
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

### kotlin-lsp (Kotlin)

```sh
brew install JetBrains/utils/kotlin-lsp

# Recommended: daemon over stdio (starts once, serves all subsequent calls from a warm session)
lsp-client session start --server kotlin-lsp --language-id kotlin --root /my/project
lsp-client query symbols src/Main.kt

# Direct TCP: attach to a server already running on port 9999 (e.g. started by IntelliJ)
lsp-client --tcp --language-id kotlin --root /my/project query symbols src/Main.kt

# Direct TCP with auto-start: spawn kotlin-lsp over TCP if none is listening
lsp-client --tcp --server kotlin-lsp --language-id kotlin --root /my/project query symbols src/Main.kt
```

### sourcekit-lsp (Swift)

sourcekit-lsp communicates over stdio and starts fresh per invocation, so the daemon is strongly recommended to avoid paying the startup cost on every call.

```sh
# sourcekit-lsp uses stdio by default — don't pass --stdio to the process
lsp-client session start --server sourcekit-lsp --no-server-stdio-flag --language-id swift --root /my/project
lsp-client query symbols Sources/App.swift
lsp-client --timeout 15s query hover Sources/App.swift 10 5
```

### rust-analyzer

```sh
lsp-client session start --server rust-analyzer --no-server-stdio-flag --language-id rust --root /my/project
lsp-client query diagnostics src/main.rs
```

## Testing

The integration tests run against a small fixture Rust project using rust-analyzer. Install rust-analyzer first if needed:

```sh
rustup component add rust-analyzer
```

Then run the tests serially (the daemon handles one connection at a time):

```sh
cargo test --test integration -- --test-threads=1
```

The first run starts a daemon for the fixture project and warms it up; subsequent runs reuse it. The daemon shuts itself down after 5 minutes of inactivity.

## How it works

Query commands auto-detect a running daemon for the project root. If a daemon is running, the client connects to it over a Unix socket — no flags needed. The daemon starts the language server once over stdio, pays the startup and indexing cost once, then serves all subsequent calls from the warm session.

Pass `--tcp` to bypass the daemon entirely and connect directly to a TCP language server (e.g. kotlin-lsp on port 9999). This is useful when a server is already running — for example, started by an IDE — and you want to share it. Add `--server <binary>` to auto-start the server if none is listening.

## Known limitations

**Android projects:** kotlin-lsp currently only supports JVM-only Kotlin Gradle projects out of the box. `workspace-symbols` returns empty results for Android projects. File-level commands (`symbols`, `hover`, `diagnostics`, etc.) work fine. Tracking issues: [#26](https://github.com/Kotlin/kotlin-lsp/issues/26), [#88](https://github.com/Kotlin/kotlin-lsp/issues/88).

## Alternatives

- [valentjn/lsp-cli](https://github.com/valentjn/lsp-cli) — Java/Kotlin, broad LSP command coverage
- [lsp-client/lsp-cli](https://github.com/lsp-client/lsp-client) — Python, designed for AI agents
- [eli0shin/cli-lsp-client](https://github.com/eli0shin/cli-lsp-client) — TypeScript, daemon-based, multi-language
