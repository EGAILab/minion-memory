"""Portable value domain, canonical bytes and content hash (SER-008, SER-009).

Validation runs in the three stages of SER-008 (G-031): stage 1 decodes JSON text (source order) or
checks native kinds; stage 2 checks the generic value domain; typed field rules (stage 3) live with
their types. Stages 2 and 3 traverse in one canonical order (G-034): depth-first, object members by
UTF-16 code units, a member's name before its value, arrays by ascending index.

A validated value is *normalized*: one numeric kind (G-023), so every integral number is an `int`
within +/-(2^53-1) (G-016, G-025) and every other number a finite non-integral `float`; -0 is 0
(G-017). Equal canonical bytes therefore mean equal normalized values.
"""

from __future__ import annotations

import hashlib
import math
from collections.abc import Iterator
from decimal import Decimal

from .errors import MnemeValidationError

type Value = bool | int | float | str | list[Value] | dict[str, Value] | None

SAFE_INTEGER_MAX = 2**53 - 1


class _Numeral(str):
    """A JSON number token kept as text until stage 2 interprets it (SER-008 number interpretation)."""


# ----------------------------------------------------------------------------------------- stage 1


_WHITESPACE = " \t\n\r"
_ESCAPES = {'"': '"', "\\": "\\", "/": "/", "b": "\b", "f": "\f", "n": "\n", "r": "\r", "t": "\t"}
_HEX = "0123456789abcdefABCDEF"


