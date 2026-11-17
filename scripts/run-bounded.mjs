#!/usr/bin/env node
// Run a command with a time limit, for shell scripts on systems without
// timeout(1) (macOS has none by default).
//
//   node scripts/run-bounded.mjs <seconds> <command> [argument...]
//
// The command inherits this process's standard streams and runs in a process
// group of its own, so that everything it started (cargo and the compilers
// and test binaries under it, say) is stopped with it. The exit status is
// the command's own; 124 when the limit passed, as timeout(1) gives; 127
// when the command could not be started; and 128 plus the signal number when
// a signal ended it. A signal sent to this process stops the command too.
import { spawn } from "node:child_process";
import { constants } from "node:os";

const [limitText, command, ...args] = process.argv.slice(2);
const limit = Number(limitText);
if (!command || !Number.isFinite(limit) || limit <= 0) {
  console.error("usage: run-bounded.mjs <seconds> <command> [argument...]");
  process.exit(2);
}

const child = spawn(command, args, { stdio: "inherit", detached: true });
let timedOut = false;
let forced;

function stop(signal) {
  try {
    // The whole group; the command alone where it has no group of its own.
    process.kill(-child.pid, signal);
  } catch {
    child.kill(signal);
  }
}

function stopNow() {
  stop("SIGTERM");
  // For whatever ignores the request.
  forced ??= setTimeout(() => stop("SIGKILL"), 10_000);
}

const timer = setTimeout(() => {
  timedOut = true;
  stopNow();
}, limit * 1000);

let signalled = 0;
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => {
    signalled = 128 + constants.signals[signal];
    stopNow();
  });
}

child.on("error", (error) => {
  console.error(`run-bounded: cannot start ${command}: ${error.message}`);
  process.exit(127);
});
child.on("exit", (code, signal) => {
  clearTimeout(timer);
  // Anything the command left behind in its group goes with it.
  if (timedOut || signalled) stop("SIGKILL");
  clearTimeout(forced);
  if (timedOut) process.exit(124);
  if (signalled) process.exit(signalled);
  process.exit(code ?? 128 + (constants.signals[signal] ?? 0));
});
