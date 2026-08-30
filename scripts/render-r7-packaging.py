#!/usr/bin/env python3
"""Render the closed ClipMesh deployment-template set into a new directory."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import sys
from typing import NoReturn


TOKEN = re.compile(r"@@([A-Z0-9_]+)@@")
ASSETS = (
    ("deploy/config/clipmesh-hub.toml", "clipmesh-hub.toml", 0o600),
    ("deploy/config/clipmesh-agent.toml", "clipmesh-agent.toml", 0o600),
    ("deploy/systemd/clipmesh-hub.service", "clipmesh-hub.service", 0o644),
    ("deploy/systemd/clipmesh-agent.service", "clipmesh-agent.service", 0o644),
    (
        "deploy/launchd/com.example.clipmesh-agent.plist",
        "com.example.clipmesh-agent.plist",
        0o644,
    ),
)
DERIVED_PATH_TOKENS = {
    "CLIPMESH_AGENT_STATE_DIRECTORY": "CLIPMESH_STATE_PATH",
    "CLIPMESH_AGENT_CONTROL_DIRECTORY": "CLIPMESH_CONTROL_SOCKET",
}


def fail(message: str) -> NoReturn:
    print(f"r7 packaging render failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def load_values() -> dict[str, str]:
    try:
        raw = json.load(sys.stdin)
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        fail(f"input is not one JSON object: {error}")
    if not isinstance(raw, dict):
        fail("input is not one JSON object")
    values: dict[str, str] = {}
    for key, value in raw.items():
        if not isinstance(key, str) or not re.fullmatch(r"[A-Z0-9_]+", key):
            fail("input contains an invalid variable name")
        if isinstance(value, bool) or not isinstance(value, (str, int)):
            fail(f"{key} is not a string or integer")
        rendered = str(value)
        if not rendered or "\x00" in rendered or "\n" in rendered or "\r" in rendered:
            fail(f"{key} is empty or contains a forbidden control character")
        values[key] = rendered
    return values


def main() -> None:
    if len(sys.argv) != 2:
        fail("usage: render-r7-packaging.py OUTPUT_DIRECTORY < variables.json")

    repository = Path(__file__).resolve().parent.parent
    sources: list[tuple[Path, str, int, str]] = []
    required: set[str] = set()
    for source_name, output_name, mode in ASSETS:
        source = repository / source_name
        text = source.read_text(encoding="utf-8")
        tokens = set(TOKEN.findall(text))
        if not tokens:
            fail(f"{source_name} contains no render variables")
        required.update(tokens)
        sources.append((source, output_name, mode, text))

    values = load_values()
    external_required = required - DERIVED_PATH_TOKENS.keys()
    missing = sorted(external_required - values.keys())
    unknown = sorted(values.keys() - external_required)
    if missing:
        fail(f"missing variables: {','.join(missing)}")
    if unknown:
        fail(f"unknown variables: {','.join(unknown)}")
    for target, source in DERIVED_PATH_TOKENS.items():
        path = Path(values[source])
        if not path.is_absolute() or path.parent == Path("/"):
            fail(f"{source} must have an absolute non-root parent")
        values[target] = str(path.parent)

    output_directory = Path(sys.argv[1])
    old_umask = os.umask(0o077)
    try:
        output_directory.mkdir(mode=0o700, parents=True, exist_ok=False)
        for source, output_name, mode, text in sources:
            rendered = TOKEN.sub(lambda match: values[match.group(1)], text)
            if TOKEN.search(rendered):
                fail(f"unresolved variable in {source.relative_to(repository)}")
            target = output_directory / output_name
            with target.open("x", encoding="utf-8", newline="\n") as handle:
                handle.write(rendered)
            target.chmod(mode)
    except FileExistsError:
        fail("output directory or target already exists")
    finally:
        os.umask(old_umask)


if __name__ == "__main__":
    main()
