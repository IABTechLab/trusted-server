#!/usr/bin/env python3
"""Accept exact final core Wasm only after maintained validation and Viceroy.

Runtime starts final bytes directly, performs publisher-origin/injection/script
requests and hashes before/after. It never invokes Cargo, Fastly or a Wasm rewrite.
"""
import argparse
import http.client
import http.server
import json
import logging
import os
import re
import signal
import socket
import subprocess
import sys
import threading
import time
import tomllib
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import deterministic_json_bytes, sha256_bytes
from prepare import _regular_bytes

MAX_EXECUTABLE = 536870912
MAX_RESPONSE = 33554432
MAX_REQUEST_SECONDS = 5
MAX_RUNTIME_LOG = 16777216
ORIGIN_MARKER = b"release-origin-example"
HTML = b"<!doctype html><html><head><title>Release fixture</title></head><body>release-origin-example</body></html>"


def _leb(data, offset):
    result = 0
    for index in range(5):
        if offset >= len(data):
            raise ValueError("truncated Wasm integer")
        byte = data[offset]
        offset += 1
        result |= (byte & 127) << (index * 7)
        if byte < 128:
            if result > 0xffffffff:
                raise ValueError("Wasm integer overflow")
            return result, offset
    raise ValueError("Wasm integer bound exceeded")


def _name(data, offset):
    size, offset = _leb(data, offset)
    if size > 4096 or offset + size > len(data):
        raise ValueError("Wasm name bound exceeded")
    try:
        return data[offset:offset + size].decode("utf-8"), offset + size
    except UnicodeError as error:
        raise ValueError("invalid Wasm name") from error


def core_wasm(data):
    """Require core module framing and default Fastly/WASI function ABI.

    This bounded framing check supplements the selected maintained Wasmtime
    validator; it deliberately does not claim to validate instruction semantics.
    """
    if data[:8] != b"\0asm\x01\0\0\0":
        raise ValueError("final core Wasm format required; components forbidden")
    offset, imported, exports, seen = 8, [], [], set()
    while offset < len(data):
        identifier = data[offset]
        size, start = _leb(data, offset + 1)
        offset = start + size
        if offset > len(data) or identifier > 12 or (identifier and identifier in seen):
            raise ValueError("invalid or repeated Wasm section")
        seen.add(identifier)
        body = data[start:offset]
        if identifier not in {2, 7}:
            continue
        count, cursor = _leb(body, 0)
        if count > 10000:
            raise ValueError("Wasm ABI member bound exceeded")
        for _ in range(count):
            module, cursor = _name(body, cursor)
            if identifier == 2:
                name, cursor = _name(body, cursor)
                if cursor >= len(body) or body[cursor] != 0:
                    raise ValueError("Fastly ABI imports must be functions")
                _, cursor = _leb(body, cursor + 1)
                if module != "wasi_snapshot_preview1" and not re.fullmatch(r"fastly_[a-z_]+", module):
                    raise ValueError("unselected Fastly ABI import module")
                imported.append((module, name))
            else:
                if cursor >= len(body):
                    raise ValueError("truncated Wasm export ABI")
                kind = body[cursor]
                _, cursor = _leb(body, cursor + 1)
                exports.append((module, kind))
        if cursor != len(body):
            raise ValueError("Wasm ABI section trailing bytes")
    if (("_start", 0) not in exports or not any(module.startswith("fastly_") for module, _ in imported)
            or not any(module == "wasi_snapshot_preview1" for module, _ in imported)):
        raise ValueError("default Fastly WASI request ABI required")
    return {"format": "core-wasm", "abi": "wasip1-fastly-default", "imports": imported, "exports": exports}


