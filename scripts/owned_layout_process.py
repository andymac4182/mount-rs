"""Pure owned-process receipt logic for the explicit layout comparison mode.

Importing this module performs no process, signal, environment or filesystem
operation. Callers create the owned new-session Popen and supply every group
and clock operation. Receipt writing and owner PID-file identity checks belong
to the separately reviewed callers.
"""

from __future__ import annotations

import subprocess


_SCHEMA = "mount-rs.owned-layout-process.v1"
_PREFIX = "OWNED_LAYOUT_PROCESS_"
_ACTIONS = frozenset(
    (
        "outer.comparison", "engine.capacity", "engine.info", "image.inspect",
        "image.pull", "container.create", "container.inspect", "container.remove",
        "container.absence", "container.exec", "container.port", "container.logs",
        "volume.create", "volume.inspect", "volume.remove", "volume.absence",
        "network.create", "network.inspect", "network.remove", "network.absence",
        "bind.cleanup",
    )
)
_KEYS = (
    "schema", "action_id", "pid", "pgid", "raw_returncode", "normalized_exit",
    "supervisor_signal", "child_return_signal", "wait_unavailable_observed",
    "deadline_exceeded", "group_leak_observed", "group_probe_unavailable_observed",
    "retirement_signal_unavailable_observed", "forced_process_retirement",
    "retirement_signals", "child_reaped", "group_state", "sticky_failure",
    "failure_code",
)
_BOOL_KEYS = (
    "wait_unavailable_observed", "deadline_exceeded", "group_leak_observed",
    "group_probe_unavailable_observed", "retirement_signal_unavailable_observed",
    "forced_process_retirement", "child_reaped", "sticky_failure",
)


def _valid_pid(value: object) -> bool:
    return type(value) is int and 0 < value <= 2147483647


def _valid_returncode(value: object) -> bool:
    return value is None or (type(value) is int and -64 <= value <= 255)


class SupervisorInterrupted(Exception):
    """One actual INT/TERM observed by the comparison supervisor handler."""

    def __init__(self, signum: int) -> None:
        if type(signum) is not int or signum not in (2, 15):
            raise ValueError(_PREFIX + "SIGNAL_INVALID")
        super().__init__(_PREFIX + "SUPERVISOR_SIGNAL")
        self.signum = signum


def probe_owned_group(pgid: int, *, killpg) -> str:
    """Observe only the supplied owned group; never discover other processes."""
    if not _valid_pid(pgid):
        raise ValueError(_PREFIX + "GROUP_INVALID")
    try:
        killpg(pgid, 0)
    except ProcessLookupError:
        return "absent"
    except OSError:
        return "unavailable"
    return "present"


def _policy(policy: str, action_id: str) -> tuple[float, float, float, float]:
    if type(action_id) is not str or action_id not in _ACTIONS:
        raise ValueError(_PREFIX + "ACTION_INVALID")
    if type(policy) is not str or policy not in ("action", "startup", "outer"):
        raise ValueError(_PREFIX + "POLICY_INVALID")
    if (policy == "outer") != (action_id == "outer.comparison"):
        raise ValueError(_PREFIX + "POLICY_INVALID")
    if policy == "outer":
        return (900.0, 5.0, 5.0, 5.0)
    if policy == "startup":
        return (120.0, 1.0, 2.0, 2.0)
    return (30.0, 1.0, 2.0, 2.0)


def _result(facts: dict[str, object]) -> tuple[int, str | None]:
    if facts["child_reaped"] is not True:
        return 125, _PREFIX + "UNREAPED"
    if facts["wait_unavailable_observed"] or facts["raw_returncode"] is None:
        return 125, _PREFIX + "WAIT_UNAVAILABLE"
    if facts["group_probe_unavailable_observed"] or facts["group_state"] == "unavailable":
        return 125, _PREFIX + "GROUP_UNAVAILABLE"
    if facts["retirement_signal_unavailable_observed"]:
        return 125, _PREFIX + "SIGNAL_UNAVAILABLE"
    if facts["group_state"] != "absent" or facts["group_leak_observed"]:
        return 125, _PREFIX + "GROUP_LEAK"
    if facts["deadline_exceeded"]:
        return 124, _PREFIX + "TIMEOUT"
    if facts["supervisor_signal"] is not None:
        return 128 + facts["supervisor_signal"], _PREFIX + "SUPERVISOR_SIGNAL"
    raw = facts["raw_returncode"]
    normalized = raw if raw >= 0 else 128 + (-raw)
    if normalized != 0:
        return normalized, _PREFIX + "CHILD_FAILED"
    if facts["forced_process_retirement"]:
        # A retirement without a retained cause is incomplete, never success.
        return 125, _PREFIX + "GROUP_LEAK"
    return 0, None