class _Parser:
    """A strict RFC 8259 parser reporting the first stage-1 violation in source order.

    Iterative (no recursion limit). Duplicate member names are detected after escape decoding, at the
    second occurrence, before any map could discard them; an escape yielding an unpaired surrogate is
    `value.lone_surrogate`; every other deviation from RFC 8259 is `value.invalid_json`.
    """

    def __init__(self, text: str) -> None:
        self.text = text
        self.pos = 0

    def fail(self, code: str, detail: str) -> MnemeValidationError:
        return MnemeValidationError(code, f"{detail} at offset {self.pos}")

    def skip_ws(self) -> None:
        text, pos = self.text, self.pos
        while pos < len(text) and text[pos] in _WHITESPACE:
            pos += 1
        self.pos = pos

    def parse(self) -> object:
        self.skip_ws()
        # Explicit stack of open containers: (container, pending member name or None).
        stack: list[tuple[list[object] | dict[str, object], str | None]] = []
        result: object = None
        while True:
            value, opened = self.parse_value_start()
            if opened is not None:
                stack.append((opened, None))
                if self.close_empty(opened):
                    value = stack.pop()[0]
                else:
                    if isinstance(opened, dict):
                        stack[-1] = (opened, self.parse_member_name(opened))
                    continue
            # Attach the completed value and close finished containers.
            while True:
                if not stack:
                    result = value
                    self.skip_ws()
                    if self.pos != len(self.text):
                        raise self.fail("value.invalid_json", "trailing content")
                    return result
                container, name = stack[-1]
                if isinstance(container, list):
                    container.append(value)
                else:
                    assert name is not None
                    container[name] = value
                self.skip_ws()
                closer = "]" if isinstance(container, list) else "}"
                char = self.peek()
                if char == ",":
                    self.pos += 1
                    self.skip_ws()
                    if isinstance(container, dict):
                        stack[-1] = (container, self.parse_member_name(container))
                    break
                if char == closer:
                    self.pos += 1
                    value = stack.pop()[0]
                    continue
                raise self.fail("value.invalid_json", f"expected ',' or {closer!r}")

    def peek(self) -> str:
        return self.text[self.pos] if self.pos < len(self.text) else ""

    def close_empty(self, opened: list[object] | dict[str, object]) -> bool:
        self.skip_ws()
        closer = "]" if isinstance(opened, list) else "}"
        if self.peek() == closer:
            self.pos += 1
            return True
        return False

    def parse_member_name(self, container: dict[str, object]) -> str:
        if self.peek() != '"':
            raise self.fail("value.invalid_json", "expected a member name")
        start = self.pos
        name = self.parse_string()
        if name in container:
            self.pos = start
            raise self.fail("value.duplicate_key", f"duplicate member name {name!r}")
        self.skip_ws()
        if self.peek() != ":":
            raise self.fail("value.invalid_json", "expected ':'")
        self.pos += 1
        self.skip_ws()
        return name

    def parse_value_start(self) -> tuple[object, list[object] | dict[str, object] | None]:
        char = self.peek()
        if char == "{":
            self.pos += 1
            return None, {}
        if char == "[":
            self.pos += 1
            return None, []
        if char == '"':
            return self.parse_string(), None
        if char == "-" or (char.isascii() and char.isdigit()):
            return self.parse_number(), None
        for literal, value in (("true", True), ("false", False), ("null", None)):
            if self.text.startswith(literal, self.pos):
                self.pos += len(literal)
                return value, None
        raise self.fail("value.invalid_json", "expected a JSON value")

    def parse_number(self) -> _Numeral:
        text, start = self.text, self.pos
        pos = start
        if pos < len(text) and text[pos] == "-":
            pos += 1

        def digits(at: int) -> int:
            end = at
            while end < len(text) and "0" <= text[end] <= "9":
                end += 1
            return end

        if pos < len(text) and text[pos] == "0":
            pos += 1
        elif pos < len(text) and "1" <= text[pos] <= "9":
            pos = digits(pos)
        else:
            self.pos = pos
            raise self.fail("value.invalid_json", "malformed number")
        if pos < len(text) and text[pos] == ".":
            end = digits(pos + 1)
            if end == pos + 1:
                self.pos = end
                raise self.fail("value.invalid_json", "malformed fraction")
            pos = end
        if pos < len(text) and text[pos] in "eE":
            pos += 1
            if pos < len(text) and text[pos] in "+-":
                pos += 1
            end = digits(pos)
            if end == pos:
                self.pos = end
                raise self.fail("value.invalid_json", "malformed exponent")
            pos = end
        if pos < len(text) and "0" <= text[pos] <= "9":  # e.g. "01": a leading zero
            self.pos = pos
            raise self.fail("value.invalid_json", "leading zero")
        self.pos = pos
        return _Numeral(text[start:pos])

    def parse_string(self) -> str:
        text = self.text
        self.pos += 1  # opening quote
        parts: list[str] = []
        while True:
            if self.pos >= len(text):
                raise self.fail("value.invalid_json", "unterminated string")
            char = text[self.pos]
            if char == '"':
                self.pos += 1
                return "".join(parts)
            if char < " ":
                raise self.fail("value.invalid_json", "unescaped control character")
            if char != "\\":
                parts.append(char)
                self.pos += 1
                continue
            escape = text[self.pos + 1 : self.pos + 2]
            if escape in _ESCAPES:
                parts.append(_ESCAPES[escape])
                self.pos += 2
                continue
            if escape != "u":
                raise self.fail("value.invalid_json", "invalid escape")
            unit = self.hex4(self.pos + 2)
            if 0xDC00 <= unit <= 0xDFFF:
                raise self.fail("value.lone_surrogate", "unpaired low surrogate escape")
            if 0xD800 <= unit <= 0xDBFF:
                low = self.hex4(self.pos + 8) if text[self.pos + 6 : self.pos + 8] == "\\u" else None
                if low is None or not 0xDC00 <= low <= 0xDFFF:
                    raise self.fail("value.lone_surrogate", "unpaired high surrogate escape")
                parts.append(chr(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)))
                self.pos += 12
                continue
            parts.append(chr(unit))
            self.pos += 6

    def hex4(self, at: int) -> int:
        chunk = self.text[at : at + 4]
        if len(chunk) != 4 or any(c not in _HEX for c in chunk):
            self.pos = at
            raise self.fail("value.invalid_json", "invalid \\u escape")
        return int(chunk, 16)


def _decode_text(text: str | bytes) -> object:
    if isinstance(text, bytes):
        try:
            text = text.decode("utf-8")
        except UnicodeDecodeError as error:
            raise MnemeValidationError("value.invalid_json", f"not UTF-8 ({error})") from None
    # A raw surrogate code point cannot come from UTF-8 text (SER-008: raw surrogate bytes are
    # invalid UTF-8), so it is a stage-1 text error, not an escape.
    for offset, char in enumerate(text):
        if "\ud800" <= char <= "\udfff":
            raise MnemeValidationError("value.invalid_json", f"raw surrogate at offset {offset}")
    return _Parser(text).parse()


