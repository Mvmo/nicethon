#!/usr/bin/env python3
"""Builds the compiler, runs the golden tests and type checks the examples.
Usage: python3 tests/run.py [name filter]"""
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GOLDEN = ROOT / "tests" / "golden"
COMPILER = ROOT / "target" / "debug" / "nicethon"


def run_test(path):
    result = subprocess.run([COMPILER, "run", path], capture_output=True, text=True)
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


def interop_test(directory):
    with tempfile.TemporaryDirectory() as out:
        for source in directory.glob("*.nice"):
            build = subprocess.run([COMPILER, "build", source, "-o", out], capture_output=True, text=True)
            if build.returncode != 0:
                return build.stderr
        env = dict(os.environ, PYTHONPATH=out)
        result = subprocess.run([COMPILER, "python", directory / "driver.py"], capture_output=True, text=True, env=env, cwd=directory)
    expected = (directory / "expected.out").read_text()
    return None if result.stdout == expected else f"expected:\n{expected}\ngot:\n{result.stdout}\n{result.stderr}"


def example_test(path):
    result = subprocess.run([COMPILER, "check", path], capture_output=True, text=True)
    return result.stderr if result.returncode != 0 else None


def main():
    subprocess.run(["cargo", "build", "--quiet"], cwd=ROOT, check=True)
    selected = sys.argv[1] if len(sys.argv) > 1 else ""
    tests = [(p, run_test) for p in sorted((GOLDEN / "run").glob("*.nice"))]
    tests += [(p, error_test) for p in sorted((GOLDEN / "err").glob("*.nice"))]
    tests += [(p, interop_test) for p in sorted((GOLDEN / "interop").iterdir()) if p.is_dir()]
    tests += [(p, example_test) for p in sorted((ROOT / "examples").rglob("*.nice")) if p.is_file() and ".nice" not in p.parts]
    failed = 0
    for path, check in tests:
        if selected not in path.name:
            continue
        problem = check(path)
        print(f"{'FAIL' if problem else 'ok  '} {path.name}")
        if problem:
            failed += 1
            print("    " + problem.replace("\n", "\n    "))
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
