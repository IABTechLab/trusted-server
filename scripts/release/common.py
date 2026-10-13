"""Strict byte-oriented utilities shared by release producers and verifiers."""

import hashlib
import json
import math
import os
import re
import tempfile
from pathlib import Path


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key!r}")
        result[key] = value
    return result


def load_json_bytes(data, *, max_bytes, max_depth=64):
    """Decode a bounded UTF-8 JSON document without duplicate object keys.

    The caller selects the byte cap. Container depth includes the root and is
    checked before JSON decoding; string content does not add depth. Raise ValueError
    for invalid limits, malformed UTF-8 or JSON, nonfinite numbers, unpaired
    surrogate escapes, or an exceeded bound.
    """
    require_nonnegative_integer(max_bytes)
    require_nonnegative_integer(max_depth)
    if not isinstance(data, bytes):
        raise ValueError("JSON input must be bytes")
    if len(data) > max_bytes:
        raise ValueError(f"JSON byte size exceeds {max_bytes}")
    text = data.decode("utf-8")
    _check_depth(text, max_depth)
    document = json.loads(
        text,
        object_pairs_hook=_unique_object,
        parse_constant=_reject_constant,
        parse_float=_finite_float,
    )
    _check_unicode(document)
    return document


def require_nonnegative_integer(value):
    """Return a nonnegative integer, rejecting booleans and other types."""
    if type(value) is not int or value < 0:
        raise ValueError("value must be a nonnegative integer")
    return value


def read_bounded(stream, *, max_bytes):
    """Read a binary stream to EOF, consuming at most the limit plus one byte.

    Short reads are supported; file metadata is never trusted as a size bound.
    Raise ValueError for an invalid limit, non-binary stream, or excess data.
    """
    require_nonnegative_integer(max_bytes)
    data = bytearray()
    while True:
        chunk = stream.read(min(65536, max_bytes + 1 - len(data)))
        if not isinstance(chunk, bytes):
            raise ValueError("stream must return binary bytes")
        if not chunk:
            return bytes(data)
        if len(data) + len(chunk) > max_bytes:
            raise ValueError(f"stream byte size exceeds {max_bytes}")
        data.extend(chunk)


def read_json_file(path, *, max_bytes, max_depth=64):
    """Read a file as bounded bytes and decode it as strict UTF-8 JSON.

    Raise ValueError for invalid limits or documents and OSError for read errors.
    Callers that also need the original digest should retain read_bounded bytes
    and pass the same bytes to load_json_bytes and sha256_bytes.
    """
    with Path(path).open("rb") as stream:
        data = read_bounded(stream, max_bytes=max_bytes)
    return load_json_bytes(data, max_bytes=max_bytes, max_depth=max_depth)


def sha256_bytes(data):
    """Return the lowercase SHA-256 digest of the exact supplied bytes."""
    if not isinstance(data, bytes):
        raise ValueError("digest input must be bytes")
    return hashlib.sha256(data).hexdigest()


def require_safe_asset_name(name):
    """Return a flat ASCII asset name matching the release document contract.

    Names have at most 160 characters, start with a letter or digit, and use
    letters, digits, underscores, or hyphens in nonempty dot-separated parts.
    Raise ValueError for an unsafe name.
    """
    if (
        not isinstance(name, str)
        or len(name) > 160
        or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*(?:\.[A-Za-z0-9_-]+)*", name)
    ):
        raise ValueError("asset name must be a safe flat ASCII filename")
    return name


def require_unique_identities(records, fields):
    """Reject repeated identity tuples selected by the caller's field names.

    Identity values must be strings or integers; missing fields and compound
    values raise ValueError rather than participating in ambiguous matches.
    """
    seen = set()
    for record in records:
        if not isinstance(record, dict) or any(field not in record for field in fields):
            raise ValueError("identity fields must be present in an object")
        identity = tuple(record[field] for field in fields)
        if any(type(value) not in (str, int) for value in identity):
            raise ValueError("identity fields must be strings or integers")
        if identity in seen:
            raise ValueError(f"duplicate identity: {identity!r}")
        seen.add(identity)


def deterministic_json_bytes(document):
    """Serialize sorted, compact UTF-8 JSON bytes followed by one newline.

    This is a producer encoding, not a canonicalization step for verification.
    Hash signed or downloaded documents using their original bytes instead.
    Raise ValueError for nonfinite numbers, surrogate strings, or invalid keys.
    """
    text = json.dumps(
        document,
        ensure_ascii=False,
        allow_nan=False,
        sort_keys=True,
        separators=(",", ":"),
    )
    _check_unicode(document)
    return (text + "\n").encode("utf-8")


def atomic_write(path, data):
    """Replace a destination with exact bytes using a private sibling file.

    Flush and sync the temporary file before atomic replacement. On failure,
    remove the temporary file and preserve the destination's existing bytes.
    Raise ValueError for non-byte data and OSError for filesystem failures.
    """
    if not isinstance(data, bytes):
        raise ValueError("atomic write input must be bytes")
    destination = Path(path)
    temporary_path = None
    try:
        with tempfile.NamedTemporaryFile(
            dir=destination.parent, prefix=f".{destination.name}.", delete=False
        ) as temporary:
            temporary_path = Path(temporary.name)
            temporary.write(data)
            temporary.flush()
            os.fsync(temporary.fileno())
        os.replace(temporary_path, destination)
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def _check_depth(text, max_depth):
    depth = 0
    in_string = False
    escaped = False
    for character in text:
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
        elif character == '"':
            in_string = True
        elif character in "[{":
            depth += 1
            if depth > max_depth:
                raise ValueError(f"JSON container depth exceeds {max_depth}")
        elif character in "]}":
            depth -= 1


def _check_unicode(document):
    pending = [document]
    while pending:
        value = pending.pop()
        if isinstance(value, str):
            value.encode("utf-8")
        elif isinstance(value, dict):
            if any(not isinstance(key, str) for key in value):
                raise ValueError("JSON object keys must be strings")
            pending.extend(value.keys())
            pending.extend(value.values())
        elif isinstance(value, (list, tuple)):
            pending.extend(value)


def _reject_constant(value):
    raise ValueError(f"JSON number must be finite: {value}")


def _finite_float(value):
    number = float(value)
    if not math.isfinite(number):
        raise ValueError(f"JSON number must be finite: {value}")
    return number
