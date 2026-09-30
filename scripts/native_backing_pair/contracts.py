"""Closed input contracts for the native backing-pair artifact core.

Importing this module performs no filesystem or process operations.
"""

from enum import Enum
import json
import math


class ContractError(RuntimeError):
    """A private input cannot qualify the requested operation."""


class Backend(Enum):
    FILESYSTEM = "filesystem"
    RUSTFS = "rustfs"


def strict_json(raw):
    def pairs(rows):
        result = {}
        for key, value in rows:
            if key in result:
                raise ContractError("duplicate_json_key")
            result[key] = value
        return result

    def constant(_):
        raise ContractError("nonfinite_json_constant")

    def finite_float(value):
        result = float(value)
        if not math.isfinite(result):
            raise ContractError("nonfinite_json_number")
        return result

    try:
        if type(raw) is bytes:
            text = raw.decode("utf-8")
        elif type(raw) is str:
            raw.encode("utf-8")
            text = raw
        else:
            raise ContractError("json_input_type")
        return json.loads(text, object_pairs_hook=pairs, parse_constant=constant,
                          parse_float=finite_float)
    except (TypeError, ValueError, UnicodeError) as error:
        raise ContractError("invalid_json") from error


def uint64(value):
    if type(value) is not int or not 0 <= value <= (1 << 64) - 1:
        raise ContractError("invalid_unsigned_integer")
    return value


def require_keys(value, required):
    if (type(required) is not frozenset or any(type(key) is not str for key in required)
            or type(value) is not dict or set(value) != required):
        raise ContractError("closed_object_fields")


def parse_backend(value):
    if type(value) is not str:
        raise ContractError("unknown_backend")
    try:
        return Backend(value)
    except ValueError as error:
        raise ContractError("unknown_backend") from error