def publisher_response(status, body, headers):
    """Require origin marker, complete HTML and injected main script path."""
    if status != 200 or ORIGIN_MARKER not in body or "text/html" not in headers.get("content-type", ""):
        raise ValueError("positive publisher-origin fixture failed")
    if b"</html>" not in body.lower() or b"</body>" not in body.lower():
        raise ValueError("publisher response must retain complete HTML")
    tags = re.findall(rb"<script\b[^>]*>", body, re.I)
    selected = [tag for tag in tags if re.search(rb'\bid=["\']trustedserver-js["\']', tag)]
    if len(selected) != 1:
        raise ValueError("publisher main script injection missing or repeated")
    match = re.search(rb'\bsrc=["\']([^"\']+)["\']', selected[0])
    if match is None:
        raise ValueError("publisher script injection src missing")
    path = match[1].decode("ascii")
    if not re.fullmatch(r"/static/tsjs=tsjs-unified\.min\.js\?v=[a-f0-9]{64}", path):
        raise ValueError("publisher script injection path mismatch")
    injected = re.search(re.escape(selected[0]) + rb"\s*</script\s*>", body, re.I)
    if injected is None or body[:injected.start()] + body[injected.end():] != HTML:
        raise ValueError("publisher response must preserve complete HTML origin fixture")
    return path


def _port():
    with socket.socket() as selected:
        selected.bind(("127.0.0.1", 0))
        return selected.getsockname()[1]


def _request(port, path, *, deadline=None, logs=None):
    """Bound wall time as well as response bytes, including slow headers/body."""
    deadline = min(deadline if deadline is not None else time.monotonic() + MAX_REQUEST_SECONDS,
                   time.monotonic() + MAX_REQUEST_SECONDS)
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
    reply, active_socket = [], []
    def read():
        try:
            connection.connect()
            active_socket.append(connection.sock)
            connection.request("GET", path, headers={"Host": "publisher.example.com"})
            response = connection.getresponse()
            body = bytearray()
            while True:
                chunk = response.read1(65536)
                if not chunk:
                    break
                body.extend(chunk)
                if len(body) > MAX_RESPONSE:
                    raise ValueError("runtime response bound exceeded")
            reply.append((response.status, bytes(body), {key.lower(): value for key, value in response.getheaders()}))
        except Exception as error:
            reply.append(error)
        finally:
            connection.close()
    worker = threading.Thread(target=read, daemon=True)
    worker.start()
    try:
        while worker.is_alive():
            if logs is not None:
                logs.check()
            if time.monotonic() >= deadline:
                raise ValueError("runtime request wall-time bound exceeded")
            worker.join(timeout=min(0.05, deadline - time.monotonic()))
        if not reply:
            raise ValueError("runtime request did not produce an outcome")
        if isinstance(reply[0], Exception):
            raise reply[0]
        return reply[0]
    finally:
        for selected in active_socket:
            try:
                selected.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            selected.close()
        connection.close()
        worker.join(timeout=1)