class _Observation:
    def __init__(self, process, pgid, probe_group, signal_group, monotonic, sleep):
        self.process = process
        self.pgid = pgid
        self.probe_group = probe_group
        self.signal_group = signal_group
        self.monotonic = monotonic
        self.sleep = sleep
        self.raw_returncode = None
        self.supervisor_signal = None
        self.wait_unavailable = False
        self.deadline_exceeded = False
        self.group_leak = False
        self.probe_unavailable = False
        self.signal_unavailable = False
        self.retirement_signals = []
        self.child_reaped = False
        self.group_state = "unavailable"

    def interrupted(self, error: SupervisorInterrupted) -> None:
        if self.supervisor_signal is None:
            self.supervisor_signal = error.signum

    def wait(self, seconds: float) -> str:
        try:
            raw = self.process.wait(timeout=seconds)
        except SupervisorInterrupted as error:
            self.interrupted(error)
            return "interrupted"
        except subprocess.TimeoutExpired:
            return "timeout"
        except Exception:
            self.wait_unavailable = True
            return "unavailable"
        self.child_reaped = True
        if not _valid_returncode(raw) or raw is None:
            self.wait_unavailable = True
            return "unavailable"
        self.raw_returncode = raw
        return "reaped"

    def probe(self) -> str:
        try:
            state = self.probe_group(self.pgid)
        except SupervisorInterrupted as error:
            self.interrupted(error)
            return "interrupted"
        except Exception:
            state = "unavailable"
        if type(state) is not str or state not in ("absent", "present", "unavailable"):
            state = "unavailable"
        self.group_state = state
        if state == "unavailable":
            self.probe_unavailable = True
        return state

    def settle(self, seconds: float) -> str:
        try:
            deadline = self.monotonic() + seconds
            while True:
                state = self.probe()
                if state != "present":
                    return state
                remaining = deadline - self.monotonic()
                if remaining <= 0:
                    return state
                self.sleep(min(0.05, remaining))
        except SupervisorInterrupted as error:
            self.interrupted(error)
            return "interrupted"
        except Exception:
            self.group_state = "unavailable"
            self.probe_unavailable = True
            return "unavailable"

    def signal(self, signum: int) -> None:
        self.retirement_signals.append(signum)
        try:
            self.signal_group(self.pgid, signum)
        except ProcessLookupError:
            # Disappearance during retirement is not proof of signal delivery.
            pass
        except SupervisorInterrupted as error:
            self.interrupted(error)
        except Exception:
            self.signal_unavailable = True


def supervise_owned_process(process, *, action_id: str, policy: str,
                            owned_pgid: int, probe_group, signal_group,
                            monotonic, sleep) -> dict[str, object]:
    """Observe one caller-owned new-session Popen with fixed production bounds."""
    deadline, term_grace, kill_grace, initial_settle = _policy(policy, action_id)
    pid = process.pid
    if not _valid_pid(pid):
        raise ValueError(_PREFIX + "ID_INVALID")
    if not _valid_pid(owned_pgid) or owned_pgid != pid:
        raise ValueError(_PREFIX + "GROUP_INVALID")
    observed = _Observation(process, owned_pgid, probe_group, signal_group, monotonic, sleep)
    initial = observed.wait(deadline)
    if initial == "timeout":
        observed.deadline_exceeded = True
    initial_group = observed.settle(initial_settle) if initial == "reaped" else None
    if initial_group == "present":
        observed.group_leak = True
    if initial != "reaped" or initial_group != "absent":
        observed.signal(15)
        observed.wait(term_grace)
        if observed.settle(term_grace) != "absent":
            observed.signal(9)
            observed.wait(kill_grace)
            observed.settle(kill_grace)
        if not observed.child_reaped:
            observed.wait(0.0)
    if observed.probe() == "interrupted":
        # A interrupted final probe cannot reuse an earlier absence result.
        observed.group_state = "unavailable"
        observed.probe_unavailable = True
    raw = observed.raw_returncode
    receipt = {
        "schema": _SCHEMA,
        "action_id": action_id,
        "pid": pid,
        "pgid": owned_pgid,
        "raw_returncode": raw,
        "normalized_exit": 0,
        "supervisor_signal": observed.supervisor_signal,
        "child_return_signal": -raw if raw is not None and raw < 0 else None,
        "wait_unavailable_observed": observed.wait_unavailable,
        "deadline_exceeded": observed.deadline_exceeded,
        "group_leak_observed": observed.group_leak,
        "group_probe_unavailable_observed": observed.probe_unavailable,
        "retirement_signal_unavailable_observed": observed.signal_unavailable,
        "forced_process_retirement": bool(observed.retirement_signals),
        "retirement_signals": list(observed.retirement_signals),
        "child_reaped": observed.child_reaped,
        "group_state": observed.group_state,
        "sticky_failure": False,
        "failure_code": None,
    }
    receipt["normalized_exit"], receipt["failure_code"] = _result(receipt)
    receipt["sticky_failure"] = (
        receipt["normalized_exit"] != 0 or receipt["forced_process_retirement"]
        or receipt["wait_unavailable_observed"] or receipt["group_probe_unavailable_observed"]
        or receipt["retirement_signal_unavailable_observed"]
    )
    return receipt


