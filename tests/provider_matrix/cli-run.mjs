import { spawn } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

export function run(command, args, timeoutMs = 120_000) {
  const useProcessGroup = process.platform !== "win32";
  const child = spawn(command, args, {
    cwd: repositoryRoot,
    env: { ...process.env },
    detached: useProcessGroup,
    stdio: ["ignore", "pipe", "pipe"],
  });
  return waitForChild(child, timeoutMs, { useProcessGroup });
}

export function waitForChild(
  child,
  timeoutMs = 120_000,
  {
    useProcessGroup = false,
    signalProcess = (pid, signal) => process.kill(pid, signal),
  } = {},
) {
  return new Promise((resolveResult) => {
    let timedOut = false;
    let settled = false;
    let killTimer;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      clearTimeout(killTimer);
      resolveResult(result);
    };
    const signalChild = (signal) => {
      if (useProcessGroup && child.pid) {
        try {
          signalProcess(-child.pid, signal);
          return;
        } catch (error) {
          if (error.code !== "ESRCH" && error.code !== "EPERM") throw error;
          // A platform or sandbox may deny group signals; stop the direct child.
        }
      }
      child.kill(signal);
    };
    const timer = setTimeout(() => {
      timedOut = true;
      signalChild("SIGTERM");
      killTimer = setTimeout(() => {
        signalChild("SIGKILL");
        child.stdout?.destroy();
        child.stderr?.destroy();
        finish({ ok: false, reason: "timeout" });
      }, 1_000);
    }, timeoutMs);
    // Drain without retaining or logging output: a full pipe blocks the child.
    child.stdout?.resume();
    child.stderr?.resume();
    child.on("error", () => {
      if (!timedOut) finish({ ok: false, reason: "spawn" });
    });
    child.on("close", (code) => {
      if (timedOut) return;
      finish({
        ok: code === 0,
        reason: "exit-" + String(code),
      });
    });
  });
}