class RuntimeLogs:
    """Drain bounded live runtime logs through parent-owned descriptors."""

    def __init__(self, process, prefix, *, maximum=MAX_RUNTIME_LOG):
        self.process, self.maximum = process, maximum
        self.failure = threading.Event()
        self.files, self.threads = [], []
        try:
            for name in ("stdout", "stderr"):
                output = Path(str(prefix) + "." + name).open("xb")
                self.files.append(output)
                thread = threading.Thread(target=self._drain, args=(getattr(process, name), output), daemon=True)
                self.threads.append(thread)
                thread.start()
        except Exception:
            self.close()
            raise

    def _drain(self, stream, output):
        total = 0
        try:
            while chunk := os.read(stream.fileno(), 65536):
                remaining = self.maximum - total
                output.write(chunk[:remaining])
                output.flush()
                total += len(chunk)
                if total > self.maximum:
                    self.failure.set()
                    return
        except (OSError, ValueError):
            self.failure.set()

    def check(self):
        if self.failure.is_set():
            raise ValueError("runtime log bound/drain failed")

    def close(self):
        try:
            os.killpg(self.process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.process.wait(timeout=5)
        for thread in self.threads:
            thread.join(timeout=5)
            if thread.is_alive():
                raise ValueError("runtime log drain completion bound exceeded")
        for stream in (self.process.stdout, self.process.stderr):
            if stream is not None:
                stream.close()
        for output in self.files:
            output.close()


def expected_javascript(source, js_dir):
    """Derive exact core/always module bytes for the disabled-integration fixture."""
    registry = _regular_bytes(Path(source) / "crates/trusted-server-core/src/integrations/registry.rs", 33554432).decode()
    match = re.search(r'const JS_ALWAYS: &\[&str\] = &\[([^]]*)\];', registry)
    if match is None or match[1] != '"creative"':
        raise ValueError("source-owned always-JS selection changed; review runtime fixture")
    return b";\n".join(_regular_bytes(Path(js_dir) / ("tsjs-" + name + ".js"), MAX_RESPONSE) for name in ("core", "creative"))


def javascript_response(status, body, headers, script, expected):
    """Require fetched exact prepared JS bytes and its actual injected version."""
    if status != 200 or not body or "javascript" not in headers.get("content-type", "") or body != expected:
        raise ValueError("actual injected JavaScript bytes mismatch")
    digest = sha256_bytes(expected)
    if script != "/static/tsjs=tsjs-unified.min.js?v=" + digest:
        raise ValueError("injected JavaScript version hash mismatch")
    return digest


class _Origin(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.server.requests.append(self.path)
        if len(self.server.requests) > 100:
            self.server.failure.set()
            self.send_error(507)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(HTML)))
        self.end_headers()
        self.wfile.write(HTML)

    def log_message(self, format, *args):
        return


def runtime_config(source, destination, origin_port, variant):
    """Create fictional integrity-checked runtime configuration without a build."""
    config_root = Path(source) / "crates/trusted-server-integration-tests/fixtures/configs"
    data = tomllib.loads(_regular_bytes(config_root / "trusted-server.integration.toml", 1048576).decode())
    data["publisher"].update(domain="publisher.example.com", cookie_domain="publisher.example.com", origin_url=f"http://127.0.0.1:{origin_port}")
    data["response_headers"] = {"x-release-runtime": variant}
    for integration in data["integrations"].values():
        integration["enabled"] = False
    # This fixed fixture contains only integers, strings, booleans and arrays;
    # Python's compact sorted JSON agrees with the selected EdgeZero canonical
    # v1 rules for this subset. Floats must not silently gain another spelling.
    def no_float(value):
        if isinstance(value, float):
            raise ValueError("runtime fixture canonical float unsupported")
        if isinstance(value, dict):
            for child in value.values():
                no_float(child)
        elif isinstance(value, list):
            for child in value:
                no_float(child)
    no_float(data)
    canonical = json.dumps(data, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    envelope = json.dumps({"data": data, "sha256": sha256_bytes(canonical), "version": 1,
                           "generated_at": "2026-01-01T00:00:00Z"}, ensure_ascii=False, separators=(",", ":"))
    manifest = tomllib.loads(_regular_bytes(Path(source) / "edgezero.toml", 1048576).decode())
    store = manifest["stores"]["config"]["default"]
    if not isinstance(store, str) or not re.fullmatch(r"[a-zA-Z0-9_-]+", store) or "'''" in envelope:
        raise ValueError("reviewed runtime store identity changed")
    template = _regular_bytes(config_root / "viceroy-template.toml", 1048576).decode()
    marker = "# GENERATED_TRUSTED_SERVER_CONFIG_STORES"
    if template.count(marker) != 1:
        raise ValueError("runtime config marker mismatch")
    block = f"[local_server.config_stores.{store}]\nformat = \"inline-toml\"\n[local_server.config_stores.{store}.contents]\n{store} = '''{envelope}'''"
    destination.write_text(template.replace(marker, block))
    # Parse the exact generated TOML before invoking the runtime.
    tomllib.loads(destination.read_text())
    return sha256_bytes(destination.read_bytes())


def publisher_runtime(wasm, source, workspace, viceroy, environment, expected):
    """Exercise publisher/injection/JS exact bytes; this alone is no acceptance.

    A local development-artifact diagnostic may call this with explicitly
    unqualified tools/bytes. Official acceptance calls it only after maintained
    validation and authenticated source/input/tool selection.
    """
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Origin)
    server.requests, server.failure = [], threading.Event()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    observations = []
    try:
        for variant in ("A", "B"):
            origin_start = len(server.requests)
            configuration = workspace / f"viceroy-{variant}.toml"
            config_hash = runtime_config(source, configuration, server.server_port, variant)
            port = _port()
            process = subprocess.Popen([str(viceroy), str(wasm), "-C", str(configuration), "--addr", f"127.0.0.1:{port}"],
                env=environment, cwd=workspace, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                close_fds=True, start_new_session=True)
            logs = RuntimeLogs(process, workspace / f"viceroy-{variant}")
            try:
                deadline = time.monotonic() + 60
                while True:
                    logs.check()
                    if process.poll() is not None:
                        raise ValueError("exact Wasm Viceroy exits before publisher fixture")
                    try:
                        status, html, headers = _request(port, "/release-fixture", deadline=deadline, logs=logs)
                        break
                    except (OSError, http.client.HTTPException):
                        if time.monotonic() >= deadline:
                            raise ValueError("exact Wasm publisher runtime timeout")
                        time.sleep(0.05)
                script = publisher_response(status, html, headers)
                if headers.get("x-release-runtime") != variant:
                    raise ValueError("runtime config cannot change independently of build")
                status, javascript, script_headers = _request(port, script, deadline=deadline, logs=logs)
                js_hash = javascript_response(status, javascript, script_headers, script, expected)
                origin = server.requests[origin_start:]
                if "/release-fixture" not in origin or server.failure.is_set():
                    raise ValueError("publisher variant did not actually reach bounded origin fixture")
                observations.append({"variant": variant, "publisher_status": 200, "script_status": 200,
                    "html_sha256": sha256_bytes(html), "javascript_sha256": js_hash,
                    "config_sha256": config_hash, "script_path": script, "origin_requests": origin})
            finally:
                logs.close()
            logs.check()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
    if observations[0]["config_sha256"] == observations[1]["config_sha256"]:
        raise ValueError("independent runtime config variants did not change")
    return observations


