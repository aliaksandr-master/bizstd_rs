"""The stubs describe the module that actually got built.

Hand-written stubs drift. A stub that has drifted is worse than no stub at all:
the type checker agrees, confidently, with code that will fail at runtime. This
compares the two and fails on the difference, so the drift is caught by the
test suite rather than by whoever trusted the annotation.

What is compared is the set of public names and, for classes, the set of public
attributes. Signatures are not compared — that would need a stub parser, and
the names going missing is the failure that actually happens when a binding
gains or loses a method.
"""

from __future__ import annotations

import ast
import inspect
from pathlib import Path
from typing import Any

import bizstd_binary
from bizstd_binary import _native

# Not `with_suffix`: the extension is `_native.abi3.so`, and replacing the last
# suffix alone would look for `_native.abi3.pyi`.
STUB = Path(_native.__file__).parent / "_native.pyi"


def stub_tree() -> ast.Module:
    assert STUB.exists(), f"the stubs are missing at {STUB}; they ship with the wheel"
    return ast.parse(STUB.read_text(encoding="utf-8"))


def public(names: object) -> set[str]:
    return {name for name in dir(names) if not name.startswith("_")}


def stub_top_level() -> set[str]:
    found: set[str] = set()
    for node in stub_tree().body:
        if isinstance(node, (ast.ClassDef, ast.FunctionDef)):
            found.add(node.name)
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            found.add(node.target.id)
    return {name for name in found if not name.startswith("_")}


def stub_class_members(class_name: str) -> set[str]:
    for node in stub_tree().body:
        if isinstance(node, ast.ClassDef) and node.name == class_name:
            members: set[str] = set()
            for item in node.body:
                if isinstance(item, ast.FunctionDef):
                    members.add(item.name)
                elif isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name):
                    members.add(item.target.id)
            return {name for name in members if not name.startswith("_")}
    raise AssertionError(f"{class_name} is not in the stubs")


def test_the_stub_file_ships_with_the_wheel() -> None:
    assert STUB.exists(), "the .pyi must ship inside the installed package"
    assert (Path(bizstd_binary.__file__).parent / "py.typed").exists(), "PEP 561 marker missing"


def test_every_public_name_is_in_the_stubs() -> None:
    real = public(_native)
    stubbed = stub_top_level()
    missing = real - stubbed
    assert not missing, f"in the module but not in the stubs: {sorted(missing)}"


def test_the_stubs_invent_nothing() -> None:
    real = public(_native)
    stubbed = stub_top_level()
    invented = stubbed - real
    assert not invented, f"in the stubs but not in the module: {sorted(invented)}"


def test_class_members_agree() -> None:
    classes = [
        name
        for name in public(_native)
        if inspect.isclass(getattr(_native, name))
        and not issubclass(getattr(_native, name), BaseException)
    ]
    assert classes, "no classes found; the module did not load as expected"
    for class_name in classes:
        real_class: Any = getattr(_native, class_name)
        real = public(real_class)
        stubbed = stub_class_members(class_name)
        missing = sorted(real - stubbed)
        invented = sorted(stubbed - real)
        assert not missing, f"{class_name}: in the class, not in the stubs: {missing}"
        assert not invented, f"{class_name}: in the stubs, not in the class: {invented}"


def test_the_two_packages_agree_on_what_they_export() -> None:
    import bizstd

    shared = set(bizstd_binary.__all__) & set(bizstd.__all__)
    for name in shared:
        here = getattr(bizstd, name)
        there = getattr(bizstd_binary, name)
        # Scalars are compared by value: two equal strings need not be the same
        # object, and requiring that would be a test of CPython's interning.
        if isinstance(here, (str, int, float)):
            assert here == there, f"{name} disagrees between the packages"
        else:
            assert here is there, f"{name} is re-exported but is not the same object"
