#!/usr/bin/env python3
"""Runs the golden tests. Usage: python3 tests/run.py [name filter]"""
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GOLDEN = ROOT / "tests" / "golden"
COMPILER = ROOT / "target" / "debug" / "nicethon"
PYTHON = os.path.join(os.environ["VIRTUAL_ENV"], "bin", "python") if "VIRTUAL_ENV" in os.environ else sys.executable


def importable(source):
    modules = re.findall(r"^// requires: (\S+)", source, re.M)
    return all(subprocess.run([PYTHON, "-c", f"import {m}"], capture_output=True).returncode == 0 for m in modules)


def run_test(path):
    result = subprocess.run([COMPILER, "run", path, "--python", PYTHON], capture_output=True, text=True)
    expected = path.with_suffix(".out").read_text()
    if result.stdout == expected:
        return None
    return f"expected:\n{expected}\ngot:\n{result.stdout}\n{result.stderr}"


def error_test(path):
    result = subprocess.run([COMPILER, "check", path], capture_output=True, text=True)
    errors = {}
    for line in result.stderr.splitlines():
        match = re.match(r".*?:(\d+):\d+: error: (.*)", line)
        if match:
            errors.setdefault(int(match.group(1)), []).append(match.group(2))
    problems = []
    for number, line in enumerate(path.read_text().splitlines(), 1):
        match = re.search(r"//~ error: (.*)", line)
        found = errors.pop(number, [])
        if match and not any(match.group(1).strip() in message for message in found):
            problems.append(f"line {number}: expected '{match.group(1).strip()}', got {found}")
        elif not match and found:
            problems.append(f"line {number}: unexpected {found}")
    problems.extend(f"line {number}: unexpected {found}" for number, found in errors.items())
    return "\n".join(problems) or None


def main():
    selected = sys.argv[1] if len(sys.argv) > 1 else ""
    tests = [(p, run_test) for p in sorted((GOLDEN / "run").glob("*.nice"))]
    tests += [(p, error_test) for p in sorted((GOLDEN / "err").glob("*.nice"))]
    failed = 0
    for path, check in tests:
        if selected not in path.name:
            continue
        sources = [path] if path.is_file() else list(path.glob("*.nice"))
        if not all(importable(s.read_text()) for s in sources):
            print(f"skip {path.name}")
            continue
        problem = check(path)
        print(f"{'FAIL' if problem else 'ok  '} {path.name}")
        if problem:
            failed += 1
            print("    " + problem.replace("\n", "\n    "))
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