def _check_native_kinds(value: object) -> None:
    """Stage 1 for native values: only JSON data-model kinds (G-034 canonical traversal)."""
    for item in _canonical_walk(value, native=True):
        if isinstance(item, dict):
            for name in item:
                if not isinstance(name, str):
                    raise MnemeValidationError(
                        "value.unsupported_type", f"member name {name!r} is not a string"
                    )
        elif not (item is None or isinstance(item, (bool, int, float, str, list))):
            raise MnemeValidationError("value.unsupported_type", f"{type(item).__name__} is not a JSON value")


# ----------------------------------------------------------------------------------------- stage 2


def _utf16_key(name: str) -> bytes:
    """Sort key ordering member names by UTF-16 code units (SER-009, G-034)."""
    return name.encode("utf-16-be", errors="surrogatepass")


def _canonical_walk(value: object, *, native: bool = False) -> Iterator[object]:
    """Depth-first pre-order in canonical order; a member name is yielded before its value."""
    stack: list[object] = [value]
    while stack:
        item = stack.pop()
        yield item
        if isinstance(item, list):
            stack.extend(reversed(item))
        elif isinstance(item, dict):
            if native and not all(isinstance(name, str) for name in item):
                continue  # reported by the stage-1 native check before ordering is possible
            members = sorted(item.items(), key=lambda kv: _utf16_key(kv[0]))
            for name, member in reversed(members):
                stack.append(member)
                stack.append(_Name(name))


class _Name(str):
    """A member name during traversal (checked as a string, never as a value)."""


def _has_surrogate(text: str) -> bool:
    return any("\ud800" <= char <= "\udfff" for char in text)


def _number(value: float | int | _Numeral) -> int | float:
    """Interpret one number in the single numeric kind; raise its stage-2 error."""
    if isinstance(value, _Numeral):
        # The binary64 value nearest the numeral, ties to even (RFC 8785 section 3.2.2.3 reading).
        number: int | float = float(value)
    elif isinstance(value, int):
        number = value  # a native integer is taken exactly
    else:
        number = value
    if isinstance(number, float):
        if not math.isfinite(number):
            raise MnemeValidationError("value.number_not_finite", repr(value))
        if number.is_integer():
            number = int(number)  # one numeric kind (G-023); -0.0 becomes 0 (G-017)
    if isinstance(number, int) and not -SAFE_INTEGER_MAX <= number <= SAFE_INTEGER_MAX:
        raise MnemeValidationError("value.integer_out_of_range", repr(value))
    return number


def _check_domain(value: object) -> None:
    """Stage 2 over the whole value, first violation in canonical order."""
    for item in _canonical_walk(value):
        if isinstance(item, _Numeral):  # before `str`: a numeral token is a str subclass
            _number(item)
        elif isinstance(item, str):
            if _has_surrogate(item):
                raise MnemeValidationError("value.lone_surrogate", "surrogate code point in a string")
        elif isinstance(item, (int, float)) and not isinstance(item, bool):
            _number(item)


def _normalize_scalar(value: object) -> Value:
    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, (_Numeral, int, float)):
        return _number(value)
    assert isinstance(value, str)
    return str(value)


def _normalize(value: object) -> Value:
    """Build the normalized value (only after stage 2 has passed); iterative, so total on depth."""
    if not isinstance(value, (list, dict)):
        return _normalize_scalar(value)
    root: list[Value] | dict[str, Value] = [] if isinstance(value, list) else {}
    # Each entry: a source container and the target container it fills.
    work: list[tuple[list[object] | dict[str, object], list[Value] | dict[str, Value]]] = [(value, root)]
    while work:
        source, target = work.pop()
        items = enumerate(source) if isinstance(source, list) else source.items()
        for key, item in items:
            if isinstance(item, (list, dict)):
                child: list[Value] | dict[str, Value] = [] if isinstance(item, list) else {}
                work.append((item, child))
                converted: Value = child
            else:
                converted = _normalize_scalar(item)
            if isinstance(target, list):
                target.append(converted)
            else:
                target[str(key)] = converted
    return root


