#!/usr/bin/env python3
"""Emit bounded GitHub annotations from an already-failed test log."""
import argparse
import os
from pathlib import Path
import re


def excerpt(text: str) -> str:
    lines = text.splitlines()
    selected = set()
    for index, line in enumerate(lines):
        if re.search(r"\bFAILED\b|panicked at|^error: test failed", line):
            selected.update(range(max(0, index - 1), min(len(lines), index + 12)))
    result = "\n".join(lines[index] for index in sorted(selected))
    return (result or "\n".join(lines[-35:]))[-16000:]


def annotation(value: str) -> str:
    return value.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("log", type=Path)
    args = parser.parse_args()
    detail = excerpt(args.log.read_text(errors="replace"))
    print("::error title=Integration test failure::" + annotation(detail))
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary).open("a") as stream:
            stream.write("## Integration test failure\n\n````text\n")
            stream.write(detail.replace("````", "''''") + "\n````\n")


if __name__ == "__main__":
    main()
