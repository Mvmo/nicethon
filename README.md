# Nicethon

A statically typed, C-style language on top of the Python ecosystem.

Every Python package (numpy, torch, pandas, the stdlib) can be imported and
called directly. Code written in Nicethon itself is strictly typed. Only
values that come from Python are dynamic, and they stay marked as dynamic
(type `Dyn`) until they are explicitly checked.

Nicethon compiles to C against the CPython C API. The result is a normal
CPython extension module, so typed code runs as native code while every
Python library keeps working.

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

