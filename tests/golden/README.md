# Golden tests

Each test is a small program with a fixed expected result.

Run them with `python3 tests/run.py`, or `python3 tests/run.py numpy` for
the tests whose name contains "numpy".

```
run/       must compile and print exactly what the matching .out file says;
           run/lib/ holds Nicethon modules the tests import
err/       must fail to compile. Every line marked with
               //~ error: <text>
           must produce an error on that line whose message contains <text>,
           and no other errors may appear (rustc's UI tests work like this)
interop/   one directory per test: Nicethon modules plus driver.py. The
           driver imports the compiled modules; its output must match
           expected.out
```

Fuzzing (`tests/fuzz.py`) complements these tests; regressions it found
live in `run/18_semantics.nice` and the `err/` tests e09 and e10.

The tests belong to the project in `tests/nice.toml`, which provides numpy.

Every expected output was checked by hand against the spec. Output that
comes from Python itself (numpy printing, exception messages,
functools.cache statistics) was checked against CPython 3.14.3 and
numpy 2.5.3.
