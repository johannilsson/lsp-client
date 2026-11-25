#!/usr/bin/env python3
"""A minimal language server over stdio, for testing how lsp-client treats servers
that are not rust-analyzer or kotlin-lsp.

It answers `initialize` and hover, and after `initialized` it opens a progress
token ("Indexing") that it never ends, which some real servers do.

Environment variables change what it does:
- FAKE_LSP_SERVER_NAME: send this as `serverInfo.name` in the `initialize` answer.
- FAKE_LSP_LOG_FILE: announce this as its log file ("Log file: <path>") in a
  `window/logMessage` sent before the `initialize` answer, as kotlin-lsp does.
- FAKE_LSP_INTELLIJ=1: also send kotlin-lsp's `intellij/workspaceImport*`
  notifications, reporting a failed import, as kotlin-lsp would.
- FAKE_LSP_STDERR: write this line to stderr at startup.
- FAKE_LSP_HOVER_DELAY: wait this many seconds before answering a hover, to
  stand for a slow query. The answer names the hovered line, so that parallel
  callers can each check they got their own.
- FAKE_LSP_INIT_DELAY: wait this many seconds before answering `initialize`,
  as a server with a slow first import does.
"""
import json
import os
import sys
import time


def read():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        line = line.decode().strip()
        if not line:
            break
        key, value = line.split(":", 1)
        headers[key.lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))


def send(message):
    body = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


if os.environ.get("FAKE_LSP_STDERR"):
    sys.stderr.write(os.environ["FAKE_LSP_STDERR"] + "\n")
    sys.stderr.flush()

while True:
    m = read()
    method = m.get("method")
    if method == "initialize":
        time.sleep(float(os.environ.get("FAKE_LSP_INIT_DELAY", "0")))
        if os.environ.get("FAKE_LSP_LOG_FILE"):
            send({"jsonrpc": "2.0", "method": "window/logMessage",
                  "params": {"type": 3, "message": "Log file: " + os.environ["FAKE_LSP_LOG_FILE"]}})
        result = {"capabilities": {"hoverProvider": True}}
        if os.environ.get("FAKE_LSP_SERVER_NAME"):
            result["serverInfo"] = {"name": os.environ["FAKE_LSP_SERVER_NAME"], "version": "1"}
        send({"jsonrpc": "2.0", "id": m["id"], "result": result})
    elif method == "initialized":
        if os.environ.get("FAKE_LSP_INTELLIJ"):
            send({"jsonrpc": "2.0", "method": "intellij/workspaceImportStatus", "params": {"blockedFolders": []}})
            send({"jsonrpc": "2.0", "method": "intellij/workspaceImportState", "params": {
                "phase": "FINISHED",
                "folders": [{"folderUri": "file:///fake/", "message": "Gradle sync failed", "status": "FAILED"}],
            }})
        send({"jsonrpc": "2.0", "id": 900, "method": "window/workDoneProgress/create", "params": {"token": "t1"}})
        send({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "t1", "value": {"kind": "begin", "title": "Indexing"}}})
    elif method == "textDocument/hover":
        time.sleep(float(os.environ.get("FAKE_LSP_HOVER_DELAY", "0")))
        line = m["params"]["position"]["line"] + 1
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"contents": {"kind": "markdown",
              "value": "hover from the fake server at line %d" % line}}})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": m["id"], "result": None})
    elif method == "exit":
        sys.exit(0)
