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

Examples are in [examples](examples), from hello world to a Nicethon
library used by plain Python.

Tests: `python3 tests/run.py` builds the compiler, runs all golden tests
(`tests/golden/`) and type checks the examples.
`python3 tests/fuzz.py diff 100` compares random programs against a Python
model of the same semantics, `python3 tests/fuzz.py syntax 300` mutates the
test programs and checks that the compiler never crashes.

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
| Lambdas | Thick arrow: `value => value * 2`, `(left, right) => left + right`, block bodies allowed. Calls from Nicethon go straight to native C, Python sees a normal callable. |
| Iterators | A type implements `Iterator<T>` with only `next`, and gets lazy `map`, `filter`, `zip`, `enumerate`, `take`, `skip`, `chain` plus `collect`, `fold`, `count`, `any`, `all`, `find`, `for_each`. `for` works on every iterator. |
| Values | `Int`, `Float` and `Bool` are plain C values (`int64_t`, `double`, `int`). `Option<T>` is unboxed too (a small struct, or a pointer that is NULL for `None`). Structs and enum variants are immutable records with native fields. |
| Generics | Monomorphized: every generic function and type gets its own C code per concrete type. `largest<Int>` compares `int64_t` values, `Option<Int>` stores an `int64_t`, trait calls on type parameters are direct C calls. |
| Python types | `lib/bindings.nice` declares types for a part of the stdlib. Other modules are `Dyn`. |
| Compiler | Rust, no dependencies. Prelude, bindings and C runtime are embedded in the binary. |

## Performance

Measured on an Apple Silicon Mac with CPython 3.14, `nicethon run --release`
against the same program in plain Python:

| Benchmark | Python | Nicethon | Speedup |
|---|---|---|---|
| Integer loop, 50M iterations | 2.84 s | 0.085 s | 33x |
| Build and sum a `List<Float>` of 5M | 0.36 s | 0.020 s | 18x |
| Recursive fibonacci(32) | 0.28 s | 0.010 s | 27x |
| Create 5M structs and use their fields | 1.73 s | 0.084 s | 21x |
| Generic `swap(Pair<A, B>)`, 5M calls | 2.67 s | 0.17 s | 16x |
| `range(0, 10M).map(..).filter(..).fold(..)` against a generator | 0.66 s | 0.094 s | 7x |

Code that mostly calls numpy or other libraries is about as fast as Python,
because the time is spent inside the library.

## How it works

```
 main.nice
     |
 lexer, parser       src/lexer.rs, src/parser.rs  (error recovery: all syntax errors at once)
     |
 type checker        src/check.rs, src/expr.rs    (bidirectional inference, exhaustiveness,
     |                                             Dyn rules, produces typed IR in src/hir.rs)
 C codegen           src/codegen.rs               (one copy per generic instance, unboxed values,
     |                                             refcounting, boundary checks)
     |
 clang / gcc  ->  main.cpython-314-darwin.so  ->  imported by CPython
```

The generated C includes [runtime/nicethon.h](runtime/nicethon.h): records,
the native list type, boundary checks, coroutines and the intrinsics of the
[prelude](lib/prelude.nice). Python tracebacks point to `.nice` lines.

## Limits

- No LSP or formatter yet.
- Python libraries are only typed through `lib/bindings.nice`; there is no
  typeshed import yet.
- A generic function that calls itself with an ever bigger type
  (`fn nest<T>(value: T) { nest([value]) }`) cannot be compiled to one copy
  per type and is rejected.
- Reference cycles through Nicethon lists are not collected.
- `build` produces one extension module, no wheel. Tested on macOS only.
- Free-threaded CPython builds are not supported.
