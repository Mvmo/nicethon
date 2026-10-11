# Imports the compiled Nicethon module like any extension module.
import numpy as np

import geometry

point = geometry.Point(3.0, 4.0)
print(point)
print(geometry.norm(point))
print(geometry.scale_all([point], 2))  # python list is copied into List<Point>, int 2 is accepted as Float

try:
    geometry.norm((3.0, 4.0))
except TypeError as error:
    print("TypeError:", error)

try:
    point.x = 1.0
except AttributeError as error:
    print("AttributeError:", error)

print(hasattr(geometry, "not_exported"))

print(geometry.total(np.arange(10.0)[::3]))  # a strided numpy view becomes a List<Float>

for attempt in [
    lambda: geometry.total(np.zeros(2, np.float32)),
    lambda: geometry.scale_all([point, (1.0, 2.0)], 2),
    lambda: geometry.halve(2**64),
]:
    try:
        attempt()
    except TypeError as error:
        print("TypeError:", error)
