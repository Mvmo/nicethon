# Examples

The examples are one project: [nice.toml](nice.toml) declares numpy and
matplotlib, [nice.lock](nice.lock) pins them. The first run installs
python, zig and the packages, nothing has to be set up before.

| Example | Shows |
|---|---|
| [hello.nice](hello.nice) | the smallest program, the project's main file |
| [bank.nice](bank.nice) | enums with data, `Result` and `?`, traits |
| [iterators.nice](iterators.nice) | own iterators and adapters, lazy chains |
| [game_of_life.nice](game_of_life.nice) | structs, lists, nested loops in native speed |
| [mandelbrot.nice](mandelbrot.nice) | float loops, ASCII output, timing |
| [word_count.nice](word_count.nice) | files, `sys.argv`, maps, sorting |
| [json_config.nice](json_config.nice) | `Dyn` from `json.loads` checked into a typed struct |
| [async_tasks.nice](async_tasks.nice) | async functions under asyncio: `gather`, `wait_for`, exceptions |
| [numpy_stats.nice](numpy_stats.nice) | numpy arrays, zero-copy lists |
| [plot.nice](plot.nice) | typed computation, matplotlib for the picture |
| [backtest.nice](backtest.nice) | the use case: a path dependent trading backtest over 200,000 bars and 80 parameter sets, 30x faster than python with identical results ([backtest_compare.py](backtest_compare.py)) |
| [interop_limits.nice](interop_limits.nice) | the boundary under load: 13 scenarios against pure python, 12 edge cases, a leak check ([interop_limits_compare.py](interop_limits_compare.py)) |
| [py_library](py_library) | a Nicethon module imported from plain Python |

Run them from this directory:

```bash
nicethon run
```

```bash
nicethon run --release mandelbrot.nice
```

```bash
nicethon run word_count.nice -- README.md
```

The backtest compares itself with the same loop in pure python:

```bash
nicethon build --release backtest.nice -o build
```

```bash
PYTHONPATH=build nicethon python backtest_compare.py
```

The boundary benchmark works the same way:

```bash
nicethon build --release interop_limits.nice -o build
```

```bash
PYTHONPATH=build nicethon python interop_limits_compare.py
```

The Python library example is built once and then imported by the
project's python like any other extension module:

```bash
nicethon build py_library/geometry.nice -o build
```

```bash
PYTHONPATH=build nicethon python py_library/use_from_python.py
```
