#!/usr/bin/env python3
"""Fuzzing for the Nicethon compiler.

python3 tests/fuzz.py syntax [rounds] [seed]
    Mutates the golden test programs. The compiler must never crash or hang,
    a mutant that type checks must compile to C, and running it must not
    crash the process.

Failing inputs are written to build/fuzz/.
"""
import math
import os
import random
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
COMPILER = ROOT / "target" / "debug" / "nicethon"
FINDINGS = ROOT / "build" / "fuzz"
PYTHON = os.path.join(os.environ["VIRTUAL_ENV"], "bin", "python") if "VIRTUAL_ENV" in os.environ else sys.executable
VOCABULARY = ["fn", "let", "var", "match", "if", "else", "{", "}", "(", ")", "[", "]", ";", ",", "=>", "->", ".", "?",
              "as!", "as?", "|>", "..", "Int", "Float", "Str", "Dyn", "List<Int>", "Option<", ">", "0", "-1", "\"x\"",
              "`a${1}`", "try", "catch", "throw", "return", "break", "self", "py", "struct", "enum", "impl", "<T>"]


def save(kind, number, files):
    folder = FINDINGS / f"{kind}-{number}"
    folder.mkdir(parents=True, exist_ok=True)
    for name, text in files.items():
        (folder / name).write_text(text)
    return folder


def run(command, timeout=30, **options):
    try:
        return subprocess.run(command, capture_output=True, text=True, timeout=timeout, **options)
    except subprocess.TimeoutExpired:
        return None


# Mutation fuzzing of the front end and code generator.

def mutate(rng, text):
    lines = text.split("\n")
    for _ in range(rng.randint(1, 3)):
        position = rng.randrange(len(text) + 1)
        choice = rng.randrange(10)
        if choice >= 6:
            # Type preserving: other numbers, operators and booleans.
            swaps = [(r"\b\d+\b", lambda m: str(rng.choice([0, 1, -1, 2, 7, 9223372036854775807]))),
                     (r" [+\-*%] ", lambda m: rng.choice([" + ", " - ", " * ", " % ", " ~/ "])),
                     (r" (<|>|<=|>=|==|!=) ", lambda m: rng.choice([" < ", " > ", " == ", " != "])),
                     (r"\b(true|false)\b", lambda m: rng.choice(["true", "false"]))]
            pattern, replace = swaps[choice - 6]
            matches = list(re.finditer(pattern, text))
            if matches:
                match = rng.choice(matches)
                text = text[:match.start()] + replace(match) + text[match.end():]
        elif choice == 0:
            text = text[:position] + text[position + rng.randint(1, 20):]
        elif choice == 1:
            text = text[:position] + " " + rng.choice(VOCABULARY) + " " + text[position:]
        elif choice == 2 and len(lines) > 1:
            index = rng.randrange(len(lines))
            lines.insert(index, lines[rng.randrange(len(lines))])
            text = "\n".join(lines)
        elif choice == 3 and len(lines) > 1:
            del lines[rng.randrange(len(lines))]
            text = "\n".join(lines)
        elif choice == 4:
            names = re.findall(r"\b[a-z_][a-z_0-9]*\b", text)
            if names:
                text = re.sub(rf"\b{re.escape(rng.choice(names))}\b", rng.choice(names), text, count=1)
        else:
            text = text[:position]
        lines = text.split("\n")
    return text


def syntax(rounds, rng):
    corpus = [p.read_text() for p in sorted((ROOT / "tests" / "golden").rglob("*.nice"))]
    failures = executed = 0
    with tempfile.TemporaryDirectory() as work:
        source = Path(work) / "mutant.nice"
        for number in range(rounds):
            text = mutate(rng, rng.choice(corpus))
            source.write_text(text)
            problem = None
            checked = run([COMPILER, "check", source], timeout=10)
            if checked is None:
                problem = "check hangs"
            elif checked.returncode not in (0, 1) or "panicked" in checked.stderr:
                problem = f"check crashed:\n{checked.stderr}"
            elif checked.returncode == 0:
                out = Path(work) / "out"
                built = run([COMPILER, "build", source, "-o", out, "--python", PYTHON], timeout=120)
                if built is None or built.returncode != 0:
                    problem = f"build failed:\n{built.stderr if built else 'timeout'}"
                else:
                    script = "import sys; sys.path.insert(0, sys.argv[1]); import mutant; mutant.main()"
                    ran = run([PYTHON, "-c", script, out], timeout=10, cwd=work)
                    executed += 1
                    if ran is not None and ran.returncode < 0:
                        problem = f"crashed with signal {-ran.returncode}:\n{ran.stderr[-2000:]}"
            if problem:
                failures += 1
                folder = save("syntax", number, {"mutant.nice": text, "problem.txt": problem})
                print(f"FAIL {number}: {problem.splitlines()[0]} -> {folder}")
            elif number % 50 == 0:
                print(f"{number} mutants ok, {executed} of them type checked, compiled and ran")
    return failures


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "syntax"
    rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 100
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else random.randrange(1 << 30)
    print(f"{mode}: {rounds} rounds, seed {seed}")
    rng = random.Random(seed)
    failures = syntax(rounds, rng)
    print(f"{failures} failures")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
