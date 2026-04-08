#!/usr/bin/env python3
"""
Nozdormu CDN — Test Backend Server

A simple HTTP server with various endpoints for E2E testing.
Each instance runs on a configurable port and identifies itself
via the X-Backend-Port response header.

Usage:
    python3 server.py 8081
    python3 server.py 8082
    python3 server.py 8083
"""

import json
import os
import sys
import time
import threading
from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse, parse_qs


class TestHandler(BaseHTTPRequestHandler):
    """HTTP request handler with test endpoints."""

    # Suppress default access logging
    def log_message(self, format, *args):
        pass

    def _send(self, status, body, content_type="text/plain", extra_headers=None):
        """Send a response with standard headers."""
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("X-Backend-Port", str(self.server.server_port))
        self.send_header("X-Backend-Id", f"backend-{self.server.server_port}")
        if extra_headers:
            for k, v in extra_headers.items():
                self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parsed = urlparse(self.path)
        path = parsed.path
        query = parse_qs(parsed.query)

        if path == "/":
            self._send(200, f"Hello from port {self.server.server_port}\n")

        elif path == "/health":
            self._send(200, "OK\n")

        elif path == "/json":
            data = {
                "server": f"backend-{self.server.server_port}",
                "port": self.server.server_port,
                "path": self.path,
                "method": "GET",
                "timestamp": time.time(),
            }
            self._send(200, json.dumps(data, indent=2), "application/json")

        elif path == "/large":
            # ~100KB of text (good for compression testing)
            line = "The quick brown fox jumps over the lazy dog. " * 10 + "\n"
            body = line * 200  # ~100KB
            self._send(200, body, "text/plain")

        elif path == "/small":
            # 50 bytes — below typical min_size (256)
            self._send(200, "x" * 50, "text/plain")

        elif path == "/html":
            html = """<!DOCTYPE html>
<html><head><title>Test Page</title></head>
<body><h1>Hello from backend {port}</h1>
<p>This is a test HTML page for compression and caching tests.</p>
{filler}
</body></html>""".format(
                port=self.server.server_port,
                filler="<p>Lorem ipsum dolor sit amet. </p>\n" * 100,
            )
            self._send(200, html, "text/html; charset=utf-8")

        elif path == "/css":
            css = "body { margin: 0; padding: 0; }\n" * 200
            self._send(200, css, "text/css")

        elif path == "/js":
            js = 'console.log("hello");\n' * 200
            self._send(200, js, "application/javascript")

        elif path == "/binary":
            # Fake PNG header + random bytes (not compressible)
            body = b"\x89PNG\r\n\x1a\n" + os.urandom(1024)
            self._send(200, body, "image/png")

        elif path == "/echo-headers":
            headers = {}
            for key, value in self.headers.items():
                headers[key] = value
            self._send(200, json.dumps(headers, indent=2), "application/json")

        elif path == "/slow":
            delay = int(query.get("delay", ["5"])[0])
            time.sleep(delay)
            self._send(200, f"Responded after {delay}s\n")

        elif path == "/status":
            code = int(query.get("code", ["200"])[0])
            self._send(code, f"Status {code}\n")

        elif path == "/sse":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "keep-alive")
            self.send_header("X-Backend-Port", str(self.server.server_port))
            self.end_headers()
            try:
                for i in range(5):
                    self.wfile.write(f"data: event {i}\n\n".encode())
                    self.wfile.flush()
                    time.sleep(0.5)
                self.wfile.write(b"data: done\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass

        elif path == "/cache-test":
            # Returns a unique body each time (to verify cache HIT vs MISS)
            body = json.dumps({
                "time": time.time(),
                "port": self.server.server_port,
                "random": os.urandom(8).hex(),
            })
            self._send(
                200, body, "application/json",
                extra_headers={"Cache-Control": "max-age=60"},
            )

        elif path == "/no-cache":
            body = json.dumps({"time": time.time()})
            self._send(
                200, body, "application/json",
                extra_headers={"Cache-Control": "no-store"},
            )

        elif path.startswith("/static/"):
            # Simulate static file serving
            ext = path.rsplit(".", 1)[-1] if "." in path else "txt"
            ct_map = {
                "js": "application/javascript",
                "css": "text/css",
                "html": "text/html",
                "json": "application/json",
                "png": "image/png",
                "jpg": "image/jpeg",
                "svg": "image/svg+xml",
                "wasm": "application/wasm",
                "txt": "text/plain",
            }
            ct = ct_map.get(ext, "application/octet-stream")
            body = f"Static content for {path} (port {self.server.server_port})\n" * 50
            self._send(
                200, body, ct,
                extra_headers={"Cache-Control": "max-age=86400"},
            )

        elif path == "/api/login":
            self._send(200, '{"status":"ok"}\n', "application/json")

        elif path == "/api/data":
            self._send(200, '{"data":"value"}\n', "application/json")

        elif path.startswith("/api/"):
            self._send(200, '{"api":"response"}\n', "application/json")

        elif path == "/204":
            self.send_response(204)
            self.send_header("X-Backend-Port", str(self.server.server_port))
            self.end_headers()

        elif path == "/304":
            self.send_response(304)
            self.send_header("X-Backend-Port", str(self.server.server_port))
            self.end_headers()

        else:
            self._send(200, f"Catch-all: {self.path}\n")

    def do_POST(self):
        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length) if content_length > 0 else b""
        data = {
            "method": "POST",
            "path": self.path,
            "body_size": len(body),
            "port": self.server.server_port,
        }
        self._send(200, json.dumps(data, indent=2), "application/json")

    def do_HEAD(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("X-Backend-Port", str(self.server.server_port))
        self.end_headers()


def run_server(port):
    server = HTTPServer(("0.0.0.0", port), TestHandler)
    print(f"[Backend] Listening on 0.0.0.0:{port}")
    server.serve_forever()


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python3 server.py PORT [PORT2 PORT3 ...]")
        sys.exit(1)

    ports = [int(p) for p in sys.argv[1:]]

    if len(ports) == 1:
        run_server(ports[0])
    else:
        threads = []
        for port in ports:
            t = threading.Thread(target=run_server, args=(port,), daemon=True)
            t.start()
            threads.append(t)
        print(f"[Backend] Started {len(ports)} servers on ports: {ports}")
        try:
            while True:
                time.sleep(3600)
        except KeyboardInterrupt:
            print("\n[Backend] Shutting down")
