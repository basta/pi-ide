#!/usr/bin/env python3
"""A tiny language server for pide-lsp's tests: answers initialize (UTF-16),
publishes one diagnostic per opened document, answers hover with the word
"fake", and exits with code 3 on the custom notification `test/crash`."""
import json
import sys


def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            if length is not None:
                break
            continue
        name, _, value = line.decode().partition(":")
        if name.lower() == "content-length":
            length = int(value.strip())
    return json.loads(sys.stdin.buffer.read(length))


def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


while True:
    msg = read()
    if msg is None:
        break
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "capabilities": {"positionEncoding": "utf-16", "hoverProvider": True, "textDocumentSync": 1}}})
    elif method == "textDocument/didOpen":
        uri = msg["params"]["textDocument"]["uri"]
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": [
            {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
             "severity": 1, "message": "fake error"}]}})
    elif method == "textDocument/hover":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"contents": "fake"}})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    elif method == "exit":
        break
    elif method == "test/crash":
        sys.stderr.write("fake server crashing on purpose\n")
        sys.exit(3)
