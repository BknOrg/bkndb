"""Only exists to force a platform-specific, ABI-agnostic wheel tag
(`py3-none-<platform>`, e.g. `py3-none-win_amd64`).

bkndb's native layer is loaded via `ctypes` at runtime (UniFFI's Python
bindings style), not through the CPython C-extension ABI — so one wheel per
OS/architecture works unmodified across every Python 3.9+ version, with no
`cp39`/`cp310`/... build matrix needed.

Two overrides are required to get there, not one:

- `BinaryDistribution.has_ext_modules() -> True` tells setuptools this is
  *not* a pure-Python package, so `bdist_wheel` picks a platform tag
  (`win_amd64`, `manylinux_2_17_x86_64`, ...) instead of `any`.
- On its own, that still isn't enough: `bdist_wheel` then assumes an
  *actual* compiled CPython extension matching the interpreter that ran the
  build, and stamps a `cpXY`/`cpXY` python+abi tag (e.g. `cp312-cp312`) —
  which would incorrectly pin the wheel to one Python version. Overriding
  `bdist_wheel.get_tag()` to force the python/abi pair to `py3`/`none`
  (keeping only the platform part) is what actually makes the wheel
  version-agnostic; there's nothing to compile here in the first place —
  `scripts/build_native.py` already placed the prebuilt library before this
  runs.

Everything else about the build is declared in `pyproject.toml`.
"""
from setuptools import setup
from setuptools.dist import Distribution

try:
    from wheel.bdist_wheel import bdist_wheel as _bdist_wheel

    class bdist_wheel(_bdist_wheel):  # noqa: N801
        def get_tag(self):
            _python, _abi, plat = super().get_tag()
            return "py3", "none", plat

    cmdclass = {"bdist_wheel": bdist_wheel}
except ImportError:
    # `wheel` isn't installed — `python -m build` always installs it as a
    # build dependency, so this only matters for `python setup.py --help`
    # and similar metadata-only invocations that never actually build one.
    cmdclass = {}


class BinaryDistribution(Distribution):
    def has_ext_modules(self) -> bool:  # noqa: D102
        return True


setup(distclass=BinaryDistribution, cmdclass=cmdclass)
