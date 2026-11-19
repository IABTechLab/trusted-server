"""Validate recorded upstream bodies and write descriptive measurement summaries."""

import base64
import collections
import hashlib
import json
import statistics
from pathlib import Path


def parse_stream(connection, data):
    """Parse complete HTTP/1 requests, rejecting truncated evidence."""
    rows = []
    position = 0
    while position < len(data):
        end = data.find(b"\r\n\r\n", position)
        if end < 0:
            raise ValueError("incomplete request head")
        lines = bytes(data[position:end]).decode("latin1").split("\r\n")
        method, path, _version = lines[0].split(" ")
        headers = dict(line.split(":", 1) for line in lines[1:])
        headers = {key.lower(): value.strip() for key, value in headers.items()}
        position = end + 4
        chunked = headers.get("transfer-encoding") == "chunked"
        if chunked:
            body = bytearray()
            while True:
                end = data.find(b"\r\n", position)
                if end < 0:
                    raise ValueError("incomplete chunk size")
                size = int(data[position:end].split(b";")[0], 16)
                position = end + 2
                if size < 0 or position + size + 2 > len(data):
                    raise ValueError("incomplete chunk body")
                body.extend(data[position:position + size])
                position += size
                if data[position:position + 2] != b"\r\n":
                    raise ValueError("invalid chunk terminator")
                position += 2
                if size == 0:
                    break
        else:
            size = int(headers.get("content-length", "0"))
            if size < 0 or position + size > len(data):
                raise ValueError("incomplete request body")
            body = data[position:position + size]
            position += size
        rows.append({
            "connection": connection, "method": method, "path": path,
            "bytes": len(body), "sha256": hashlib.sha256(body).hexdigest(),
            "cookie": headers.get("cookie"), "action": headers.get("next-action"),
            "chunked": chunked,
        })
    return rows


def validate_history(rows, expected_count=30):
    """Require exact historical bodies and reuse across distinct requests."""
    history = [row for row in rows if row["action"] == "test-action-id"]
    if len(history) != expected_count:
        raise ValueError("missing historical POST receipts")
    expected_hash = hashlib.sha256(b"[]").hexdigest()
    if any(row["bytes"] != 2 or row["sha256"] != expected_hash for row in history):
        raise ValueError("historical POST body changed")
    counts = collections.Counter(row["connection"] for row in history)
    if max(counts.values(), default=0) < 2:
        raise ValueError("historical requests do not demonstrate upstream reuse")
    return history


def timing(values):
    """Return descriptive milliseconds without a performance pass threshold."""
    values = sorted(values)
    return {
        "n": len(values), "median_ms": round(statistics.median(values), 3),
        "p95_ms": round(values[min(len(values) - 1, int(len(values) * .95))], 3),
        "max_ms": round(max(values), 3),
    }


def main():
    root = Path(__file__).resolve().parent
    streams = collections.defaultdict(bytearray)
    for line in (root / "connections.jsonl").read_text().splitlines():
        event = json.loads(line)
        if event["event"] == "request-bytes":
            streams[event["id"]].extend(base64.b64decode(event["data"], validate=True))
    rows = [row for connection, data in streams.items() for row in parse_stream(connection, data)]
    history = validate_history(rows)
    result = json.loads((root / "results.json").read_text())
    if result.get("success") is not True:
        raise ValueError("runtime assertions did not pass")
    counts = collections.Counter(row["connection"] for row in rows)
    actual = [row for row in rows if row["action"] and row["action"] != "test-action-id"]
    summary = {
        "success": True, "upstream_requests": len(rows), "used_connections": len(counts),
        "requests_per_connection": dict(counts), "historical_posts": len(history),
        "historical_connections": sorted({row["connection"] for row in history}),
        "valid_fetch_actions": len(actual),
        "valid_fetch_action_connections": sorted({row["connection"] for row in actual}),
        "timings": {},
    }
    for port in [18980, 18982]:
        requests = [row for row in result["requests"] if row["port"] == port]
        summary["timings"][str(port)] = {
            "historical_post": timing([row["ms"] for row in requests if row["path"] == "/"]),
            "api_get": timing([row["ms"] for row in requests if row["path"] == "/api/hello"]),
            "concurrent_echo": timing([row["ms"] for row in requests if row["path"].startswith("/api/echo?probe=probe-concurrent")]),
            "browser_cycles": timing([cycle["ms"] for row in result["browser"] if row["port"] == port for cycle in row["cycles"]]),
        }
    (root / "parsed-requests.json").write_text(json.dumps(rows, indent=2) + "\n")
    (root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    metadata = json.loads((root / "metadata.json").read_text())
    lines = ["# Axum / Next.js measurement", "", f"Runtime revision: `{metadata['runtime_revision']}`.", "",
             f"Correctness checks passed. {len(rows)} upstream requests used {len(counts)} TCP connections; {len(history)} historical POST receipts were complete and demonstrated reuse.", "",
             "| Workload | Direct median / p95 (ms) | Managed median / p95 (ms) |", "| --- | ---: | ---: |"]
    for name in summary["timings"]["18980"]:
        direct = summary["timings"]["18980"][name]
        managed = summary["timings"]["18982"][name]
        lines.append(f"| {name} | {direct['median_ms']} / {direct['p95_ms']} | {managed['median_ms']} / {managed['p95_ms']} |")
    lines += ["", "Local HTTP/1, explicitly allowed proxy origin. Timings include fixture pacing and queueing; browser cycles include loading and returning navigation. These are complete-path observations, not a pooled-versus-fresh comparison. No timing threshold is enforced.", "", "Artifacts include source/tool/binary identity, individual observations, parsed receipts, raw synthetic TCP requests and logs."]
    (root / "summary.md").write_text("\n".join(lines) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
