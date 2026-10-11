# Runs the backtest of backtest.nice in pure python and through the compiled
# Nicethon module, checks that both give exactly the same numbers and
# compares the time.
#   nicethon build backtest.nice -o build
#   PYTHONPATH=build nicethon python backtest_compare.py
import time

import backtest

FEE = 0.0005


def simulate(prices, fast, slow, stop):
    fast_sum = slow_sum = 0.0
    was_above = True
    entry = None
    peak = 0.0
    cash = best = 1.0
    max_drawdown = 0.0
    trades = 0
    for index, price in enumerate(prices):
        fast_sum += price
        slow_sum += price
        if index >= fast:
            fast_sum -= prices[index - fast]
        if index >= slow:
            slow_sum -= prices[index - slow]
        if index + 1 >= slow:
            above = fast_sum / fast > slow_sum / slow
            if entry is None:
                if above and not was_above:
                    entry = price
                    peak = price
                    cash *= 1.0 - FEE
                    trades += 1
            else:
                peak = max(peak, price)
                if price < peak * (1.0 - stop) or not above:
                    cash *= price / entry * (1.0 - FEE)
                    entry = None
            was_above = above
        equity = cash * price / entry if entry is not None else cash
        best = max(best, equity)
        max_drawdown = max(max_drawdown, 1.0 - equity / best)
    if entry is not None:
        cash *= prices[-1] / entry * (1.0 - FEE)
    return cash - 1.0, max_drawdown, trades


prices = backtest.prices(200_000, 7)
grid = backtest.grid()

start = time.perf_counter()
native = [backtest.backtest(prices, params) for params in grid]
native_time = time.perf_counter() - start

python_prices = list(prices)
start = time.perf_counter()
reference = [simulate(python_prices, params.fast, params.slow, params.stop) for params in grid]
python_time = time.perf_counter() - start

differing = [params for params, outcome, expected in zip(grid, native, reference)
             if (outcome.total_return, outcome.max_drawdown, outcome.trades) != expected]
print(f"{len(grid)} strategies on {len(prices)} bars")
print(f"python:   {python_time:7.3f} s")
print(f"nicethon: {native_time:7.3f} s, {python_time / native_time:.0f}x faster")
print(f"{len(differing)} of {len(grid)} results differ" if differing else "all results identical to the last bit")
