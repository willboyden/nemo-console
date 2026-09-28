"""Shape of a metric key as accepted by the history endpoint (`sy.tokens` or `sy.tokens{kind=prompt}`)."""
import re

KEY_RE = re.compile(r"[a-z][a-z0-9_.]{0,63}(?:\{[A-Za-z0-9_.:/\-=,]{1,200}\})?")
