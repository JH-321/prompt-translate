#!/usr/bin/env python3
"""Offline self-check for koen's protect/restore logic (no API calls)."""
import os
from importlib.machinery import SourceFileLoader

koen = SourceFileLoader(
    "koen", os.path.join(os.path.dirname(__file__), "bin", "koen")).load_module()

# passthrough: no Hangul -> untouched, no backend call
assert koen.translate("plain english, no api call") == "plain english, no api call"

# protect/restore round-trip
src = "코드 ```py\nx=1\n``` 와 `inline` 그리고 https://a.b/c 확인"
masked, saved = koen.protect(src)
assert "```" not in masked and "https://" not in masked and "`inline`" not in masked
assert len(saved) == 3
assert koen.restore(masked, saved) == src

# lost placeholder -> hard error (triggers fallback to original upstream)
try:
    koen.restore(masked.replace("⟦K0⟧", ""), saved)
    raise AssertionError("should have raised")
except ValueError:
    pass

# fences hide their own inner inline code / URLs
masked2, saved2 = koen.protect("```\n`a` https://x.y\n```")
assert len(saved2) == 1

print("ok")
