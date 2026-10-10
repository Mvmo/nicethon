# Plain python using the compiled Nicethon module like any extension module.
#   nicethon build examples/py_library/geometry.nice -o build
#   PYTHONPATH=build python examples/py_library/use_from_python.py
import numpy as np

import geometry
from geometry import Circle, Point, Polygon

shapes = [
    Circle(Point(0, 0), 1),  # ints are accepted where Float is expected
    Polygon([Point(0, 0), Point(2, 0), Point(2, 2), Point(0, 2)]),
    Polygon([Point(0, 0), Point(4, 0), Point(0, 3)]),
]
for shape in shapes:
    print(f"{type(shape).__name__:8} area {geometry.area(shape):6.3f}  perimeter {geometry.perimeter(shape):6.3f}")

print("largest area:", geometry.largest(shapes, geometry.area))
print("fewest corners:", geometry.largest(shapes, lambda shape: -len(getattr(shape, "corners", []))))
print("no shapes:", geometry.largest([], geometry.area))

# The result is a Nicethon List<Float>; numpy wraps its memory without copying.
noisy = np.sin(np.linspace(0, 3, 12)) + np.tile([0.2, -0.2], 6)
smoothed = np.asarray(geometry.smooth(list(noisy), 3))
print("smoothed:", smoothed.round(2), smoothed.dtype)

# Wrong types fail at the boundary with a precise message.
for attempt in [
    lambda: geometry.area("circle"),
    lambda: geometry.largest(shapes, lambda shape: "big"),
    lambda: setattr(shapes[0], "radius", 2.0),
]:
    try:
        attempt()
    except (TypeError, AttributeError) as error:
        print(f"{type(error).__name__}: {error}")
