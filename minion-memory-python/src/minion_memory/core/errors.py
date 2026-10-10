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
