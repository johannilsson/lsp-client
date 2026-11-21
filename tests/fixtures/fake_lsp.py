#!/usr/bin/env python3
"""A minimal language server over stdio, for testing how lsp-client treats servers
that are not rust-analyzer or kotlin-lsp.

It answers `initialize` and hover, and after `initialized` it opens a progress
token ("Indexing") that it never ends, which some real servers do.
"""
import json
import sys


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


while True:
    m = read()
    method = m.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"capabilities": {"hoverProvider": True}}})
    elif method == "initialized":
        send({"jsonrpc": "2.0", "id": 900, "method": "window/workDoneProgress/create", "params": {"token": "t1"}})
        send({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "t1", "value": {"kind": "begin", "title": "Indexing"}}})
    elif method == "textDocument/hover":
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"contents": {"kind": "markdown", "value": "hover from the fake server"}}})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": m["id"], "result": None})
    elif method == "exit":
        sys.exit(0)