def decode_json(text: str | bytes) -> Value:
    """Decode JSON text into a normalized value: stage 1 (source order), then stage 2."""
    decoded = _decode_text(text)
    _check_domain(decoded)
    return _normalize(decoded)


def validate_value(value: object) -> Value:
    """Validate a native value: stage 1 (kinds), then stage 2; return the normalized value."""
    _check_native_kinds(value)
    _check_domain(value)
    return _normalize(value)


# --------------------------------------------------------------------- canonical bytes (SER-009)


def _format_number(number: int | float) -> str:
    """ECMAScript Number::toString of a finite binary64 value (RFC 8785 section 3.2.2.3)."""
    if isinstance(number, int):
        return str(number)
    if number == 0:
        return "0"
    if number < 0:
        return "-" + _format_number(-number)
    # repr() yields the shortest round-tripping digits; Decimal exposes them as digits x 10^exp.
    _sign, digit_tuple, exponent = Decimal(repr(number)).as_tuple()
    assert isinstance(exponent, int)
    digits = "".join(map(str, digit_tuple)).lstrip("0")
    stripped = digits.rstrip("0")
    exponent += len(digits) - len(stripped)
    digits = stripped
    k = len(digits)
    n = exponent + k  # value = 0.digits x 10^n
    if k <= n <= 21:
        return digits + "0" * (n - k)
    if 0 < n <= 21:
        return digits[:n] + "." + digits[n:]
    if -6 < n <= 0:
        return "0." + "0" * (-n) + digits
    e = n - 1
    mantissa = digits if k == 1 else digits[0] + "." + digits[1:]
    return f"{mantissa}e{'+' if e > 0 else '-'}{abs(e)}"


_SHORT_ESCAPES = {'"': '\\"', "\\": "\\\\", "\b": "\\b", "\f": "\\f", "\n": "\\n", "\r": "\\r", "\t": "\\t"}


def _format_string(text: str) -> str:
    out = ['"']
    for char in text:
        if char in _SHORT_ESCAPES:
            out.append(_SHORT_ESCAPES[char])
        elif char < " ":
            out.append(f"\\u{ord(char):04x}")
        else:
            out.append(char)
    out.append('"')
    return "".join(out)


def _serialize(value: Value) -> str:
    # Iterative to stay total on deep values: a stack of pending output pieces and values.
    out: list[str] = []
    work: list[object] = [value]
    while work:
        item = work.pop()
        if isinstance(item, _Literal):
            out.append(item)
        elif item is None:
            out.append("null")
        elif item is True:
            out.append("true")
        elif item is False:
            out.append("false")
        elif isinstance(item, (int, float)):
            out.append(_format_number(item))
        elif isinstance(item, str):
            out.append(_format_string(item))
        elif isinstance(item, list):
            pending: list[object] = [_Literal("[")]
            for index, element in enumerate(item):
                if index:
                    pending.append(_Literal(","))
                pending.append(element)
            pending.append(_Literal("]"))
            work.extend(reversed(pending))
        else:
            assert isinstance(item, dict)
            pending = [_Literal("{")]
            for index, name in enumerate(sorted(item, key=_utf16_key)):
                if index:
                    pending.append(_Literal(","))
                pending.append(_Literal(_format_string(name) + ":"))
                pending.append(item[name])
            pending.append(_Literal("}"))
            work.extend(reversed(pending))
    return "".join(out)


class _Literal(str):
    """Already-serialized output during iterative serialization."""


def canonical_bytes(value: object) -> bytes:
    """RFC 8785 canonical bytes (UTF-8) of a value, validated through SER-008 first."""
    return _serialize(validate_value(value)).encode("utf-8")


def content_hash(value: object) -> str:
    """SHA-256 of the canonical bytes, 64 lowercase hex digits (SER-009, G-014)."""
    return hashlib.sha256(canonical_bytes(value)).hexdigest()
