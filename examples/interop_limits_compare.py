# Pushes the boundary between python and Nicethon in every direction. Each
# scenario runs as pure python and through interop_limits.nice; both must
# return the same result, and the table shows how long each one takes. Then
# the edge cases of the boundary are checked: wrong types, huge ints, numpy
# arrays, exceptions and panics crossing it, deep mutual recursion. At the
# end everything runs again and again, and the number of memory blocks
# python has allocated must not grow: no leaked references.
#   nicethon build --release interop_limits.nice -o build
#   PYTHONPATH=build nicethon python interop_limits_compare.py
import asyncio
import gc
import sys
import time
from dataclasses import dataclass

import numpy as np

import interop_limits as nice
from _nicethon import Panic


def longest_collatz(limit):
    best = (1, 0)
    for start in range(1, limit):
        value = start
        steps = 0
        while value != 1:
            value = value // 2 if value % 2 == 0 else 3 * value + 1
            steps += 1
        if steps > best[1]:
            best = (start, steps)
    return best


def call_python(function, count):
    total = 0
    for value in range(count):
        total += function(value)
    return total


def mix(value):
    return value * 31 % 1000


def call_each(function, count):
    total = 0
    for value in range(count):
        total += function(value)
    return total


def sort_words(words):
    return sorted(words, key=lambda word: (len(word), word))


# A plain loop on purpose: the builtin sum() uses compensated summation.
def sum_floats(values):
    total = 0.0
    for value in values:
        total += value
    return total


def numpy_std(count):
    values = [(index % 1000) * 0.001 for index in range(count)]
    return float(np.std(np.array(values)))


def dyn_sum_of_squares(values):
    total = 0
    for value in values:
        total = total + value * value
    return total


def parse_numbers(texts):
    total = failures = 0
    for text in texts:
        try:
            total += int(text)
        except ValueError:
            failures += 1
    return total, failures


def count_words(words):
    counts = {}
    for word in words:
        counts[word] = counts.get(word, 0) + 1
    return counts


def sum_iterable(items):
    total = 0
    for item in items:
        total += item
    return total


@dataclass(slots=True)
class Point:
    x: float
    y: float


def centroid(points):
    x = y = 0.0
    for point in points:
        x += point.x
        y += point.y
    return Point(x / len(points), y / len(points))


async def tick(rounds):
    for _ in range(rounds):
        await asyncio.sleep(0)
    return rounds


async def gather_ticks(tick, count):
    return sum(await asyncio.gather(*(tick(3) for _ in range(count))))


WORDS = [f"w{index * 7919 % 5000}" for index in range(1_000_000)]
TEXTS = [str(index) if index % 5 else f"x{index}" for index in range(300_000)]
FLOATS = [index * 0.25 for index in range(2_000_000)]
ARRAY = np.array(FLOATS)
INTEGERS = list(range(1_000_000))

# name, pure python, the same through Nicethon
SCENARIOS = [
    ("typed loop: longest collatz chain below 200k", lambda: longest_collatz(200_000), lambda: nice.longest_collatz(200_000)),
    ("nicethon calls a python lambda 1M times", lambda: call_python(lambda value: value % 7, 1_000_000), lambda: nice.call_python(lambda value: value % 7, 1_000_000)),
    ("python calls a nicethon function 1M times", lambda: call_each(mix, 1_000_000), lambda: call_each(nice.mix, 1_000_000)),
    ("sorted() calls a nicethon key for 200k words", lambda: sort_words(WORDS[:200_000]), lambda: nice.sort_words(WORDS[:200_000])),
    ("python list of 2M floats into a typed list", lambda: sum_floats(FLOATS), lambda: nice.sum_floats(FLOATS)),
    ("numpy array of 2M floats into a typed list", lambda: sum_floats(ARRAY.tolist()), lambda: nice.sum_floats(ARRAY)),
    ("build 2M floats, numpy reads them (no copy)", lambda: numpy_std(2_000_000), lambda: nice.numpy_std(2_000_000)),
    ("Dyn: python ints and operators, 1M squares", lambda: dyn_sum_of_squares(INTEGERS), lambda: nice.dyn_sum_of_squares(INTEGERS)),
    ("int() raising ValueError, 300k texts", lambda: parse_numbers(TEXTS), lambda: nice.parse_numbers(TEXTS)),
    ("dict counting, 1M words", lambda: count_words(WORDS), lambda: nice.count_words(WORDS)),
    ("for loop over a python generator, 1M items", lambda: sum_iterable(value * 3 for value in range(1_000_000)), lambda: nice.sum_iterable(value * 3 for value in range(1_000_000))),
    ("python creates 300k records, centroid of them", lambda: centroid([Point(index * 0.5, index * 0.25) for index in range(300_000)]), lambda: nice.centroid([nice.Point(index * 0.5, index * 0.25) for index in range(300_000)])),
    ("asyncio runs 20k coroutines, 3 awaits each", lambda: asyncio.run(gather_ticks(tick, 20_000)), lambda: asyncio.run(gather_ticks(nice.tick, 20_000))),
]


