# Nicethon

A statically typed, C-style language on top of the Python ecosystem.

Every Python package (numpy, torch, pandas, the stdlib) can be imported and
called directly. Code written in Nicethon itself is strictly typed. Only
values that come from Python are dynamic, and they stay marked as dynamic
(type `Dyn`) until they are explicitly checked.

Nicethon compiles to C against the CPython C API. The result is a normal
CPython extension module, so typed code runs as native code while every
Python library keeps working.

## Setup

Needs Rust (cargo), a C compiler (clang or gcc) and CPython 3.12 or newer
with its headers.

```bash
cargo build --release
```

```bash
./target/release/nicethon run hello.nice
```

`cargo install --path .` puts `nicethon` on the PATH.

| Command | Does |
|---|---|
| `nicethon check file.nice` | type check only |
| `nicethon run file.nice` | compile, then run `main()` |
| `nicethon build file.nice -o build` | write `build/file.c` and the extension module `build/file.cpython-*.so` |

Flags: `--release` compiles the C code with `-O2` instead of `-O1`.
Arguments after `--` go to the program (`sys.argv`). `CC` picks the C
compiler (default `cc`).
`--python path` picks the interpreter. Without it, the active virtualenv
(`$VIRTUAL_ENV`) is used, otherwise `python3`. The packages installed for
that interpreter are the ones a program can import.

Tests: `python3 tests/run.py` runs all golden tests (`tests/golden/`).

## Taste

```
import py * as np from "numpy";
import py { pi } from "math";

struct Point {
    x: Float;
    y: Float;
}

enum Shape {
    Circle(center: Point, radius: Float),
    Rect(min: Point, max: Point),
}

fn area(shape: Shape) -> Float {
    match shape {
        Circle(_, radius) => pi * radius * radius,
        Rect(min, max) => (max.x - min.x) * (max.y - min.y),
    }
}

fn main() {
    let origin = Point(0.0, 0.0);
    let shapes = [Shape.Circle(origin, 1.0), Shape.Rect(origin, Point(2.0, 3.0))];

    let total = shapes
        |> map(area)
        |> filter(value => value > 1.0)
        |> sum;
    print(`total area: ${total}`);

    let grid = np.arange(12).reshape(3, 4);     // grid: Dyn
    let count = grid.sum() as! Int;             // explicit, checked at runtime
    let wrong: Int = grid.sum();                // compile error: expected Int, found Dyn
}
```

The language is described in [spec/semantics.md](spec/semantics.md) and
[spec/grammar.ebnf](spec/grammar.ebnf). The programs in
[tests/golden/run](tests/golden/run) show every feature with its expected
output.

## Decisions

| Topic | Decision |
|---|---|
| Backend | C against the CPython C API, compiled to an extension module. Runs inside a normal CPython process. |
| Syntax | Braces, `;` after every statement. The last expression of a block without `;` is the block's value (like Rust). |
| Imports | TypeScript style: `import py * as np from "numpy";`, `import py { sqrt, pi as PI } from "math";`, `import { Point } from "./geometry";` |
| Python values | Type `Dyn`. Usable like in Python, never implicitly converted to a static type. `as!` checks and throws `TypeError`, `as?` checks and returns `Option`. |
| Errors | Own code: `Result<T, E>` and `?`. Python exceptions: `try { } catch (error: ValueError) { } finally { }`, `throw`, and `try expr` which gives `Result<T, PyErr>`. |
| Containers at the boundary | Passed by reference. Nicethon lists are native objects. Python sees them as sequences, and numpy wraps `List<Int>` and `List<Float>` without copying (buffer protocol). |
| Lambdas | Thick arrow: `value => value * 2`, `(left, right) => left + right`, block bodies allowed. Python sees a normal callable. |
| Values | `Int`, `Float` and `Bool` are plain C values (`int64_t`, `double`, `int`). Structs and enum variants are immutable records with native fields. |
| Generics | Monomorphized: every generic function and type gets its own C code per concrete type. `largest<Int>` compares `int64_t` values, `Option<Int>` stores an `int64_t`, trait calls on type parameters are direct C calls. |
| Python types | `lib/bindings.nice` declares types for a part of the stdlib. Other modules are `Dyn`. |
| Compiler | Rust, no dependencies. Prelude, bindings and C runtime are embedded in the binary. |
