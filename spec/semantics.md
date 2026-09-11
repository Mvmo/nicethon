# Nicethon semantics

The grammar is in [grammar.ebnf](grammar.ebnf). This file says what programs
mean and how they map to CPython. Open decisions are listed at the end.

## 1. Two worlds

Static world: everything written in Nicethon. Every expression has a type
known at compile time. No implicit conversions, no truthiness, no null.

Dynamic world: every value that comes from Python without type information
has type `Dyn`. On a `Dyn` value everything Python allows is allowed, and
the result is `Dyn` again.

One rule keeps the static world sound (types never lie at runtime):
`Dyn` never flows into a static type implicitly.

```
static -> Dyn     implicit, always allowed (boxing: wrap the value as a Python object)
Dyn -> static     only with as! or as?, checked at runtime
```

Rule of thumb: a `Dyn` behaves like TypeScript's `any` when you use it, and
like TypeScript's `unknown` when you assign it.

## 2. Syntax notes

- Every statement ends with `;`. Block-like expressions (block, `if`,
  `match`, `while`, `for`, `loop`) and `try`/`with` statements need no `;`,
  like in C and Rust.
- A block's value is its last expression if that has no `;`, otherwise `()`.
  Functions return the value of their body block; `return` is optional.
- `return`, `break`, `continue` and `throw` are expressions of type `Never`,
  so `None => return Err(error),` works inside a match.
- A match arm or a lambda body may be a single assignment:
  `Ok(value) => total += value,`.