def raises(kind, function, text=""):
    try:
        function()
    except kind as error:
        return text in str(error)
    return False


def grow_while_viewed():
    values = nice.numbers(3)
    view = np.asarray(values)
    blocked = raises(BufferError, lambda: nice.grow(values))
    del view
    nice.grow(values)
    return blocked and list(values) == [0, 1, 2, 3]


def bounce(depth):
    return nice.bounce(depth, bounce)


CHECKS = [
    ("an int beyond 64 bit is rejected with a clear message", lambda: raises(TypeError, lambda: nice.mix(2**64), "int out of 64 bit range")),
    ("numpy integers are accepted as Int", lambda: nice.mix(np.int64(7)) == mix(7)),
    ("numpy float64 arrays and strided views become List<Float>", lambda: nice.sum_floats(np.arange(10.0)[::3]) == 18.0),
    ("float32 arrays are rejected, no silent conversion", lambda: raises(TypeError, lambda: nice.sum_floats(np.zeros(3, np.float32)))),
    ("a wrong list item is named in the error", lambda: raises(TypeError, lambda: nice.centroid([nice.Point(1, 2), (1.0, 2.0)]), "item 1 is tuple")),
    ("exceptions from python callbacks pass through native code", lambda: raises(ZeroDivisionError, lambda: nice.call_python(lambda value: 1 // (value - 3), 10))),
    ("a callback returning the wrong type is caught", lambda: raises(TypeError, lambda: nice.call_python(lambda value: "x", 10), "expected Int, found str")),
    ("Int overflow in typed code is a catchable panic", lambda: raises(Panic, lambda: nice.call_python(lambda value: 2**62, 10), "overflow")),
    ("records are immutable", lambda: raises(AttributeError, lambda: setattr(nice.Point(1, 2), "x", 5.0))),
    ("a List<Int> stays a List<Int> when python appends", lambda: raises(TypeError, lambda: nice.numbers(3).append("4"))),
    ("a list cannot grow while numpy views it, afterwards it can", grow_while_viewed),
    ("python and Nicethon call each other 300 levels deep", lambda: bounce(300) == 300),
]


# Nicethon lists and records into plain values python can compare.
def plain(value):
    if hasattr(value, "x") and hasattr(value, "y"):
        return (value.x, value.y)
    if isinstance(value, (int, float, str, tuple, dict)):
        return value
    return list(value)


def fastest(function, rounds=3):
    best = float("inf")
    for _ in range(rounds):
        start = time.perf_counter()
        result = function()
        best = min(best, time.perf_counter() - start)
    return result, best


def main():
    print(f"{'scenario':48} {'python':>9} {'nicethon':>9} {'speedup':>8}")
    differing = 0
    for name, python, native in SCENARIOS:
        expected, python_time = fastest(python)
        actual, native_time = fastest(native)
        same = plain(expected) == plain(actual)
        differing += not same
        mark = "" if same else "   RESULTS DIFFER"
        print(f"{name:48} {python_time:8.3f}s {native_time:8.3f}s {python_time / native_time:7.1f}x{mark}")
    print(f"\n{len(SCENARIOS) - differing} of {len(SCENARIOS)} scenarios return exactly what python returns")

    print("\nboundary behavior:")
    failed = 0
    for name, check in CHECKS:
        passed = check()
        failed += not passed
        print(f"  {'ok  ' if passed else 'FAIL'}  {name}")

    gc.collect()
    before = sys.getallocatedblocks()
    for _ in range(10):
        for _, _, native in SCENARIOS:
            native()
        for _, check in CHECKS:
            check()
    gc.collect()
    grown = sys.getallocatedblocks() - before
    print(f"\n10 more rounds of all scenarios and checks: {grown:+d} allocated memory blocks")
    sys.exit(1 if differing or failed or grown > 1000 else 0)


main()
