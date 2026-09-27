#!/usr/bin/env python3
"""Modeled comparison supervisor controls; these do not start owned children."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import signal
import subprocess
import unittest
from unittest.mock import patch


MODULE_PATH = Path(__file__).with_name("owned_layout_process.py")


class MissingSupervisorInterrupted(Exception):
    def __init__(self, signum: int) -> None:
        super().__init__("modeled supervisor interruption")
        self.signum = signum


class MissingAPI:
    """An inert absent-feature sentinel makes the first RED semantic."""

    SupervisorInterrupted = MissingSupervisorInterrupted

    @staticmethod
    def probe_owned_group(_pgid: int, *, killpg: object) -> object:
        return None

    @staticmethod
    def supervise_owned_process(_process: object, **_kwargs: object) -> dict[str, object]:
        return {}

    @staticmethod
    def retain_before_release(_receipt: object, **_kwargs: object) -> bool:
        return False


if MODULE_PATH.exists():
    spec = importlib.util.spec_from_file_location("owned_layout_process_controls", MODULE_PATH)
    if spec is None or spec.loader is None:
        raise AssertionError("comparison process module must have an inert source loader")
    api = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(api)
else:
    api = MissingAPI()


class FakeClock:
    def __init__(self) -> None:
        self.now = 0.0
        self.sleeps: list[float] = []

    def monotonic(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        if not 0.0 <= seconds <= 0.05:
            raise AssertionError("modeled supervisor must use bounded group-probe sleeps")
        self.sleeps.append(seconds)
        self.now = round(self.now + seconds, 6)


class FakePopen:
    def __init__(self, waits: list[object], *, pid: int = 24680, returncode: int | None = None) -> None:
        self.pid = pid
        self.returncode = returncode
        self.events = list(waits)
        self.wait_timeouts: list[float] = []
        self.poll_calls = 0

    def wait(self, timeout: float) -> int:
        self.wait_timeouts.append(timeout)
        event = self.events.pop(0) if len(self.events) > 1 else self.events[0]
        if event == "timeout":
            raise subprocess.TimeoutExpired("modeled-owned-child", timeout)
        if isinstance(event, BaseException):
            raise event
        if type(event) is not int:
            raise AssertionError("invalid modeled child wait event")
        self.returncode = event
        return event

    def poll(self) -> int | None:
        self.poll_calls += 1
        return self.returncode


class FakeGroup:
    def __init__(self, states: list[object], *, signal_error: BaseException | None = None) -> None:
        self.states = list(states)
        self.probes: list[int] = []
        self.signals: list[tuple[int, int]] = []
        self.signal_error = signal_error

    def probe(self, pgid: int) -> str:
        self.probes.append(pgid)
        event = self.states.pop(0) if len(self.states) > 1 else self.states[0]
        if isinstance(event, BaseException):
            raise event
        if type(event) is not str:
            raise AssertionError("invalid modeled group probe event")
        return event

    def send(self, pgid: int, signum: int) -> None:
        self.signals.append((pgid, signum))
        if self.signal_error is not None:
            raise self.signal_error


class EqualToAnything:
    """A modeled non-scalar that defeats value equality but not exact types."""

    def __eq__(self, _other: object) -> bool:
        return True

    def __ne__(self, _other: object) -> bool:
        return False


RECEIPT_KEYS = {
    "schema", "action_id", "pid", "pgid", "raw_returncode", "normalized_exit",
    "supervisor_signal", "child_return_signal", "deadline_exceeded", "group_leak_observed",
    "wait_unavailable_observed",
    "group_probe_unavailable_observed", "retirement_signal_unavailable_observed",
    "forced_process_retirement", "retirement_signals", "child_reaped", "group_state",
    "sticky_failure", "failure_code",
}


def run_model(process: FakePopen, group: FakeGroup, *, policy: str = "action", action_id: str = "container.inspect", owned_pgid: int | None = None, clock: FakeClock | None = None) -> dict[str, object]:
    clock = clock if clock is not None else FakeClock()
    return api.supervise_owned_process(
        process,
        action_id=action_id,
        policy=policy,
        owned_pgid=process.pid if owned_pgid is None else owned_pgid,
        probe_group=group.probe,
        signal_group=group.send,
        monotonic=clock.monotonic,
        sleep=clock.sleep,
    )


class OwnedLayoutProcessModels(unittest.TestCase):
    def test_model_fixtures_are_inert_and_clock_does_not_sleep(self) -> None:
        process = FakePopen([0])
        group = FakeGroup(["absent"])
        clock = FakeClock()
        self.assertEqual(process.wait(30.0), 0)
        self.assertEqual(group.probe(process.pid), "absent")
        clock.sleep(0.05)
        self.assertEqual(clock.monotonic(), 0.05)
        self.assertEqual(process.poll_calls, 0)

    def test_probe_absent_uses_only_the_exact_owned_group(self) -> None:
        calls: list[tuple[int, int]] = []

        def killpg(pgid: int, signum: int) -> None:
            calls.append((pgid, signum))
            raise ProcessLookupError()

        result = api.probe_owned_group(24680, killpg=killpg)
        self.assertEqual(result, "absent", "ESRCH must be an explicit owned-group absence")
        self.assertEqual(calls, [(24680, 0)])

    def test_probe_present_uses_only_the_exact_owned_group(self) -> None:
        calls: list[tuple[int, int]] = []

        def killpg(pgid: int, signum: int) -> None:
            calls.append((pgid, signum))

        result = api.probe_owned_group(24680, killpg=killpg)
        self.assertEqual(result, "present", "a successful signal-zero probe means present")
        self.assertEqual(calls, [(24680, 0)])

    def test_probe_eperm_is_unavailable_without_a_process_inventory(self) -> None:
        calls: list[tuple[int, int]] = []

        def killpg(pgid: int, signum: int) -> None:
            calls.append((pgid, signum))
            raise PermissionError()

        with patch("subprocess.run", side_effect=AssertionError("global process inventory forbidden")):
            result = api.probe_owned_group(24680, killpg=killpg)
        self.assertEqual(result, "unavailable", "EPERM supplies no absence proof")
        self.assertEqual(calls, [(24680, 0)])

    def test_clean_wait_and_final_absence_have_an_exact_closed_receipt(self) -> None:
        result = run_model(FakePopen([0]), FakeGroup(["absent"]))
        self.assertEqual(set(result), RECEIPT_KEYS, "owned outcome fields must remain closed")
        self.assertEqual(result["schema"], "mount-rs.owned-layout-process.v1")
        self.assertEqual((result["pid"], result["pgid"]), (24680, 24680))
        self.assertEqual(result["action_id"], "container.inspect")
        self.assertEqual(result["raw_returncode"], 0)
        self.assertEqual(result["normalized_exit"], 0)
        self.assertIs(result["child_reaped"], True)
        self.assertEqual(result["group_state"], "absent")
        self.assertIs(result["sticky_failure"], False)
        self.assertIs(result["forced_process_retirement"], False)
        self.assertEqual(result["retirement_signals"], [])

    def test_positive_143_does_not_establish_child_sigterm(self) -> None:
        result = run_model(FakePopen([143]), FakeGroup(["absent"]))
        self.assertEqual(result.get("raw_returncode"), 143, "retain actual positive child exit")
        self.assertEqual(result.get("normalized_exit"), 143)
        self.assertIsNone(result.get("child_return_signal"))
        self.assertIsNone(result.get("supervisor_signal"))
        self.assertIs(result.get("sticky_failure"), True)
        self.assertEqual(result.get("failure_code"), "OWNED_LAYOUT_PROCESS_CHILD_FAILED")

    def test_negative_15_records_actual_child_signal(self) -> None:
        result = run_model(FakePopen([-15]), FakeGroup(["absent"]))
        self.assertEqual(result.get("raw_returncode"), -15, "negative returncode is signal evidence")
        self.assertEqual(result.get("child_return_signal"), 15)
        self.assertEqual(result.get("normalized_exit"), 143)
        self.assertIsNone(result.get("supervisor_signal"))
        self.assertIs(result.get("forced_process_retirement"), False)

    def test_timeout_stays_failed_after_retirement_child_exit_zero(self) -> None:
        group = FakeGroup(["absent"])
        result = run_model(FakePopen(["timeout", 0]), group)
        self.assertIs(result.get("deadline_exceeded"), True, "later exit0 cannot erase timeout")
        self.assertEqual(result.get("raw_returncode"), 0)
        self.assertEqual(result.get("normalized_exit"), 124)
        self.assertEqual(result.get("failure_code"), "OWNED_LAYOUT_PROCESS_TIMEOUT")
        self.assertIs(result.get("forced_process_retirement"), True)
        self.assertEqual(result.get("retirement_signals"), [15])
        self.assertEqual(group.signals, [(24680, signal.SIGTERM)])

    def test_wait_error_stays_failed_after_actual_reaping_and_absence(self) -> None:
        result = run_model(FakePopen([OSError("modeled wait unavailable"), 0]), FakeGroup(["absent"]))
        self.assertIs(result.get("wait_unavailable_observed"), True, "later actual wait cannot erase wait fault")
        self.assertIs(result.get("child_reaped"), True)
        self.assertEqual(result.get("raw_returncode"), 0)
        self.assertEqual(result.get("group_state"), "absent")
        self.assertEqual(result.get("normalized_exit"), 125)
        self.assertEqual(result.get("failure_code"), "OWNED_LAYOUT_PROCESS_WAIT_UNAVAILABLE")
        self.assertIs(result.get("sticky_failure"), True)

    def test_initial_group_leak_stays_failed_after_final_absence(self) -> None:
        # Forty-one0.05s probes exhaust the unchanged2s action settle window.
        group = FakeGroup(["present"] * 41 + ["absent"])
        result = run_model(FakePopen([0]), group)
        self.assertIs(result.get("group_leak_observed"), True, "late absence cannot erase a leak")
        self.assertEqual(result.get("group_state"), "absent")
        self.assertEqual(result.get("normalized_exit"), 125)
        self.assertEqual(result.get("failure_code"), "OWNED_LAYOUT_PROCESS_GROUP_LEAK")
        self.assertIs(result.get("forced_process_retirement"), True)

    def test_initial_unavailable_probe_stays_failed_after_final_absence(self) -> None:
        result = run_model(FakePopen([0]), FakeGroup(["unavailable", "absent"]))
        self.assertIs(result.get("group_probe_unavailable_observed"), True, "unknown is sticky")
        self.assertEqual(result.get("group_state"), "absent")
        self.assertEqual(result.get("normalized_exit"), 125)
        self.assertEqual(result.get("failure_code"), "OWNED_LAYOUT_PROCESS_GROUP_UNAVAILABLE")
        self.assertIs(result.get("sticky_failure"), True)

    def test_final_present_group_is_not_settled(self) -> None:
        result = run_model(FakePopen([0]), FakeGroup(["present"]))
        self.assertEqual(result.get("group_state"), "present", "retained group is not absence")
        self.assertEqual(result.get("normalized_exit"), 125)
        self.assertIs(result.get("sticky_failure"), True)
        self.assertEqual(result.get("retirement_signals"), [15, 9])

    def test_final_unavailable_group_is_not_settled(self) -> None:
        result = run_model(FakePopen([0]), FakeGroup(["unavailable"]))
        self.assertEqual(result.get("group_state"), "unavailable", "denied probe is not absence")
        self.assertIs(result.get("group_probe_unavailable_observed"), True)
        self.assertEqual(result.get("normalized_exit"), 125)
        self.assertIs(result.get("sticky_failure"), True)

    def test_poll_returncode_cannot_replace_a_successful_wait(self) -> None:
        process = FakePopen(["timeout"], returncode=0)
        result = run_model(process, FakeGroup(["absent"]))
        self.assertIs(result.get("child_reaped"), False, "actual wait must establish reaping")
        self.assertEqual(result.get("normalized_exit"), 125)
        self.assertEqual(result.get("failure_code"), "OWNED_LAYOUT_PROCESS_UNREAPED")
        self.assertEqual(process.poll_calls, 0, "do not infer reaping from poll/property")

    def test_supervisor_int_is_independent_of_retirement_child_sigterm(self) -> None:
        process = FakePopen([api.SupervisorInterrupted(2), -15])
        result = run_model(process, FakeGroup(["absent"]), policy="outer", action_id="outer.comparison")
        self.assertEqual(result.get("supervisor_signal"), 2, "retain actual supervisor INT")
        self.assertEqual(result.get("raw_returncode"), -15)
        self.assertEqual(result.get("child_return_signal"), 15)
        self.assertEqual(result.get("normalized_exit"), 130)
        self.assertEqual(result.get("failure_code"), "OWNED_LAYOUT_PROCESS_SUPERVISOR_SIGNAL")
        self.assertIs(result.get("forced_process_retirement"), True)

    def test_interruption_during_retirement_keeps_first_signal_and_single_cleanup(self) -> None:
        process = FakePopen([api.SupervisorInterrupted(2), api.SupervisorInterrupted(15), -9])
        group = FakeGroup(["absent"])
        result = run_model(process, group, policy="outer", action_id="outer.comparison")
        self.assertEqual(result.get("supervisor_signal"), 2, "later TERM must not replace earlier INT")
        self.assertEqual(result.get("child_return_signal"), 9)
        self.assertIs(result.get("child_reaped"), True)
        self.assertEqual(result.get("normalized_exit"), 130)
        self.assertLessEqual(group.signals.count((24680, signal.SIGTERM)), 1)
        self.assertLessEqual(group.signals.count((24680, signal.SIGKILL)), 1)

    def test_interruption_during_probe_uses_the_single_retirement_path(self) -> None:
        group = FakeGroup([api.SupervisorInterrupted(15), "absent"])
        result = run_model(FakePopen([0]), group, policy="outer", action_id="outer.comparison")
        self.assertEqual(result.get("supervisor_signal"), 15, "probe interruption is an actual supervisor signal")
        self.assertEqual(result.get("normalized_exit"), 143)
        self.assertIs(result.get("sticky_failure"), True)
        self.assertEqual(group.signals, [(24680, signal.SIGTERM)])

    def test_denied_retirement_signal_is_sticky_after_final_absence(self) -> None:
        group = FakeGroup(["absent"], signal_error=PermissionError())
        result = run_model(FakePopen(["timeout", 0]), group)
        self.assertIs(result.get("retirement_signal_unavailable_observed"), True, "denied delivery is unknown")
        self.assertEqual(result.get("group_state"), "absent")
        self.assertEqual(result.get("normalized_exit"), 125)
        self.assertEqual(result.get("failure_code"), "OWNED_LAYOUT_PROCESS_SIGNAL_UNAVAILABLE")

    def test_unknown_action_id_is_refused_before_any_process_operation(self) -> None:
        process = FakePopen([0])
        group = FakeGroup(["absent"])
        rejected = False
        try:
            run_model(process, group, action_id="raw-owner-or-argv")
        except ValueError as error:
            self.assertEqual(str(error), "OWNED_LAYOUT_PROCESS_ACTION_INVALID")
            rejected = True
        self.assertIs(rejected, True, "action_id must use the closed operation allowlist")
        self.assertEqual(process.wait_timeouts, [])
        self.assertEqual(group.probes, [])
        self.assertEqual(group.signals, [])

    def test_substituted_group_is_refused_before_any_process_operation(self) -> None:
        process = FakePopen([0])
        group = FakeGroup(["absent"])
        rejected = False
        try:
            run_model(process, group, owned_pgid=24681)
        except ValueError as error:
            self.assertEqual(str(error), "OWNED_LAYOUT_PROCESS_GROUP_INVALID")
            rejected = True
        self.assertIs(rejected, True, "new-session Popen must bind the exact owned leader/group")
        self.assertEqual(process.wait_timeouts, [])
        self.assertEqual(group.probes, [])
        self.assertEqual(group.signals, [])

    def test_comparison_policies_retain_existing_timeouts(self) -> None:
        for policy, action_id, expected in [
            ("action", "container.inspect", 30.0),
            ("startup", "container.create", 120.0),
            ("outer", "outer.comparison", 900.0),
        ]:
            with self.subTest(policy=policy):
                process = FakePopen([0])
                run_model(process, FakeGroup(["absent"]), policy=policy, action_id=action_id)
                self.assertEqual(process.wait_timeouts, [expected], "policy must not weaken the cutoff")

    def test_retirement_waits_and_probe_graces_keep_exact_policy_values(self) -> None:
        for policy, action_id, initial, term, kill in [
            ("action", "container.inspect", 30.0, 1.0, 2.0),
            ("startup", "container.create", 120.0, 1.0, 2.0),
            ("outer", "outer.comparison", 900.0, 5.0, 5.0),
        ]:
            with self.subTest(policy=policy):
                clock = FakeClock()
                process = FakePopen(["timeout", "timeout", -9])
                group = FakeGroup(["present"] * (int(term / 0.05) + 1) + ["absent"])
                result = run_model(process, group, policy=policy, action_id=action_id, clock=clock)
                self.assertEqual(process.wait_timeouts, [initial, term, kill], "keep every wait grace")
                self.assertEqual(clock.now, term, "TERM probe grace must not shorten or extend")
                self.assertEqual(result.get("retirement_signals"), [15, 9])
                self.assertEqual(result.get("normalized_exit"), 124)

    def test_initial_settle_grace_keeps_exact_policy_values(self) -> None:
        for policy, action_id, settle in [
            ("action", "container.inspect", 2.0),
            ("startup", "container.create", 2.0),
            ("outer", "outer.comparison", 5.0),
        ]:
            with self.subTest(policy=policy):
                clock = FakeClock()
                group = FakeGroup(["present"] * (int(settle / 0.05) + 1) + ["absent"])
                result = run_model(FakePopen([0]), group, policy=policy, action_id=action_id, clock=clock)
                self.assertEqual(clock.now, settle, "initial group settle must retain exact existing grace")
                self.assertIs(result.get("group_leak_observed"), True)

    def test_unknown_policy_and_outer_action_mismatch_are_refused(self) -> None:
        for policy, action_id, code in [
            ("arbitrary", "container.inspect", "OWNED_LAYOUT_PROCESS_POLICY_INVALID"),
            ("action", "outer.comparison", "OWNED_LAYOUT_PROCESS_POLICY_INVALID"),
            ("outer", "container.inspect", "OWNED_LAYOUT_PROCESS_POLICY_INVALID"),
        ]:
            with self.subTest(policy=policy, action_id=action_id):
                process = FakePopen([0])
                group = FakeGroup(["absent"])
                rejected = False
                try:
                    run_model(process, group, policy=policy, action_id=action_id)
                except ValueError as error:
                    self.assertEqual(str(error), code)
                    rejected = True
                self.assertIs(rejected, True, "closed policy must fail before any wait/probe/signal")
                self.assertEqual(process.wait_timeouts, [])
                self.assertEqual(group.probes, [])
                self.assertEqual(group.signals, [])

    def test_boolean_nonpositive_and_out_of_range_pid_are_refused(self) -> None:
        for pid in [True, 0, -1, 2147483648]:
            with self.subTest(pid=pid):
                process = FakePopen([0], pid=pid)
                group = FakeGroup(["absent"])
                rejected = False
                try:
                    run_model(process, group)
                except ValueError as error:
                    self.assertEqual(str(error), "OWNED_LAYOUT_PROCESS_ID_INVALID")
                    rejected = True
                self.assertIs(rejected, True, "only an exact positive POSIX owned pid is valid")
                self.assertEqual(process.wait_timeouts, [])
                self.assertEqual(group.probes, [])
                self.assertEqual(group.signals, [])

    def test_boolean_and_substituted_pgid_are_refused(self) -> None:
        for pgid in [True, 0, -1, 24681, 2147483648]:
            with self.subTest(pgid=pgid):
                rejected = False
                try:
                    run_model(FakePopen([0]), FakeGroup(["absent"]), owned_pgid=pgid)
                except ValueError as error:
                    self.assertEqual(str(error), "OWNED_LAYOUT_PROCESS_GROUP_INVALID")
                    rejected = True
                self.assertIs(rejected, True, "exact owned group cannot be coerced or substituted")

    def test_invalid_supervisor_signal_is_refused(self) -> None:
        for signum in [True, 0, 9, 64]:
            with self.subTest(signum=signum):
                rejected = False
                try:
                    api.SupervisorInterrupted(signum)
                except ValueError as error:
                    self.assertEqual(str(error), "OWNED_LAYOUT_PROCESS_SIGNAL_INVALID")
                    rejected = True
                self.assertIs(rejected, True, "only actual comparison INT/TERM is accepted")

    def test_expected_docker_force_removal_has_no_host_retirement(self) -> None:
        result = run_model(FakePopen([0]), FakeGroup(["absent"]), action_id="container.remove")
        self.assertIs(result.get("forced_process_retirement"), False, "resource force policy is separate")
        self.assertEqual(result.get("retirement_signals"), [])
        self.assertEqual(result.get("normalized_exit"), 0)

    def test_actual_engine_image_port_log_action_ids_have_fixed_receipts(self) -> None:
        for action_id, policy in [
            ("engine.info", "action"), ("image.inspect", "action"),
            ("image.pull", "startup"), ("container.port", "action"),
            ("container.logs", "action"),
        ]:
            with self.subTest(action_id=action_id):
                result = run_model(FakePopen([0]), FakeGroup(["absent"]), action_id=action_id, policy=policy)
                self.assertEqual(result.get("action_id"), action_id, "actual owner calls use fixed safe IDs")
                self.assertEqual(result.get("normalized_exit"), 0)

    def test_receipt_is_retained_before_matching_pid_file_release(self) -> None:
        receipt = run_model(FakePopen([0]), FakeGroup(["absent"]))
        events: list[str] = []

        def retain(actual: object) -> None:
            self.assertEqual(actual, receipt)
            events.append("retained")

        released = api.retain_before_release(receipt, retain=retain, release=lambda: events.append("released"))
        self.assertIs(released, True, "settled child permits identity-checked owner release")
        self.assertEqual(events, ["retained", "released"], "receipt precedes PID-file release")

    def test_unreaped_outcome_is_retained_without_pid_file_release(self) -> None:
        receipt = run_model(FakePopen(["timeout"]), FakeGroup(["absent"]))
        events: list[str] = []
        released = api.retain_before_release(receipt, retain=lambda _actual: events.append("retained"), release=lambda: events.append("released"))
        self.assertEqual(events, ["retained"], "unreaped owner remains tracked after receipt retention")
        self.assertIs(released, False)

    def test_settled_timeout_and_signal_are_retained_before_release(self) -> None:
        for waits in [["timeout", 0], [api.SupervisorInterrupted(15), -15]]:
            with self.subTest(first_event=type(waits[0]).__name__):
                receipt = run_model(FakePopen(waits), FakeGroup(["absent"]), policy="outer", action_id="outer.comparison")
                events: list[str] = []
                released = api.retain_before_release(receipt, retain=lambda _actual: events.append("retained"), release=lambda: events.append("released"))
                self.assertIs(receipt.get("sticky_failure"), True, "settled retirement still failed")
                self.assertIs(released, True)
                self.assertEqual(events, ["retained", "released"])

    def test_receipt_retention_failure_prevents_pid_file_release(self) -> None:
        receipt = run_model(FakePopen([0]), FakeGroup(["absent"]))
        events: list[str] = []

        def retain(_actual: object) -> None:
            events.append("retention_failed")
            raise OSError("modeled exclusive output refusal")

        rejected = False
        try:
            api.retain_before_release(receipt, retain=retain, release=lambda: events.append("released"))
        except OSError:
            rejected = True
        self.assertIs(rejected, True, "failed exclusive receipt publication must not be hidden")
        self.assertEqual(events, ["retention_failed"])

    def test_retainer_mutation_cannot_upgrade_release_or_rewrite_failure(self) -> None:
        receipt = run_model(FakePopen(["timeout"]), FakeGroup(["absent"]))
        original = dict(receipt)
        events: list[str] = []

        def retain(actual: dict[str, object]) -> None:
            actual["child_reaped"] = True
            actual["group_state"] = "absent"
            actual["normalized_exit"] = 0
            actual["sticky_failure"] = False
            actual["failure_code"] = None
            events.append("mutation_attempt")

        rejected = False
        try:
            api.retain_before_release(receipt, retain=retain, release=lambda: events.append("released"))
        except ValueError as error:
            self.assertEqual(str(error), "OWNED_LAYOUT_PROCESS_RECEIPT_MUTATED")
            rejected = True
        self.assertIs(rejected, True, "retainer cannot change pinned release/failure facts")
        self.assertEqual(receipt, original, "owner outcome itself remains unchanged")
        self.assertEqual(events, ["mutation_attempt"])

    def test_retainer_equal_value_type_mutation_is_refused_before_release(self) -> None:
        for key, value in [
            ("child_reaped", 1), ("normalized_exit", 0.0),
            ("failure_code", EqualToAnything()),
        ]:
            with self.subTest(key=key):
                receipt = run_model(FakePopen([0]), FakeGroup(["absent"]))
                original = dict(receipt)
                events: list[str] = []

                def retain(actual: dict[str, object]) -> None:
                    actual[key] = value
                    events.append("type_mutation")

                rejected = False
                try:
                    api.retain_before_release(receipt, retain=retain, release=lambda: events.append("released"))
                except ValueError as error:
                    self.assertEqual(str(error), "OWNED_LAYOUT_PROCESS_RECEIPT_MUTATED")
                    rejected = True
                self.assertIs(rejected, True, "equal values must not bypass exact closed scalar types")
                self.assertEqual(receipt, original)
                self.assertEqual(events, ["type_mutation"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