def _receipt_copy(receipt: dict[str, object]) -> dict[str, object]:
    invalid = _PREFIX + "RECEIPT_INVALID"
    if type(receipt) is not dict or any(type(key) is not str for key in receipt) or set(receipt) != set(_KEYS):
        raise ValueError(invalid)
    if receipt["schema"] != _SCHEMA or type(receipt["schema"]) is not str:
        raise ValueError(invalid)
    if type(receipt["action_id"]) is not str or receipt["action_id"] not in _ACTIONS:
        raise ValueError(invalid)
    if not _valid_pid(receipt["pid"]) or not _valid_pid(receipt["pgid"]) or receipt["pid"] != receipt["pgid"]:
        raise ValueError(invalid)
    if not _valid_returncode(receipt["raw_returncode"]):
        raise ValueError(invalid)
    if type(receipt["normalized_exit"]) is not int or not 0 <= receipt["normalized_exit"] <= 255:
        raise ValueError(invalid)
    if any(type(receipt[key]) is not bool for key in _BOOL_KEYS):
        raise ValueError(invalid)
    supervisor_signal = receipt["supervisor_signal"]
    if supervisor_signal is not None and (type(supervisor_signal) is not int or supervisor_signal not in (2, 15)):
        raise ValueError(invalid)
    raw = receipt["raw_returncode"]
    expected_child_signal = -raw if raw is not None and raw < 0 else None
    if receipt["child_return_signal"] != expected_child_signal:
        raise ValueError(invalid)
    if receipt["child_return_signal"] is not None and type(receipt["child_return_signal"]) is not int:
        raise ValueError(invalid)
    signals = receipt["retirement_signals"]
    if type(signals) is not list or any(type(value) is not int for value in signals) or signals not in ([], [15], [15, 9]):
        raise ValueError(invalid)
    if receipt["forced_process_retirement"] != bool(signals):
        raise ValueError(invalid)
    if receipt["retirement_signal_unavailable_observed"] and not signals:
        raise ValueError(invalid)
    if type(receipt["group_state"]) is not str or receipt["group_state"] not in ("absent", "present", "unavailable"):
        raise ValueError(invalid)
    if receipt["failure_code"] is not None and type(receipt["failure_code"]) is not str:
        raise ValueError(invalid)
    normalized, failure = _result(receipt)
    if receipt["normalized_exit"] != normalized or receipt["failure_code"] != failure:
        raise ValueError(invalid)
    sticky = normalized != 0 or bool(signals) or any(
        receipt[key] for key in (
            "wait_unavailable_observed", "group_probe_unavailable_observed",
            "retirement_signal_unavailable_observed",
        )
    )
    if receipt["sticky_failure"] != sticky:
        raise ValueError(invalid)
    copied = {key: receipt[key] for key in _KEYS}
    copied["retirement_signals"] = list(signals)
    return copied


def retain_before_release(receipt: dict[str, object], *, retain, release) -> bool:
    """Retain pinned independent facts before owner-checked PID-file release."""
    pinned = _receipt_copy(receipt)
    retained_copy = _receipt_copy(pinned)
    retain(retained_copy)
    try:
        validated_retained = _receipt_copy(retained_copy)
    except ValueError:
        raise ValueError(_PREFIX + "RECEIPT_MUTATED") from None
    if validated_retained != pinned:
        raise ValueError(_PREFIX + "RECEIPT_MUTATED")
    if pinned["child_reaped"] is True and pinned["group_state"] == "absent":
        release()
        return True
    return False
