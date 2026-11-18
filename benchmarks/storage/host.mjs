// Host conditions around a timed window: kernel pressure-stall totals and the
// page cache's dirty and writeback backlog. Linux exposes both under /proc;
// elsewhere every field is null. Reading them takes well under a millisecond
// and happens outside the timed window.
import { execFile } from "node:child_process"
import { readFile } from "node:fs/promises"
import { promisify } from "node:util"

import { errorRecord } from "./errors.mjs"

const execFileAsync = promisify(execFile)
const PRESSURE_RESOURCES = Object.freeze(["io", "cpu", "memory"])

/** `some`/`full` cumulative stall totals, in microseconds, from a PSI file. */
export function parsePressure(text) {
  const totals = { some: null, full: null }
  for (const line of String(text).split("\n")) {
    const match = /^(some|full)\b.*\btotal=(\d+)/.exec(line.trim())
    if (match) totals[match[1]] = Number(match[2])
  }
  return totals
}

/** Dirty and Writeback page-cache sizes, in KiB, from /proc/meminfo. */
export function parseMeminfo(text) {
  const fields = { dirtyKiB: null, writebackKiB: null }
  for (const line of String(text).split("\n")) {
    const match = /^(Dirty|Writeback):\s+(\d+)\s+kB/.exec(line)
    if (match) fields[match[1] === "Dirty" ? "dirtyKiB" : "writebackKiB"] = Number(match[2])
  }
  return fields
}

async function readOptional(path, read) {
  try {
    return await read(path)
  } catch {
    return null
  }
}

const readText = (path) => readFile(path, "utf8")

/** One reading of the host's pressure totals and page-cache backlog. */
export async function readHostConditions(read = readText) {
  const pressure = {}
  for (const resource of PRESSURE_RESOURCES) {
    const text = await readOptional(`/proc/pressure/${resource}`, read)
    pressure[resource] = text === null ? null : parsePressure(text)
  }
  const meminfo = await readOptional("/proc/meminfo", read)
  return { pressure, ...(meminfo === null ? parseMeminfo("") : parseMeminfo(meminfo)) }
}

function stallMs(before, after, resource, kind) {
  const start = before.pressure[resource]?.[kind]
  const end = after.pressure[resource]?.[kind]
  if (!Number.isFinite(start) || !Number.isFinite(end)) return null
  return Math.max(0, end - start) / 1000
}

/**
 * What the host went through between two readings: milliseconds during which
 * some (or all) runnable tasks were stalled on I/O, CPU or memory, and the
 * page-cache backlog at each end. A stall here that matches a slow window in
 * the samples points at the host rather than the provider.
 */
export function hostConditionsDelta(before, after) {
  return {
    source: "linux /proc/pressure and /proc/meminfo; null where unavailable",
    ioSomeStallMs: stallMs(before, after, "io", "some"),
    ioFullStallMs: stallMs(before, after, "io", "full"),
    cpuSomeStallMs: stallMs(before, after, "cpu", "some"),
    memorySomeStallMs: stallMs(before, after, "memory", "some"),
    dirtyKiBBefore: before.dirtyKiB,
    writebackKiBBefore: before.writebackKiB,
    dirtyKiBAfter: after.dirtyKiB,
    writebackKiBAfter: after.writebackKiB,
  }
}

/**
 * Write the page cache's dirty backlog to disk before a timed window, so the
 * window does not also pay for writeback of what earlier steps wrote (build
 * output, container layers). Every synchronous commit inside the window
 * (SQLite with synchronous=FULL, for one) otherwise waits behind that
 * backlog. Only data already written is flushed; nothing the measured
 * provider does later is excluded.
 */
export async function flushHostWriteback({
  read = readText,
  sync = () => execFileAsync("sync", []),
  now = () => performance.now(),
} = {}) {
  const before = await readHostConditions(read)
  const started = now()
  try {
    await sync()
  } catch (error) {
    return {
      status: "failed",
      syncMs: now() - started,
      dirtyKiBBefore: before.dirtyKiB,
      writebackKiBBefore: before.writebackKiB,
      error: errorRecord(error),
    }
  }
  const syncMs = now() - started
  const after = await readHostConditions(read)
  return {
    status: "ok",
    syncMs,
    ioFullStallMsDuringSync: stallMs(before, after, "io", "full"),
    dirtyKiBBefore: before.dirtyKiB,
    writebackKiBBefore: before.writebackKiB,
    dirtyKiBAfter: after.dirtyKiB,
    writebackKiBAfter: after.writebackKiB,
  }
}

/**
 * The slowest operations of a run, each with when it started relative to
 * the start of the timed window, so a stall shows up as a cluster of
 * operations that started apart and ended together.
 */
export function slowestOperations(samples, limit = 8) {
  const operations = []
  for (const sample of samples) {
    for (const operation of ["write", "read", "delete"]) {
      const startedAtMs = sample[`${operation}StartedAtMs`]
      const ms = sample[`${operation}Ms`]
      if (!Number.isFinite(startedAtMs) || !Number.isFinite(ms)) continue
      operations.push({
        iteration: sample.iteration,
        operation,
        startedAtMs,
        endedAtMs: startedAtMs + ms,
        ms,
      })
    }
  }
  operations.sort((left, right) => right.ms - left.ms || left.startedAtMs - right.startedAtMs)
  return operations.slice(0, limit)
}