- `//` starts a comment. Floor division is `~/` (Python's `//`).
- Keywords may be used as member names (`re.match`, `np.var`), named
  argument names (`f(type = 1)`) and import names with `as`.
- Functions declared inside a function body are local closures.

## 3. Types and their representation

| Type | Meaning | Representation in C | Seen by Python as |
|---|---|---|---|
| `Int` | 64-bit signed integer | `int64_t` | `int` |
| `BigInt` | arbitrary precision integer | Python `int` | `int` |
| `Float` | IEEE 754 double | `double` | `float` |
| `Bool` | `true` / `false` | `int` | `bool` |
| `Str` | immutable unicode text | Python `str` | the same `str` |
| `Bytes` | immutable bytes | Python `bytes` | the same `bytes` |
| `()` | unit, "no value" | nothing | `None` |
| `Never` | type of `return`, `throw`, ...; has no values | - | - |
| `(A, B)` | tuple | Python `tuple` of boxed values | `tuple` |
| struct | immutable value with named fields | record: a Python object with native fields (`double`, `int64_t`, ...) | that record |
| enum | one of several variants | each variant is its own record type, unit variants are singletons | that record |
| `List<T>` | growable array, reference type | native list object, `Int`/`Float`/`Bool` elements unboxed | the same object, by reference |
| `Map<K, V>`, `Set<T>` | hash map and set, reference types | Python `dict` and `set` | the same object |
| `Option<T>` | `Some(value)` or `None` | unboxed: `{value, tag}` struct for `Int`, `Float`, `Bool`, a pointer that is NULL for `None` otherwise; a record inside lists, fields and other containers | the value, or `None` |
| `Result<T, E>` | `Ok(value)` or `Err(error)` | prelude enum, one record type per `T`, `E` | the record |
| `(A) -> R` | function or closure | closure object with a native C entry (called directly from Nicethon) and a Python entry | a callable |
| `Coroutine<T>` | result of calling an `async fn` | coroutine object | itself |
| `PyErr` | a caught Python exception | prelude struct: `message`, `type_name`, `value` | the record |
| `Dyn` | any Python object | `PyObject *` | itself |
| py class | Python class written in Nicethon | Python class | itself |
| extern type | Python class declared in an `extern py` block | `PyObject *` | itself |

Records are immutable, so sharing one record between two variables is
not observable: structs behave like values.

## 4. Bindings and mutability

- `let` creates an immutable binding, `var` a mutable one.
- Value types (numbers, `Bool`, tuples, structs, enums, `Option`, `Result`):
  `let` also freezes the content. With `var point = ...;` the statement
  `point.x = 1.0;` builds a copy with the new field and rebinds `point`, so
  other variables holding the old value do not change.
- Reference types (`List`, `Map`, `Set`, `Dyn`, Python classes): `let` fixes
  the binding, the object itself can still be mutated (like `const` in
  TypeScript).
- `point.copy(x = 1.0)` returns a copy of a struct with fields replaced.
- Closures capture `let` bindings by value. A `var` binding captured by a
  closure lives in a shared cell, so the closure and the function see the
  same variable.

## 5. Numbers

- `Int` is 64 bit. `+ - * **` that overflow panic (checked with the compiler
  builtins `__builtin_add_overflow` and friends). Use `BigInt` for
  unbounded integers.
- `/` is always true division: `Int / Int` gives `Float`, like Python 3.
- `~/` is floor division (Python's `//`), `%` is floor modulo: the sign
  follows the divisor, `-7 % 2 == 1`, like Python. Integer division or
  modulo by zero panics.
- `Float` `~/` and `%` give exactly Python's results, signed zeros included
  (`4.0 % -2.0` is `-0.0`).
- `Float` division by zero follows IEEE 754: `1.0 / 0.0` is `inf`,
  `1.0 % 0.0` and `1.0 ~/ 0.0` are `nan`. This differs from Python, which
  raises `ZeroDivisionError`.
- `Int ** Int` needs a non-negative exponent, `Float ** Float` gives `Float`.
- No implicit conversion between `Int` and `Float`. Exception: number
  literals take the type the context expects (`let ratio: Float = 1;`).
- `as` performs conversions that are checked at compile time: `Int as Float`,
  `Float as Int` (truncates toward zero, panics on nan, inf or out of
  range), `Int as BigInt`, `BigInt as Int` (panics when out of range),
  `BigInt as Float`, and any static type `as Dyn`.
- Comparisons need both sides to have the same type: `1 == 1.0` is an error.

## 6. Strings and printing

- `"..."` with escapes `\n \t \\ \" \u{1F600}`.
- Template strings: `` `sum = ${total}` `` formats each value with `Show`.
- `Show` format, used by `print`, templates and `__repr__` of records and lists:
  - `Int`: `55`. `Float`: shortest round-trip form, as in Python (`1.0`, `0.1`).
    `Bool`: `true`, `false`. Unit fields: `()`.
  - `Str`: as is at top level, quoted `"..."` inside containers and records.
  - `List`: `[1, 2]`. Tuples: `(1, "a")`.
  - Records in constructor syntax: `Point(x = 3.0, y = 4.0)`, `Some(3)`,
    `None`, `Err(NotANumber(text = "abc"))`.
  - Everything else (`Dyn`, maps, sets, Python classes): Python's `str()`.
- `print(value)` prints `Show` plus a newline. Python's own print is
  available as `import py { print as py_print } from "builtins";`.

## 7. Functions, lambdas, pipes

- Top-level functions and methods need full signatures; a missing return
  type means `()`.
- Default values: `fn connect(host: Str, port: Int = 80)`. Named arguments
  at the call site: `connect("localhost", port = 8080)`.
- Struct and variant constructors are functions with one parameter per field:
  `Point(1.0, 2.0)` or `Point(x = 1.0, y = 2.0)`.
- Methods live in `impl` blocks with `self` first. Functions without `self`
  are called on the type: `Point.origin()`.
- Lambdas: `value => expr`, `(left, right) => expr`, `(value: Int) => { ... }`.
  Parameter types come from the expected function type. Without one, they
  must be annotated. The return type is always inferred.
- Inference is bidirectional and local: types flow from annotations and
  arguments into expressions, never across function signatures. The
  expected type also flows into calls and operators, so
  `let total: Int = -sum([]);` needs no further annotation.
- `expr as T` on a value whose type is still open acts as a type
  annotation: `[] as List<Int>`. When nothing determines a type, the
  checker asks for an annotation instead of guessing. Arguments
  that need context (lambdas without annotations) are checked after the
  other arguments, so in `reduce((left, right) => left * right, numbers)`
  the element type comes from `numbers`. TypeScript does the same.
- Pipe: `value |> function` means `function(value)`,
  `value |> function(more)` means `function(value, more)`.

## 8. Structs, enums, match

- Every struct and enum automatically gets structural `==`, `Show`, `Hash`
  and, for structs, `copy`.
- Variants are written `Shape.Circle(...)` in expressions. `Some`, `None`,
  `Ok`, `Err` are global. In patterns the enum is known from the matched
  value, so `Circle(..)` is enough.
- Patterns: `_`, literals, bindings, `Variant(sub, ...)`,
  `Variant(field = sub, ..)`, structs `Point(x, y)`, tuples `(left, right)`,
  lists `[first, second, ..]`, alternatives `left | right` (without
  bindings), guards `pattern if condition`.
- Patterns on `Dyn`: `name: Type` runs the `as?` check and binds the result;
  `None` matches Python's `None`.
- A single name in a pattern is a unit variant if the matched type has a
  variant with that name (`Empty`, `None`), otherwise a new binding.
- `match` must be exhaustive (cover every possible value), nested patterns
  included. The error names a value that no arm matches, e.g.
  `non-exhaustive match: Some(None) not covered`. A match on `Dyn` always
  needs a `_` arm.
- `if let pattern = value { }` and `while let` work as in Rust.

## 9. Generics and traits

- Generic functions and types: `fn largest<T: Ord>(items: List<T>) -> Option<T>`.
- Traits are nominal: a type implements a trait only through
  `impl Trait for Type`, also for builtin types (`impl Describe for Int`).
  Traits may have default methods.
- Bounds are checked at the call site: `largest(points)` fails to compile
  if `Point` does not implement `Ord`.
- Built-in traits: `Eq`, `Ord`, `Hash`, `Show`. Numbers, `Str`, `Bool` and
  `Bytes` implement all of them; every type implements `Eq`, `Hash` and `Show`.
- No inheritance between Nicethon types. Only `py class` inherits, and only
  from Python classes.
- Type arguments are inferred; explicit form `parse<Int>(text)`.

## 10. Errors

### 10.1 Own errors: Result and ?

`expr?` on `Result<T, E>` gives the `T`, or returns `Err(error)` from the
enclosing function, which must return `Result<_, E>` with the same `E`.
On `Option<T>` it returns `None` from a function returning `Option<_>`.

### 10.2 Panics

Panics are bugs: index out of range, `Int` overflow, `unwrap()` on `None`,
failed `Float as Int`, a call to `panic(message)`. A panic raises the
Python exception `nicethon.Panic`. It subclasses `BaseException`, not
`Exception`, so `except Exception:` blocks inside Python libraries do not
swallow it. `catch (error)` does not catch it either.

### 10.3 Python exceptions

Every call that reaches Python can throw. There is no checked-exception
tracking.

```
try {
    let config = json.loads(text);
} catch (error: ValueError | KeyError) {
    print(error.message);
} catch (error) {
    throw error;
} finally {
    cleanup();
}
```

- The filter after `:` lists Python exception classes. Without a filter,
  `catch` catches every `Exception` (not `KeyboardInterrupt`, `SystemExit`
  or `Panic`).
- `error` has type `PyErr` with `message: Str` (Python's `str(exception)`),
  `type_name: Str` and `value: Dyn` (the exception object).
- `throw expr;` takes a Python exception instance or a `PyErr` (rethrow).
- `finally` runs on every way out of the `try`: normal end, caught or
  uncaught exception, `return`, `break` and `continue`.
- `try expr` without a block evaluates `expr` and returns
  `Result<T, PyErr>`. Because `?` binds tighter, write `(try load(path))?`.

### 10.4 Messages of boundary checks

A failed check throws `TypeError`:

```
expected Int, found str                       (as!)
argument 'point': expected Point, found tuple (exported functions, lambdas called by Python)
```

Python type names are `module.qualname`, without the module for builtins:
`int`, `tuple`, `numpy.int64`.

## 11. The Python boundary

### 11.1 Imports

```
import py * as np from "numpy";                 // the module object
import py { sqrt, pi as PI } from "math";       // single names
import py * as F, { relu } from "torch.nn.functional";
import * as geometry from "./geometry";         // Nicethon module
import { Point, norm } from "./geometry";
```

- `py` imports take a dotted Python module path. Nicethon imports take a
  path relative to the importing file, without extension. All Nicethon
  modules of a program are compiled into one extension module.
- Python imports run at module initialization, like Python's `import`.
  Imported names are bound once, like `from math import pi`.

### 11.2 Where types for Python code come from

`extern py` blocks declare the types of a Python module. The compiler ships
[lib/bindings.nice](../lib/bindings.nice) for parts of the stdlib (math,
json, functools, asyncio, io, ...); a program can declare more:

```
extern py "requests" {
    fn get(url: Str, params?: Map<Str, Str>, timeout?: Float) -> Response;

    type Response {
        status_code: Int;
        text: Str;
        fn json(self) -> Dyn;
        fn raise_for_status(self);
    }
}
```

- `name?: Type` marks a parameter the caller may leave out; it is then not
  passed to Python at all.
- Generic functions (`fn reduce<T>(function: (T, T) -> T, items: List<T>) -> T;`)
  and generic types are allowed.
- A type with `fn __call__` is callable, `fn __enter__` gives the type of
  the value bound by `with`, and a decorator's return type becomes the type
  of the decorated name.
- Everything without a declaration is `Dyn`.

Python does not enforce its annotations, so return values of typed Python
calls are checked like `as!`.

### 11.3 What you can do with Dyn

| Expression | Result |
|---|---|
| `d.attr`, `d.method(x)`, `d[i]`, `d[1:, ::2]`, `d[..., 0]`, `d(x)`, `d + 1`, `d @ e` | `Dyn` |
| `d == e` | `Dyn` (numpy returns arrays here) |
| `x in d` | `Bool` |
| `d[0] = x;`, `d.attr = x;`, `d += 1;` | Python setitem, setattr, in-place operator |
| `for item in d { }` | `item: Dyn`, Python iteration protocol |
| `np.array(values)` (static into Dyn) | implicit, always allowed |
| `let count: Int = d;` | compile error |
| `if d { }`, `while d { }` | compile error, conditions must be `Bool` |
| `d as! Int` | `Int`, throws `TypeError` on mismatch |
| `d as? Int` | `Option<Int>` |
| `match d { count: Int => ..., None => ..., _ => ... }` | type tests |
| `1 as! Int` | compile error: `as!` and `as?` need a `Dyn` operand |

### 11.4 What as! accepts

| Target | Accepts |
|---|---|
| `Int` | `int` and objects with `__index__` (numpy integers); must fit in 64 bit |
| `BigInt` | the same, any size |
| `Float` | `float`, `int` and objects with `__float__` (numpy floats) |
| `Bool` | `bool`, and `numpy.bool` as a special case |
| `Str`, `Bytes` | `str`, `bytes` and their subclasses |
| `List<T>` | a Nicethon list with the same element storage: O(1). A Python `list` or `tuple`: copied, every element checked |
| `Map<K, V>`, `Set<T>` | a `dict` or `set`; the elements are not checked |
| `(A, B)` | a tuple of the same length, every element checked |
| `Option<T>` | `None` gives `None`, anything else is checked as `T` and wrapped in `Some` |
| struct, enum | exactly that Nicethon type |
| `(A) -> R` | any callable; the result is checked on every call |
| py class, extern type | not checked |

### 11.5 Calling Python

- Positional arguments, named arguments (`sep = ";"`), `*items` and
  `**options` spread like in Python.
- Static arguments are boxed. Lists, maps and sets go by reference.
  `Option` arguments become the value or `None`.
- Operators on `Dyn` use Python's operator protocols (`~/` is Python's `//`).
  If one side is static, it is boxed first.

### 11.6 Python calling Nicethon

- Only `export`ed items (and `main`) are visible to Python.
- Every function has a wrapper for Python that checks each argument like
  `as!`, fills in default values, calls the native function and boxes the
  result. Calls from Nicethon go straight to the native function without
  checks. PyO3's `#[pyfunction]` works the same way.
- Exported structs and enum variants are Python types with read-only
  attributes, a checking constructor, `__eq__`, `__hash__` and `__repr__`.
  Assigning to a field raises
  `AttributeError: cannot assign to field 'x' of immutable struct Point`.
- Nicethon lists implement Python's sequence protocol (`len`, indexing,
  iteration, `in`, `append`). Writes from Python are checked against the
  element type, so a `List<Int>` stays a `List<Int>`. Lists of `Int` and
  `Float` also implement the buffer protocol (raw access to the memory), so
  `np.asarray(values)` wraps them without copying and writes are shared.
  While such a view exists the list cannot grow; `push` throws `BufferError`,
  like Python's `bytearray`.
- Closures handed to Python are callables. Unannotated parameters are `Dyn`,
  annotated ones are checked on every call.

### 11.7 py class

```
py class Net : nn.Module {
    layer: Dyn;

    fn __init__(self) {
        super.__init__();
        self.layer = nn.Linear(784, 10);
    }

    fn forward(self, input: Dyn) -> Dyn {
        self.layer(input) |> F.relu
    }
}
```

- Creates a real Python class with the given Python bases. Needed for APIs
  built on inheritance (torch, unittest, html.parser, Django).
- `self` has the class type: declared fields and methods are typed,
  attributes of an extern base type are typed too, others are `Dyn`.
  Reading a declared field checks its type, because Python may have
  replaced it. `super` is Python's `super()`.
- Python calls the methods through their checking wrappers.

### 11.8 Decorators

`@decorator` before a function passes the compiled function to the
decorator at module initialization and binds the name to the result, like
Python. The result type comes from the decorator's declaration in an
`extern py` block (`functools.cache` keeps the call signature), otherwise it
is `Dyn`. Calls of a decorated function always go through Python,
recursive ones included.

### 11.9 with and async

- `with expr as name { }` uses Python's context manager protocol
  (`__enter__`, `__exit__`); exceptions are passed to `__exit__`, and
  `__exit__` also runs on `return`, `break` and `continue`.
- Calling an `async fn` returns a `Coroutine<T>` that implements Python's
  coroutine protocol (`send`, `throw`, `close`, `__await__`), so
  `asyncio.run` and friends accept it. `await` works on `Coroutine<T>`
  (gives `T`) and on any `Dyn` awaitable (gives `Dyn`). Throwing into a
  suspended Nicethon coroutine ends it with that exception.

## 12. Native backend

- A program (the entry file plus the Nicethon modules it imports) compiles
  to one C file and one CPython extension module, e.g.
  `geometry.cpython-314-darwin.so`. The module name is the entry file name.
- The Python interpreter is a build input: the active virtualenv's python,
  or `--python`. Include path and extension suffix come from its `sysconfig`.
- `nicethon run main.nice` compiles with `-O1`, starts that interpreter,
  imports the module and calls `main()`. `--release` uses `-O2`.
- Target: CPython 3.12 and newer, full C API.
- Code generation starts at the non-generic functions and creates a copy of
  a generic function or type the first time it is used with new concrete
  types.
- Refcounting: every temporary owns its reference and is released at the
  end of its statement. Variables own their values. On an error the code
  jumps to the function's error label, which releases everything still held,
  adds a traceback entry and returns the error marker (NULL for objects, -1
  for numbers, together with a set Python error).
- Tracebacks: every function adds an entry with its `.nice` file and line,
  so Python tracebacks show `File "main.nice", line 12, in area` and the
  source line.

Typed code becomes plain C without Python API calls in the hot path. This
is the generated code for `area` from the enums test, with readable names
in place of the numbered ones:

```
fn area(shape: Shape) -> Float {
    match shape {
        Circle(_, radius) => PI * radius * radius,
        Rect(min, max) => (max.x - min.x) * (max.y - min.y),
        Empty => 0.0,
    }
}
```

```c
typedef struct { PyObject_HEAD double x; double y; } Point;
typedef struct { PyObject_HEAD PyObject *center; double radius; } Circle;
typedef struct { PyObject_HEAD PyObject *min; PyObject *max; } Rect;

static double area(PyObject *shape) {
    double result;
    if (Py_IS_TYPE(shape, &CircleType)) {
        double radius = ((Circle *)shape)->radius;
        result = (PI * radius) * radius;
        goto match_end;
    }
    if (Py_IS_TYPE(shape, &RectType)) {
        PyObject *min = ((Rect *)shape)->min, *max = ((Rect *)shape)->max;
        result = (((Point *)max)->x - ((Point *)min)->x) * (((Point *)max)->y - ((Point *)min)->y);
        goto match_end;
    }
    if (Py_IS_TYPE(shape, &EmptyType)) {
        result = 0.0;
        goto match_end;
    }
    ...
match_end:
    return result;
}
```

`Dyn` code becomes C API calls with error checks and refcounting:

```
let total = grid.sum() as! Int;
```

```c
t1 = PyObject_GetAttr(grid, K_sum);
if (!t1) { nt_lineno = 12; goto error; }
t2 = PyObject_Vectorcall(t1, NULL, 0, NULL);
if (!t2) { nt_lineno = 12; goto error; }
t3 = nt_from_py(t2, "i", "Int", "");      /* __index__, 64-bit range check */
if (!t3) { nt_lineno = 12; goto error; }
total = PyLong_AsLongLong(t3);
```

## 13. Runtime

[runtime/nicethon.h](../runtime/nicethon.h) is included into every generated
C file: records, the list type, boundary checks (`nt_from_py` with a small
type description string), the coroutine type and the intrinsics declared
in the prelude. The `Panic` exception type lives in a module `_nicethon`
in `sys.modules`, so several Nicethon extension modules in one process share
it (`import py { Panic } from "_nicethon";` to catch panics).

## 14. Open questions

1. Typed third-party packages: import typeshed and inline annotations
   (numpy, pandas-stubs) instead of hand-written `extern py` blocks.
2. Tuples are Python tuples with boxed items, so `zip` and `enumerate`
   allocate per item. Unboxed tuples like unboxed options.
3. Operator overloading through traits (`Add`, `Index`, ...).
4. Error conversion for `?` (Rust's `From`), or is `map_err` enough?
5. Mutating methods on structs (Swift's `mutating`), or is `copy` enough?
6. Trait objects (dynamic dispatch as a type, Rust's `dyn Trait`). The name
   must not clash with `Dyn`.
7. Generators (`yield`) written in Nicethon.
8. A general rule for `as! Bool` instead of the `numpy.bool` special case.
9. Return type annotations on lambdas.
10. `Int` overflow: panic (current) or automatic promotion to `BigInt`?
11. Garbage collection for reference cycles through Nicethon lists.
12. Free-threaded CPython (no GIL), Windows, wheels, LSP, formatter.
