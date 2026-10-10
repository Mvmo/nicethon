# Examples

Run from the repository root, after `cargo build --release`
(or with `nicethon` on the PATH after `cargo install --path .`).

| Example | Shows | Needs |
|---|---|---|
| [hello.nice](hello.nice) | the smallest program | |
| [bank.nice](bank.nice) | enums with data, `Result` and `?`, traits | |
| [iterators.nice](iterators.nice) | own iterators and adapters, lazy chains | |
| [game_of_life.nice](game_of_life.nice) | structs, lists, nested loops in native speed | |
| [mandelbrot.nice](mandelbrot.nice) | float loops, ASCII output, timing | |
| [word_count.nice](word_count.nice) | files, `sys.argv`, maps, sorting | |
| [json_config.nice](json_config.nice) | `Dyn` from `json.loads` checked into a typed struct | |
| [async_tasks.nice](async_tasks.nice) | async functions under asyncio: `gather`, `wait_for`, exceptions | |
| [numpy_stats.nice](numpy_stats.nice) | numpy arrays, zero-copy lists | numpy |
| [plot.nice](plot.nice) | typed computation, matplotlib for the picture | numpy, matplotlib |
| [py_library](py_library) | a Nicethon module imported from plain Python | numpy |

```bash
nicethon run examples/bank.nice
```

```bash
nicethon run --release examples/mandelbrot.nice
```

```bash
nicethon run examples/word_count.nice -- README.md
```

The Python library example is built once and then imported like any other
extension module:

```bash
nicethon build examples/py_library/geometry.nice -o build
```

```bash
PYTHONPATH=build python3 examples/py_library/use_from_python.py
```
