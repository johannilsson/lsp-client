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

## Configuration file

Instead of passing `--server`, `--language-id`, and other flags on every `session start`, you can place a `.lsp-client.toml` in your project root:

```toml
server      = "/path/to/kotlin-language-server"
language_id = "kotlin"
root        = "/path/to/your/project"   # defaults to cwd if omitted
timeout     = "30s"
no_server_stdio_flag = false
```

All fields are optional. `lsp-client` walks up from the current directory to find the nearest `.lsp-client.toml`, so placing it at the repo root covers all subdirectories.

With the file in place, starting a session is just:

```sh
lsp-client session start
```

## Server-specific setup

### kotlin-lsp (Kotlin)

```sh
# macOS
brew install JetBrains/utils/kotlin-lsp
# Linux/Windows: download the standalone archive from
# https://github.com/Kotlin/kotlin-lsp/releases, unpack it and put bin/intellij-server
# on PATH as kotlin-lsp

# Recommended: daemon over stdio (starts once, serves all subsequent calls from a warm session).
# The first Gradle import of a project can take a minute or more: give --timeout room.
lsp-client --timeout 10m session start --server kotlin-lsp --language-id kotlin --root /my/project --wait
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

**Android projects (kotlin-lsp):** Android Gradle Plugin support in kotlin-lsp is experimental. Tested with kotlin-lsp 271.2085.0 on an AGP 9.5 / Gradle 9.8 app that uses a JVM-only `includeBuild`: once the Gradle sync succeeds, hover, definition, references, workspace symbols and diagnostics work, including across the included build and into library jars. Things to watch for:

- **The whole sync can fail on a variant you don't use.** kotlin-lsp syncs every variant, including `release` ([#291](https://github.com/Kotlin/kotlin-lsp/issues/291)). With AGP 9's Compose Preview support, the `release` variant fails because AGP disables its unit tests by default. The project then gets no model at all: only `symbols` works, and `diagnostics` comes back silently empty. Look for `Gradle sync failed` in the server log (its path is printed at startup with `--verbose`). One workaround is to turn on host tests for `release` in `app/build.gradle.kts`:

  ```kotlin
  androidComponents {
      beforeVariants(selector().withBuildType("release")) {
          (it as com.android.build.api.variant.HasHostTestsBuilder)
              .hostTests[com.android.build.api.variant.HostTestBuilder.UNIT_TEST_TYPE]?.enable = true
      }
  }
  ```

- **`BuildConfig` (and other generated sources) are not provided by the server** ([#253](https://github.com/Kotlin/kotlin-lsp/issues/253)). On a fresh clone they show up as `Unresolved reference` errors. Run `./gradlew :app:generateDebugBuildConfig` (or a build) *before* `session start`; files generated while the server runs are not picked up until a restart.
- Use AGP 9 / Gradle 9: older combinations can import with empty source roots ([#241](https://github.com/Kotlin/kotlin-lsp/issues/241)).
- Kotlin naming inspections flag every `@Composable` function name as a warning.

## Alternatives

- [valentjn/lsp-cli](https://github.com/valentjn/lsp-cli) — Java/Kotlin, broad LSP command coverage
- [lsp-client/lsp-cli](https://github.com/lsp-client/lsp-client) — Python, designed for AI agents
- [eli0shin/cli-lsp-client](https://github.com/eli0shin/cli-lsp-client) — TypeScript, daemon-based, multi-language
