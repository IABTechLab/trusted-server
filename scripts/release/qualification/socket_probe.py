"""Measure real socket operations in this process and inherited descendants.

No result here grants qualification. External destinations are documentation
addresses, not services: routing/refusal/timeout is recorded as unavailable.
"""

import argparse
import errno
import json
import os
import socket
import subprocess
import sys
from pathlib import Path

DENIAL = {errno.EPERM, errno.EACCES}


def internet_probe(depth, family, protocol, operation, destination):
    """Attempt an operation, retaining the actual failing phase and errno."""
    address_family = socket.AF_INET if family == "ipv4" else socket.AF_INET6
    loopback = "127.0.0.1" if family == "ipv4" else "::1"
    external = "192.0.2.1" if family == "ipv4" else "2001:db8::1"
    record = {"depth": depth, "family": family, "protocol": protocol, "operation": operation,
              "destination": destination, "outcome": "unavailable", "errno": 0,
              "phase": "socket"}
    sockets = []
    try:
        transport = socket.SOCK_DGRAM if protocol == "udp" else socket.SOCK_STREAM
        client = socket.socket(address_family, transport)
        sockets.append(client)
        client.settimeout(0.15)
        address = loopback if destination == "loopback" else external
        port = 9
        if protocol == "tcp" and operation in {"connect", "send"} and destination == "loopback":
            # An unconfined local listener gives the client a positive endpoint.
            # Its setup failure must never masquerade as client connect denial:
            # still attempt the actual client syscall against the fallback port.
            try:
                server = socket.socket(address_family, socket.SOCK_STREAM)
                sockets.append(server)
                server.bind((loopback, 0))
                server.listen(1)
                port = server.getsockname()[1]
            except OSError:
                pass
        record["phase"] = operation
        if operation == "bind":
            client.bind((address, 0))
        elif operation == "connect":
            client.connect((address, port))
        elif protocol == "udp":
            client.sendto(b"qualification-probe", (address, port))
        else:
            record["phase"] = "connect"
            client.connect((address, port))
            record["phase"] = "send"
            client.sendall(b"qualification-probe")
        record["outcome"] = "allowed"
    except OSError as error:
        record["errno"] = error.errno or errno.ETIMEDOUT
        record["outcome"] = "denied" if error.errno in DENIAL else "unavailable"
    finally:
        for connection in sockets:
            connection.close()
    return record


def unix_probe(depth, private_root):
    """Exchange bytes over a filesystem Unix socket inside the private root."""
    record = {"depth": depth, "family": "unix", "operation": "exchange",
              "destination": "private", "outcome": "unavailable", "errno": 0,
              "phase": "socket", "protocol": "unix"}
    path = str(private_root / f"probe-{depth}.sock")
    sockets = []
    try:
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sockets.append(server)
        record["phase"] = "bind"
        server.bind(path)
        server.listen(1)
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sockets.append(client)
        record["phase"] = "connect"
        client.connect(path)
        accepted, _ = server.accept()
        sockets.append(accepted)
        record["phase"] = "exchange"
        client.sendall(b"private")
        if accepted.recv(7) == b"private":
            record["outcome"] = "allowed"
    except OSError as error:
        record["errno"] = error.errno or errno.ETIMEDOUT
        record["outcome"] = "denied" if error.errno in DENIAL else "unavailable"
    finally:
        for connection in sockets:
            connection.close()
        if os.path.exists(path):
            os.unlink(path)
    return record


def probe_tree(private_root, depth=0):
    """Close inherited FDs for descendants and probe three generations."""
    results = [internet_probe(depth, family, protocol, operation, destination)
               for family in ("ipv4", "ipv6") for protocol in ("tcp", "udp")
               for operation in ("bind", "connect", "send")
               for destination in ("loopback", "external")]
    results.append(unix_probe(depth, private_root))
    if depth < 2:
        child = subprocess.run([sys.executable, "-I", "-B", str(Path(__file__).resolve()),
                                "--private-root", str(private_root), "--depth", str(depth + 1)],
                               env={"HOME": str(private_root), "TMPDIR": str(private_root),
                                    "LANG": "C", "LC_ALL": "C"}, close_fds=True,
                               capture_output=True, check=False, timeout=15)
        if child.returncode or len(child.stdout) > 65536:
            raise ValueError("socket descendant probe failed")
        results.extend(json.loads(child.stdout))
    return results


def main():
    """Print observations only; no caller-supplied commands are accepted."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--private-root", type=Path, required=True)
    parser.add_argument("--depth", type=int, choices=(0, 1, 2), default=0)
    arguments = parser.parse_args()
    root = arguments.private_root.resolve(strict=True)
    if not root.is_dir() or arguments.private_root.is_symlink():
        raise ValueError("probe requires a private existing directory")
    sys.stdout.write(json.dumps(probe_tree(root, arguments.depth), separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
