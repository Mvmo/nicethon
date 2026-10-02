#!/usr/bin/env python3
"""Fuzzing for the Nicethon compiler.

python3 tests/fuzz.py syntax [rounds] [seed]
    Mutates the golden test programs. The compiler must never crash or hang,
    a mutant that type checks must compile to C, and running it must not
    crash the process.

python3 tests/fuzz.py diff [rounds] [seed]
    Generates random well-typed programs together with a Python program that
    models Nicethon's semantics (Int overflow panics, ~/, %, IEEE floats).
    Both run, the outputs must match.

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


# Differential fuzzing: each expression is rendered as Nicethon and as Python.

INT, FLOAT, BOOL, STR, LIST = "Int", "Float", "Bool", "Str", "List<Int>"

PRELUDE_PY = '''
import math
from itertools import chain, islice

class Panic(Exception):
    pass

def i64(value):
    if not -2**63 <= value < 2**63:
        raise Panic("integer overflow")
    return value

def add(a, b): return i64(a + b)
def sub(a, b): return i64(a - b)
def mul(a, b): return i64(a * b)
def neg(a): return i64(-a)

def floordiv(a, b):
    if b == 0:
        raise Panic("division by zero")
    return i64(a // b)

def mod(a, b):
    if b == 0:
        raise Panic("modulo by zero")
    return a % b

def ipow(a, b):
    result = 1
    for _ in range(b):
        result = mul(result, a)
    return result

def fdiv(a, b):
    if b != 0:
        return a / b
    if a == 0 or a != a:
        return math.nan
    return math.copysign(math.inf, a) * math.copysign(1.0, b)

def fmod(a, b):
    return math.nan if b == 0 else a % b

def ffloordiv(a, b):
    return math.nan if b == 0 else a // b

def ffloor(a):
    return a if a == 0 or not math.isfinite(a) else float(math.floor(a))

def ftoi(a):
    if not -9223372036854775809.0 < a < 9223372036854775808.0:
        raise Panic("Float out of range for Int")
    return int(a)

def index(items, position):
    if not -len(items) <= position < len(items):
        raise Panic("index out of range")
    return items[position]

def get(items, position):
    return items[position] if 0 <= position < len(items) else None

def isum(items):
    total = 0
    for item in items:
        total = add(total, item)
    return total

def pick(flag, left, right):
    return left if flag else right

def ifold(items, function):
    total = 0
    for item in items:
        total = function(total, item)
    return total

def count(items):
    return sum(1 for _ in items)

class Holder:
    def __init__(self, value, count):
        self.value, self.count = value, count

def hold(value):
    return Holder(value, 1)

def bumped(holder):
    return Holder(holder.value, add(holder.count, 1))

def show(value, nested=False):
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, float):
        return repr(value)
    if isinstance(value, str):
        return '"' + value + '"' if nested else value
    if isinstance(value, list):
        return "[" + ", ".join(show(item, True) for item in value) + "]"
    return str(value)
'''

PRELUDE_NICE = '''import py { Panic } from "_nicethon";

fn pick<T>(flag: Bool, left: T, right: T) -> T {
    if flag { left } else { right }
}

struct Holder<T> {
    value: T;
    count: Int;
}

fn hold<T>(value: T) -> Holder<T> {
    Holder(value = value, count = 1)
}

fn bumped<T>(holder: Holder<T>) -> Holder<T> {
    holder.copy(count = holder.count + 1)
}
'''


class Program:
    def __init__(self, rng):
        self.rng = rng
        self.counter = 0
        self.functions = []

    def fresh(self, prefix):
        self.counter += 1
        return f"{prefix}{self.counter}"

    def literal(self, ty):
        rng = self.rng
        if ty == INT:
            value = rng.choice([rng.randint(-9, 9), rng.randint(-1000, 1000), 2**62 + rng.randint(-5, 5), -2**63 + rng.randint(1, 3)])
            return (f"({value})" if value < 0 else str(value)), str(value)
        if ty == FLOAT:
            value = rng.choice([0.0, 0.5, 1.5, -2.25, 3.0, 1e10, 0.1, -0.0, 7.75, 1e300])
            return (f"({value!r})" if math.copysign(1, value) < 0 else repr(value)), repr(value)
        if ty == BOOL:
            value = rng.random() < 0.5
            return ("true" if value else "false"), str(value)
        if ty == STR:
            word = rng.choice(["", "a", "bc", "Nice", "py", "Zz"])
            return f'"{word}"', repr(word)
        items = [self.literal(INT) for _ in range(rng.randint(0, 4))]
        if not items:
            return "([] as List<Int>)", "[]"
        return "[" + ", ".join(n for n, _ in items) + "]", "[" + ", ".join(p for _, p in items) + "]"

    def held(self, ty, sub):
        def make():
            value, pvalue = sub(ty)
            if self.rng.random() < 0.5:
                return f"hold({value}).value", f"hold({pvalue}).value"
            return f"bumped(bumped(hold({value}))).value", f"bumped(bumped(hold({pvalue}))).value"
        return make

    def expr(self, ty, scope, depth):
        rng = self.rng
        names = [name for name, kind in scope if kind == ty]
        if depth <= 0 or rng.random() < 0.25:
            if names and rng.random() < 0.6:
                name = rng.choice(names)
                return name, name
            return self.literal(ty)
        sub = lambda kind: self.expr(kind, scope, depth - 1)
        options = getattr(self, f"expr_{ty.replace('<', '_').replace('>', '')}")(sub, scope, depth)
        options.append(self.held(ty, sub))
        return rng.choice(options)()

    def expr_Int(self, sub, scope, depth):
        def binary(nice_op, py_function):
            def make():
                (a, pa), (b, pb) = sub(INT), sub(INT)
                return f"({a} {nice_op} {b})", f"{py_function}({pa}, {pb})"
            return make

        def power():
            (a, pa), exponent = sub(INT), self.rng.randint(0, 4)
            return f"({a} ** {exponent})", f"ipow({pa}, {exponent})"

        def negate():
            a, pa = sub(INT)
            return f"(-{a})", f"neg({pa})"

        def condition():
            (c, pc), (a, pa), (b, pb) = sub(BOOL), sub(INT), sub(INT)
            return f"(if {c} {{ {a} }} else {{ {b} }})", f"({pa} if {pc} else {pb})"

        def length():
            s, ps = sub(STR)
            return f"{s}.len()", f"len({ps})"

        def list_ops():
            (items, pitems), (i, pi) = sub(LIST), sub(INT)
            return self.rng.choice([
                (f"{items}.len()", f"len({pitems})"),
                (f"sum({items})", f"isum({pitems})"),
                (f"{items}[{i}]", f"index({pitems}, {pi})"),
            ])

        def option():
            (items, pitems), (i, pi), (k, pk), (other, pother) = sub(LIST), sub(INT), sub(INT), sub(INT)
            value = self.fresh("found")
            return (f"(match {items}.get({i}) {{ Some({value}) => ({value} + {k}), None => {other} }})",
                    f"(lambda {value}: add({value}, {pk}) if {value} is not None else {pother})(get({pitems}, {pi}))")

        def to_int():
            f, pf = sub(FLOAT)
            return f"({f} as Int)", f"ftoi({pf})"

        def generic():
            (c, pc), (a, pa), (b, pb) = sub(BOOL), sub(INT), sub(INT)
            return f"pick({c}, {a}, {b})", f"pick({pc}, {pa}, {pb})"

        def count():
            a, pa = sub(self.rng.choice([INT, FLOAT, STR, LIST]))
            return f"bumped(hold({a})).count", f"bumped(hold({pa})).count"

        def consume():
            items, pitems = sub(LIST)
            value = self.fresh("item")
            keep, pkeep = self.expr(BOOL, scope + [(value, INT)], depth - 1)
            other, pother = sub(INT)
            return self.rng.choice([
                (f"{items}.iter().count()", f"count(iter({pitems}))"),
                (f"{items}.iter().fold(0, (total, {value}) => total + {value})", f"ifold({pitems}, add)"),
                (f"(match {items}.iter().find({value} => {keep}) {{ Some(found) => found, None => {other} }})",
                 f"(lambda found: found if found is not None else {pother})(next(({value} for {value} in {pitems} if {pkeep}), None))"),
                (f"(if {items}.iter().any({value} => {keep}) {{ 1 }} else {{ 0 }})", f"(1 if any({pkeep} for {value} in {pitems}) else 0)"),
            ])

        options = [count, consume, binary("+", "add"), binary("-", "sub"), binary("*", "mul"), binary("~/", "floordiv"),
                   binary("%", "mod"), power, negate, condition, length, list_ops, option, to_int, generic]
        options += [self.call(INT, sub)] if self.functions else []
        if any(name == "bump" for name, _ in scope):
            def bump():
                a, pa = sub(INT)
                return f"bump({a})", f"bump({pa})"
            options.append(bump)
        return options

    def expr_Float(self, sub, scope, depth):
        def binary(nice_op, py_template):
            def make():
                (a, pa), (b, pb) = sub(FLOAT), sub(FLOAT)
                return f"({a} {nice_op} {b})", py_template.format(pa, pb)
            return make

        def int_division():
            (a, pa), (b, pb) = sub(INT), sub(INT)
            return f"({a} / {b})", f"fdiv(float({pa}), float({pb}))"

        def convert():
            a, pa = sub(INT)
            return f"({a} as Float)", f"float({pa})"

        def method():
            a, pa = sub(FLOAT)
            return self.rng.choice([(f"{a}.abs()", f"abs({pa})"), (f"{a}.floor()", f"ffloor({pa})")])

        def condition():
            (c, pc), (a, pa), (b, pb) = sub(BOOL), sub(FLOAT), sub(FLOAT)
            return f"(if {c} {{ {a} }} else {{ {b} }})", f"({pa} if {pc} else {pb})"

        def generic():
            (c, pc), (a, pa), (b, pb) = sub(BOOL), sub(FLOAT), sub(FLOAT)
            return f"pick({c}, {a}, {b})", f"pick({pc}, {pa}, {pb})"

        return [binary("+", "({} + {})"), binary("-", "({} - {})"), binary("*", "({} * {})"), binary("/", "fdiv({}, {})"),
                binary("%", "fmod({}, {})"), binary("~/", "ffloordiv({}, {})"), int_division, convert, method, condition, generic]

    def expr_Bool(self, sub, scope, depth):
        def compare():
            kind = self.rng.choice([INT, FLOAT, STR])
            op = self.rng.choice(["<", "<=", ">", ">=", "==", "!="])
            (a, pa), (b, pb) = sub(kind), sub(kind)
            return f"({a} {op} {b})", f"({pa} {op} {pb})"

        def logic():
            op, py_op = self.rng.choice([("&&", "and"), ("||", "or")])
            (a, pa), (b, pb) = sub(BOOL), sub(BOOL)
            return f"({a} {op} {b})", f"({pa} {py_op} {pb})"

        def negate():
            a, pa = sub(BOOL)
            return f"(!{a})", f"(not {pa})"

        def contains():
            (items, pitems), (i, pi) = sub(LIST), sub(INT)
            return f"{items}.contains({i})", f"({pi} in {pitems})"

        def prefix():
            (a, pa), (b, pb) = sub(STR), sub(STR)
            return f"{a}.starts_with({b})", f"{pa}.startswith({pb})"

        return [compare, compare, logic, negate, contains, prefix]

    def expr_Str(self, sub, scope, depth):
        def concat():
            (a, pa), (b, pb) = sub(STR), sub(STR)
            return f"({a} + {b})", f"({pa} + {pb})"

        def template():
            parts = [sub(self.rng.choice([INT, FLOAT, BOOL, STR, LIST])) for _ in range(self.rng.randint(1, 3))]
            nice = "`" + "-".join(f"${{{n}}}" for n, _ in parts) + "`"
            return nice, "(" + ' + "-" + '.join(f"show({p})" for _, p in parts) + ")"

        def method():
            a, pa = sub(STR)
            return self.rng.choice([(f"{a}.upper()", f"{pa}.upper()"), (f"{a}.lower()", f"{pa}.lower()")])

        def text_of():
            a, pa = sub(self.rng.choice([INT, FLOAT, BOOL, LIST]))
            return f"str({a})", f"show({pa})"

        def generic():
            (c, pc), (a, pa), (b, pb) = sub(BOOL), sub(STR), sub(STR)
            return f"pick({c}, {a}, {b})", f"pick({pc}, {pa}, {pb})"

        return [concat, template, method, text_of, generic]

    def expr_List_Int(self, sub, scope, depth):
        def mapped():
            items, pitems = sub(LIST)
            value = self.fresh("item")
            body, pbody = self.expr(INT, scope + [(value, INT)], depth - 1)
            return f"{items}.map({value} => {body})", f"[{pbody} for {value} in {pitems}]"

        def filtered():
            items, pitems = sub(LIST)
            value = self.fresh("item")
            body, pbody = self.expr(BOOL, scope + [(value, INT)], depth - 1)
            return f"{items}.filter({value} => {body})", f"[{value} for {value} in {pitems} if {pbody}]"

        def concat():
            (a, pa), (b, pb) = sub(LIST), sub(LIST)
            return f"({a} + {b})", f"({pa} + {pb})"

        def numbers():
            count = self.rng.randint(0, 5)
            return f"(0..{count})", f"list(range(0, {count}))"

        # Lazy iterator chains; the model uses generators so evaluation order matches.
        def lazy():
            items, pitems = sub(LIST)
            value = self.fresh("item")
            body, pbody = self.expr(INT, scope + [(value, INT)], depth - 1)
            keep, pkeep = self.expr(BOOL, scope + [(value, INT)], depth - 1)
            k = self.rng.randint(-1, 4)
            other, pother = sub(LIST)
            return self.rng.choice([
                (f"{items}.iter().map({value} => {body}).collect()", f"list(({pbody} for {value} in {pitems}))"),
                (f"{items}.iter().filter({value} => {keep}).take({k}).collect()", f"list(islice(({value} for {value} in {pitems} if {pkeep}), max({k}, 0)))"),
                (f"{items}.iter().map({value} => {body}).skip({k}).collect()", f"list(islice(({pbody} for {value} in {pitems}), max({k}, 0), None))"),
                (f"{items}.iter().chain({other}.iter()).collect()", f"list(chain({pitems}, {pother}))"),
                (f"{items}.zip({other}).map({value} => ({value}.0 - {value}.1)).collect()", f"[sub(a, b) for a, b in zip({pitems}, {pother})]"),
                (f"{items}.enumerate().map({value} => ({value}.0 * {value}.1)).collect()", f"[mul(i, v) for i, v in enumerate({pitems})]"),
                (f"range({k}, {k + 3}).collect()", f"list(range({k}, {k + 3}))"),
            ])

        return [mapped, filtered, concat, numbers, lazy]

    def call(self, ty, sub):
        def make():
            candidates = [f for f in self.functions if f[2] == ty]
            if not candidates:
                return self.literal(ty)
            name, params, _ = self.rng.choice(candidates)
            args = [sub(kind) for kind in params]
            return f"{name}({', '.join(a for a, _ in args)})", f"{name}({', '.join(p for _, p in args)})"
        return make

    def function(self):
        name = self.fresh("helper")
        params = [self.rng.choice([INT, FLOAT, STR, BOOL]) for _ in range(self.rng.randint(1, 3))]
        ret = self.rng.choice([INT, FLOAT, STR, BOOL])
        scope = [(f"p{i}", kind) for i, kind in enumerate(params)]
        body, pbody = self.expr(ret, scope, 3)
        nice = f"fn {name}({', '.join(f'p{i}: {k}' for i, k in enumerate(params))}) -> {ret} {{\n    {body}\n}}\n"
        python = f"def {name}({', '.join(f'p{i}' for i in range(len(params)))}):\n    return {pbody}\n"
        self.functions.append((name, params, ret))
        return nice, python

    def block(self, scope, depth, indent):
        nice, python = [], []
        scope = list(scope)
        pad, ppad = "    " * indent, "    " * indent
        for _ in range(self.rng.randint(1, 5)):
            choice = self.rng.randrange(7 if depth > 0 else 4)
            if choice == 0:
                ty = self.rng.choice([INT, FLOAT, BOOL, STR, LIST])
                name = self.fresh("value")
                value, pvalue = self.expr(ty, scope, 3)
                nice.append(f"{pad}let {name}: {ty} = {value};")
                python.append(f"{ppad}{name} = {pvalue}")
                scope.append((name, ty))
            elif choice == 1:
                ty = self.rng.choice([INT, FLOAT, BOOL, STR, LIST])
                value, pvalue = self.expr(ty, scope, 3)
                nice.append(f"{pad}print({value});")
                python.append(f"{ppad}print(show({pvalue}))")
            elif choice == 2:
                lists = [name for name, kind in scope if kind == LIST]
                if lists:
                    name = self.rng.choice(lists)
                    value, pvalue = self.expr(INT, scope, 2)
                    nice.append(f"{pad}{name}.push({value});")
                    python.append(f"{ppad}{name}.append({pvalue})")
            elif choice == 3:
                ty = self.rng.choice([INT, FLOAT, STR])
                name = self.fresh("var")
                value, pvalue = self.expr(ty, scope, 2)
                update, pupdate = self.expr(ty, scope + [(name, ty)], 2)
                nice += [f"{pad}var {name}: {ty} = {value};", f"{pad}{name} = {update};", f"{pad}print({name});"]
                python += [f"{ppad}{name} = {pvalue}", f"{ppad}{name} = {pupdate}", f"{ppad}print(show({name}))"]
            elif choice == 4:
                cond, pcond = self.expr(BOOL, scope, 2)
                then, pthen = self.block(scope, depth - 1, indent + 1)
                other, pother = self.block(scope, depth - 1, indent + 1)
                nice += [f"{pad}if {cond} {{", *then, f"{pad}}} else {{", *other, f"{pad}}}"]
                python += [f"{ppad}if {pcond}:", *pthen, f"{ppad}else:", *pother]
            elif choice == 5:
                name = self.fresh("step")
                count = self.rng.randint(0, 4)
                body, pbody = self.block(scope + [(name, INT)], depth - 1, indent + 1)
                nice += [f"{pad}for {name} in 0..{count} {{", *body, f"{pad}}}"]
                python += [f"{ppad}for {name} in range(0, {count}):", *pbody]
            else:
                name = self.fresh("loop")
                count = self.rng.randint(0, 3)
                body, pbody = self.block(scope, depth - 1, indent + 1)
                nice += [f"{pad}var {name} = 0;", f"{pad}while {name} < {count} {{", *body, f"{pad}    {name} += 1;", f"{pad}}}"]
                python += [f"{ppad}{name} = 0", f"{ppad}while {name} < {count}:", *pbody, f"{ppad}    {name} += 1"]
        if not python:
            nice.append(f"{pad}print(0);")
            python.append(f"{ppad}print(show(0))")
        return nice, python

    def build(self, cases):
        nice_parts, py_parts = [PRELUDE_NICE], [PRELUDE_PY]
        for _ in range(self.rng.randint(0, 3)):
            nice, python = self.function()
            nice_parts.append(nice)
            py_parts.append(python)
        names = []
        for _ in range(cases):
            name = self.fresh("case")
            names.append(name)
            scope = [("bump", "closure")]
            body, pbody = self.block(scope, 2, 1)
            nice_parts.append(f"fn {name}() {{\n    var counter = 0;\n    let bump = (step: Int) => {{\n        counter += step;\n        counter\n    }};\n" + "\n".join(body) + "\n}\n")
            py_parts.append(f"def {name}():\n    counter = 0\n    def bump(step):\n        nonlocal counter\n        counter = add(counter, step)\n        return counter\n" + "\n".join(pbody) + "\n")
        calls = "\n".join(f"    try {{\n        {name}();\n    }} catch (error: Panic) {{\n        print(\"panic\");\n    }}" for name in names)
        nice_parts.append(f"fn main() {{\n{calls}\n}}\n")
        py_parts.append("for case in [" + ", ".join(names) + "]:\n    try:\n        case()\n    except Panic:\n        print(\"panic\")\n")
        return "\n".join(nice_parts), "\n".join(py_parts)


def differential(rounds, rng):
    failures = 0
    with tempfile.TemporaryDirectory() as work:
        for number in range(rounds):
            nice, python = Program(rng).build(cases=rng.randint(3, 8))
            (Path(work) / "program.nice").write_text(nice)
            (Path(work) / "model.py").write_text(python)
            expected = run([PYTHON, Path(work) / "model.py"])
            actual = run([COMPILER, "run", Path(work) / "program.nice", "--python", PYTHON], timeout=120)
            problem = None
            if expected is None or expected.returncode != 0:
                problem = f"model failed:\n{expected.stderr if expected else 'timeout'}"
            elif actual is None:
                problem = "nicethon timed out"
            elif actual.stdout != expected.stdout or actual.returncode != 0:
                problem = f"expected:\n{expected.stdout}\ngot:\n{actual.stdout}\n{actual.stderr[-3000:]}"
            if problem:
                failures += 1
                folder = save("diff", number, {"program.nice": nice, "model.py": python, "problem.txt": problem})
                print(f"FAIL {number}: {problem.splitlines()[0]} -> {folder}")
            elif number % 10 == 0:
                print(f"{number} programs ok")
    return failures


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "diff"
    rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 100
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else random.randrange(1 << 30)
    print(f"{mode}: {rounds} rounds, seed {seed}")
    rng = random.Random(seed)
    failures = syntax(rounds, rng) if mode == "syntax" else differential(rounds, rng)
    print(f"{failures} failures")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
