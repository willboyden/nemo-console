"""Golden for services::pyfmt::float_repr: CPython `repr(float)` of 9000+ doubles (random bit patterns, large
values with binary fractions that are exact decimal ties, and known edge cases such as -1133092563735992.25, whose
shortest repr needs round-half-to-even). One line per value: `<16 hex digits of the IEEE bits> <repr>`.

    REF=$(node parity/ref.mjs) && "$REF/.venv/bin/python" -I parity/gen/gen_float_repr_golden.py > backend/crates/nemo-console/tests/fixtures/pyfmt/float_repr.txt
"""
import math
import random
import struct

vals = [-1133092563735992.25, 1133092563735992.25, 0.5, 2.5, 1e16 + 2, 9007199254740993.0, 123456789012345.675, 5e-324,
        1.7976931348623157e308, 0.1 + 0.2, 1 / 3, 2.0 ** 53, 4503599627370497.5, 1e-5, 1e-4, 1e15, 1e16, 0.0, -0.0]
random.seed(7)
for _ in range(3000):
    vals.append(struct.unpack("d", struct.pack("Q", random.getrandbits(64)))[0])
    vals.append(random.uniform(-1e17, 1e17))
    vals.append(float(random.randint(-10**17, 10**17)) + random.choice([0.25, 0.5, 0.75, 0.125]))
for v in vals:
    if math.isfinite(v):
        print(f'{struct.unpack("Q", struct.pack("d", v))[0]:016x} {v!r}')