def validate_final(wasm, source, workspace, tools, environment, js_dir):
    """Accept unchanged final bytes after maintained validation and actual runtime."""
    from rust_recipe import capture
    wasm = Path(wasm).resolve(strict=True)
    workspace = Path(workspace).resolve(strict=True)
    before = sha256_bytes(_regular_bytes(wasm, MAX_EXECUTABLE))
    abi = core_wasm(_regular_bytes(wasm, MAX_EXECUTABLE))
    if Path(source).resolve() == workspace or workspace.is_relative_to(Path(source).resolve()):
        raise ValueError("runtime fixture must run outside checkout")
    expected = expected_javascript(source, js_dir)
    capture([tools["wasmtime"], "compile", str(wasm), "--output", str(workspace / "validated.cwasm")],
            environment, workspace, workspace / "wasmtime", timeout=300)
    observations = publisher_runtime(wasm, source, workspace, tools["viceroy"], environment, expected)
    after = sha256_bytes(_regular_bytes(wasm, MAX_EXECUTABLE))
    if before != after:
        raise ValueError("final Wasm changed during runtime acceptance")
    result = {"sha256_before": before, "sha256_after": after, "abi": abi, "runtime": observations,
              "outside_checkout": True, "exact_final_bytes": True, "maintained_validation": True}
    (workspace / "runtime-outcome.json").write_bytes(deterministic_json_bytes(result))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--wasm", required=True, type=Path)
    parser.parse_args()
    raise ValueError("release qualification pending: authenticated Wasmtime/Viceroy selection required")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        logging.error("Final Fastly acceptance refuses: %s", error)
        sys.exit(1)
