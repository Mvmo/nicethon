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

