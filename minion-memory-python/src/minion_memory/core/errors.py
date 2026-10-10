"""Stable validation errors of the Layer 01 contract (SER-008 error map, G-031)."""

from __future__ import annotations


class MnemeValidationError(ValueError):
    """A contract rejection carrying one stable error code (SER-008 stage table).

    `argument` names the primitive argument that failed when a primitive takes several (G-034:
    arguments in declared order).
    """

    def __init__(self, code: str, detail: str = "", argument: str | None = None) -> None:
        super().__init__(f"{code}: {detail}" if detail else code)
        self.code = code
        self.detail = detail
        self.argument = argument

    def for_argument(self, argument: str) -> MnemeValidationError:
        return MnemeValidationError(self.code, self.detail, argument)


_LIMIT = 40


def describe(value: object) -> str:
    """A bounded, non-recursive description of a rejected value for diagnostics (WP01-PY-004).

    Only ever called with exact built-ins or decoder tokens (native input is extracted first, G-042):
    never `repr` of a container, since deep containers would recurse, and huge integers exceed the
    host's int-to-string limit. Building a diagnostic must never replace the stable error code.
    """
    if value is None or isinstance(value, bool):
        return repr(value)
    if isinstance(value, int):
        return f"integer of {value.bit_length()} bits" if value.bit_length() > 64 else str(value)
    if isinstance(value, float):
        return repr(value)
    if isinstance(value, str):
        return repr(value[:_LIMIT]) + (f"... ({len(value)} characters)" if len(value) > _LIMIT else "")
    assert isinstance(value, (list, dict)), "describe() receives exact built-ins only"
    return f"{type(value).__name__} of {len(value)} items"
