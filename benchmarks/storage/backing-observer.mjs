import { createHash } from "node:crypto"

const UINT64_MAX = 18_446_744_073_709_551_615n
const LABEL_KEYS = new Set(["mount-rs.tidb.run", "mount-rs.foundationdb.run", "com.mount-rs.ozone-test", "com.mount-rs.ozone-test-run"])
const HARD_CAPS = Object.freeze({ maxContainers: 16, maxRequests: 257, maxBoundaries: 64, maxResponseBytes: 1_048_576, maxDeviceEntries: 128, maxJournalBytes: 8_388_608, requestTimeoutMs: 2000, ownerTimeoutMs: 60_000 })
const METRICS = ["cpu_usage_ns", "block_bytes", "block_operations"]
const LIMIT_KEYS = ["Memory", "MemorySwap", "NanoCpus", "CpuQuota", "CpuPeriod", "CpuShares", "PidsLimit"]
// Exact factory identity, original projections and session ownership stay private.
const FACTORY_OBSERVERS = new WeakMap()

class ObserverFault extends Error {
  constructor(code) { super(code); this.code = code }
}
const fault = (code) => { throw new ObserverFault(code) }
const issueCode = (error) => error instanceof ObserverFault ? error.code : "observer_transport_failure"
const defaultClock = () => ({ now: () => performance.now(), utc: () => new Date().toISOString(), cpu: () => process.cpuUsage(), setTimeout, clearTimeout })

function validateClock(clock) {
  for (const method of ["now", "utc", "cpu", "setTimeout", "clearTimeout"]) {
    if (typeof clock?.[method] !== "function") fault("observer_clock")
  }
  if (!Number.isFinite(clock.now())) fault("observer_clock")
  return clock
}
function name(value) {
  if (typeof value !== "string" || !/^[a-zA-Z0-9_.:-]{1,120}$/.test(value)) fault("observer_name")
  return value
}
function uint(value) {
  if (typeof value !== "string" || !/^(0|[1-9]\d*)$/.test(value) || value.length > 20 || BigInt(value) > UINT64_MAX) fault("invalid_counter")
  return value
}
function optionalUint(value) { return value === undefined || value === null ? null : uint(value) }
function timestamp(value) {
  if (typeof value !== "string") fault("read_timestamp")
  const match = /^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,9}))?Z$/.exec(value)
  const ms = match && Date.parse(`${match[1]}Z`)
  if (!match || !Number.isFinite(ms) || new Date(ms).toISOString().slice(0, 19) !== match[1]) fault("read_timestamp")
  return (BigInt(ms) * 1_000_000n + BigInt((match[2] || "").padEnd(9, "0"))).toString()
}

/** Node24 exposes each original numeric token before Number rounding matters. */
export function parseEngineJSON(text) {
  if (typeof text !== "string") fault("response_json")
  try {
    return JSON.parse(text, (_key, value, context) => {
      if (typeof value !== "number") return value
      if (typeof context?.source !== "string") fault("lossless_json_runtime_unavailable")
      return context.source
    })
  } catch (error) {
    if (error instanceof ObserverFault) throw error
    fault("response_json")
  }
}

function allowlistEntries(allowlist, maxContainers = HARD_CAPS.maxContainers) {
  if (!Array.isArray(allowlist) || allowlist.length < 1 || allowlist.length > maxContainers) fault("allowlist_size")
  const cids = new Set(), roles = new Set()
  return allowlist.map((entry) => {
    if (!/^[a-f0-9]{64}$/.test(entry?.cid || "") || cids.has(entry.cid)) fault("allowlist_cid")
    const role = name(entry.role)
    if (roles.has(role)) fault("allowlist_role")
    const labels = Object.entries(entry.labels || {})
    if (labels.length < 1 || labels.length > LABEL_KEYS.size) fault("ownership_labels")
    for (const [key, value] of labels) {
      if (!LABEL_KEYS.has(key) || typeof value !== "string" || !/^[a-zA-Z0-9_.:-]{1,160}$/.test(value)) fault("ownership_labels")
    }
    cids.add(entry.cid); roles.add(role)
    return { cid: entry.cid, role, labels: Object.fromEntries(labels.sort(([a], [b]) => a.localeCompare(b))) }
  })
}

/** Projection happens before any response-derived object reaches a journal. */
export function projectInspect(input, expected) {
  expected = allowlistEntries([expected])[0]
  if (input?.Id !== expected.cid || !/^sha256:[a-f0-9]{64}$/.test(input.Image || "")) fault("inspect_identity")
  if (input.State?.Running !== true) fault("container_not_running")
  timestamp(input.State.StartedAt)
  const labels = {}
  for (const [key, value] of Object.entries(expected.labels)) {
    if (input.Config?.Labels?.[key] !== value) fault("ownership_mismatch")
    labels[key] = value
  }
  const limits = {}
  for (const key of LIMIT_KEYS) {
    const value = input.HostConfig?.[key]
    limits[key] = value === undefined || value === null ? null : ["MemorySwap", "CpuQuota", "PidsLimit"].includes(key) && value === "-1" ? "-1" : uint(value)
  }
  const cpuset = input.HostConfig?.CpusetCpus
  if (cpuset !== undefined && (typeof cpuset !== "string" || !/^[0-9,-]{0,160}$/.test(cpuset))) fault("inspect_limits")
  limits.CpusetCpus = cpuset ?? null
  return { cid: expected.cid, role: expected.role, labels, image: input.Image, running: true, started_at: input.State.StartedAt, restart_count: uint(input.RestartCount), limits, configured_limit_semantics: "field_specific_zero_unset_and_minus_one_unlimited; not_observed_capacity" }
}

function deviceCounters(entries, maxEntries) {
  if (entries === undefined || entries === null || Array.isArray(entries) && entries.length === 0) return null
  if (!Array.isArray(entries) || entries.length > maxEntries) fault("device_entries_cap")
  const selected = new Map()
  for (const entry of entries) {
    if (!["Read", "Write"].includes(entry?.op)) continue
    const key = `${uint(entry.major)}:${uint(entry.minor)}:${entry.op}`
    if (selected.has(key)) fault("duplicate_device_key")
    selected.set(key, uint(entry.value))
  }
  return selected.size ? Object.fromEntries([...selected].sort(([a], [b]) => a.localeCompare(b))) : null
}

export function projectStats(input, cid, maxEntries = HARD_CAPS.maxDeviceEntries) {
  if (input?.id !== cid) fault("stats_identity")
  const cpu = input.cpu_stats || {}
  return {
    cid, read: input.read, read_ns: timestamp(input.read),
    cpu_usage_ns: optionalUint(cpu.cpu_usage?.total_usage),
    cpu_user_ns: optionalUint(cpu.cpu_usage?.usage_in_usermode),
    cpu_kernel_ns: optionalUint(cpu.cpu_usage?.usage_in_kernelmode),
    system_cpu_usage_ns: optionalUint(cpu.system_cpu_usage), online_cpus: optionalUint(cpu.online_cpus),
    throttling: { periods: optionalUint(cpu.throttling_data?.periods), throttled_periods: optionalUint(cpu.throttling_data?.throttled_periods), throttled_ns: optionalUint(cpu.throttling_data?.throttled_time) },
    memory: { usage_bytes: optionalUint(input.memory_stats?.usage), limit_bytes: optionalUint(input.memory_stats?.limit), semantics: "api_gauges; not_cli_cache_adjusted" },
    block_bytes: deviceCounters(input.blkio_stats?.io_service_bytes_recursive, maxEntries),
    block_operations: deviceCounters(input.blkio_stats?.io_serviced_recursive, maxEntries),
  }
}

function subtract(before, after) {
  if (before === null || before === undefined || after === null || after === undefined) fault("metric_unavailable")
  const delta = BigInt(uint(after)) - BigInt(uint(before))
  if (delta < 0n) fault("counter_reset")
  return delta.toString()
}
function metricDelta(before, after, metric, elapsed) {
  if (metric === "cpu_usage_ns") {
    const value = subtract(before[metric], after[metric])
    return { complete: true, value, rate: { numerator_ns: value, denominator_ns: elapsed } }
  }
  const first = before[metric], last = after[metric]
  if (!first || !last) fault("metric_unavailable")
  const keys = Object.keys(first)
  if (JSON.stringify(keys) !== JSON.stringify(Object.keys(last))) fault("device_keys_changed")
  const devices = Object.fromEntries(keys.map((key) => [key, subtract(first[key], last[key])]))
  const value = Object.values(devices).reduce((sum, item) => sum + BigInt(item), 0n).toString()
  return { complete: true, value, devices, rate: { numerator: value, denominator_ns: elapsed, seconds_scale: "1000000000" } }
}
function indexedSamples(boundary, allowed) {
  const index = new Map()
  if (!Array.isArray(boundary?.samples)) fault("boundary_samples")
  for (const sample of boundary.samples) {
    if (!allowed.has(sample.cid) || index.has(sample.cid)) fault("duplicate_or_foreign_boundary_member")
    index.set(sample.cid, sample)
  }
  return index
}
function skew(boundary) {
  const windows = (boundary?.samples || []).map((sample) => sample.stats_window).filter(Boolean)
  const reads = (boundary?.samples || []).map((sample) => sample.stats?.read_ns).filter((value) => value !== undefined)
  const sorted = reads.map(BigInt).sort((a, b) => a < b ? -1 : a > b ? 1 : 0)
  return {
    local_enclosing_ms: windows.length ? { dispatch: Math.min(...windows.map((item) => item.dispatch_ms)), response: Math.max(...windows.map((item) => item.response_ms)) } : null,
    daemon_read_skew_ns: sorted.length ? (sorted.at(-1) - sorted[0]).toString() : null,
    semantics: "endpoint_skew; not_workload_duration",
  }
}

/** Missing members invalidate only their metric totals; partial values stay separate. */
export function summarizeInterval(allowlist, before, after, metadata = {}) {
  const entries = allowlistEntries(allowlist)
  const allowed = new Set(entries.map((entry) => entry.cid))
  const first = indexedSamples(before, allowed), last = indexedSamples(after, allowed)
  const containers = entries.map((entry) => {
    const start = first.get(entry.cid), end = last.get(entry.cid)
    const metrics = {}
    for (const metric of METRICS) {
      try {
        if (!start?.stats || !end?.stats) fault("member_unavailable")
        if (start.identity && end.identity && JSON.stringify(start.identity) !== JSON.stringify(end.identity)) fault("identity_drift")
        const elapsed = BigInt(end.stats.read_ns) - BigInt(start.stats.read_ns)
        if (elapsed <= 0n) fault("nonincreasing_read_timestamp")
        metrics[metric] = metricDelta(start.stats, end.stats, metric, elapsed.toString())
      } catch (error) { metrics[metric] = { complete: false, value: null, issue: issueCode(error) } }
    }
    return { cid: entry.cid, role: entry.role, metrics }
  })
  const metrics = Object.fromEntries(METRICS.map((metric) => {
    const missing = containers.filter((entry) => !entry.metrics[metric].complete).map((entry) => entry.cid)
    const partial = containers.reduce((sum, entry) => sum + BigInt(entry.metrics[metric].value ?? "0"), 0n).toString()
    return [metric, { complete: missing.length === 0, total: missing.length ? null : partial, partial_total: partial, missing_members: missing }]
  }))
  return { schema: "mount-rs.backing-interval.v1", kind: metadata.kind === "idle" ? "idle" : "phase", complete: Object.values(metrics).every((metric) => metric.complete), workload_elapsed_ms: Number.isFinite(metadata.workload_elapsed_ms) ? metadata.workload_elapsed_ms : null, attribution: "descriptive enclosing container accounting; no idle subtraction or physical-device attribution", containers, metrics, endpoints: { before: skew(before), after: skew(after) }, cpu_percentage_aggregate: "unavailable_for_different_windows" }
}

async function deadlineCall(operation, clock, deadline, controller, parentSignal) {
  if (clock.now() >= deadline) { controller.abort(); fault("request_deadline") }
  let timer, onAbort
  const cancellation = new Promise((_resolve, reject) => {
    timer = clock.setTimeout(() => { controller.abort(); reject(new ObserverFault("request_deadline")) }, deadline - clock.now())
    if (parentSignal) {
      onAbort = () => { controller.abort(); reject(new ObserverFault("observer_aborted")) }
      if (parentSignal.aborted) onAbort()
      else parentSignal.addEventListener("abort", onAbort, { once: true })
    }
  })
  try {
    const value = await Promise.race([Promise.resolve().then(operation), cancellation])
    if (clock.now() > deadline) { controller.abort(); fault("request_deadline") }
    return value
  } finally {
    clock.clearTimeout(timer)
    if (onAbort) parentSignal.removeEventListener("abort", onAbort)
  }
}

function terminalProjection(metadata, complete) {
  const native = typeof metadata.native_quiescent === "boolean" ? metadata.native_quiescent : null
  const owned = metadata.owned_operations_settled === true
  const workload = typeof metadata.workload_native_evidence_complete === "boolean" ? metadata.workload_native_evidence_complete : null
  const enabled = metadata.native_profiling_enabled === true
  const cleanup = metadata.cleanup_complete === true
  const scope = ["runner_workload_native_diagnostics_and_owned_operation_settlement", "runner_owned_operations_only; native_workload_proof_unobserved"].includes(metadata.quiescence_scope)
    ? metadata.quiescence_scope : "caller_supplied_quiescence; not_independently_verified"
  return { status: ["ok", "failed", "skipped"].includes(metadata.status) ? metadata.status : "unknown", native_quiescent: native, owned_operations_settled: owned, native_profiling_enabled: enabled, workload_native_evidence_complete: workload, operation_deadline_failed: metadata.operation_deadline_failed === true, cleanup_complete: cleanup, quiescence_scope: scope, prior_native_uncertainty: metadata.prior_native_uncertainty === true, safe_to_continue_pair: complete && native === true && owned && enabled && workload === true && cleanup && metadata.operation_deadline_failed !== true && metadata.prior_native_uncertainty !== true }
}

/** No I/O exists until the caller explicitly supplies a transport and invokes a hook. */
export function createBackingObserver({ allowlist, transport, clock = defaultClock(), caps = {} }) {
  clock = validateClock(clock)
  if (typeof transport?.request !== "function") fault("observer_transport")
  const limits = { ...HARD_CAPS }
  for (const [key, value] of Object.entries(caps)) {
    if (!Object.hasOwn(HARD_CAPS, key) || !Number.isSafeInteger(value) || value < 1 || value > HARD_CAPS[key] || key === "maxJournalBytes" && value < 8192) fault("observer_cap")
    limits[key] = value
  }
  const expected = allowlistEntries(allowlist, limits.maxContainers)
  const reservedBytes = Buffer.byteLength(JSON.stringify({ allowlist: expected, caps: limits })) + 4096
  if (reservedBytes > limits.maxJournalBytes) fault("receipt_base_cap")
  const started = clock.now(), ownerDeadline = started + limits.ownerTimeoutMs
  const identities = new Map(), active = new Map(), lastStats = new Map(), phases = new Map()
  const issues = new Set(), journal = []
  const cost = { requests: 0, response_bytes: 0, wall_ms: 0, cpu_user_us: 0, cpu_system_us: 0 }
  let version, closed = false, stopped = false, boundaries = 0, droppedEntries = 0, journalBytes = 0, terminal, finalizedAt, finalReceipt
  let claimedBy, finalizedBy
  const issue = (error) => { issues.add(issueCode(error)) }
  const append = (entry) => {
    const bytes = Buffer.byteLength(JSON.stringify(entry)) + 1
    if (journalBytes + bytes > limits.maxJournalBytes - reservedBytes) { issues.add("journal_bytes_cap"); droppedEntries += 1; return false }
    journal.push(entry); journalBytes += bytes
    return true
  }
  async function request(path, cid, parentSignal) {
    const key = cid || "version"
    if (closed || stopped) fault("observer_stopped")
    if (active.has(key)) fault("request_overlap")
    if (cost.requests >= limits.maxRequests) { stopped = true; fault("request_count_cap") }
    const dispatch = clock.now(), cpuBefore = clock.cpu()
    const deadline = Math.min(ownerDeadline, dispatch + limits.requestTimeoutMs)
    if (dispatch >= deadline) { stopped = true; fault("owner_deadline") }
    const controller = new AbortController()
    cost.requests += 1
    const handle = { controller, utc: clock.utc() }
    active.set(key, handle)
    let work
    try {
      const value = await deadlineCall(() => {
        work = (async () => {
          const response = await transport.request({ method: "GET", path, cid, signal: controller.signal, maxResponseBytes: limits.maxResponseBytes })
          if (controller.signal.aborted) fault("observer_aborted")
          if (response?.status !== 200 || !response.body?.[Symbol.asyncIterator]) fault("response_status_or_body")
          const chunks = [], hash = createHash("sha256")
          let bytes = 0
          for await (const chunk of response.body) {
            if (controller.signal.aborted) fault("observer_aborted")
            if (clock.now() > deadline) { controller.abort(); fault("request_deadline") }
            if (!(chunk instanceof Uint8Array)) fault("response_chunk")
            bytes += chunk.byteLength; cost.response_bytes += chunk.byteLength
            if (bytes > limits.maxResponseBytes) { controller.abort(); fault("response_bytes_cap") }
            hash.update(chunk); chunks.push(Buffer.from(chunk))
          }
          return { value: parseEngineJSON(Buffer.concat(chunks).toString("utf8")), sha256: hash.digest("hex"), bytes }
        })()
        work.then(() => { if (active.get(key) === handle) active.delete(key) }, () => { if (active.get(key) === handle) active.delete(key) })
        return work
      }, clock, deadline, controller, parentSignal)
      return { ...value, window: { dispatch_ms: dispatch, response_ms: clock.now(), dispatch_utc: handle.utc, response_utc: clock.utc() } }
    } catch (error) {
      controller.abort()
      if (!work) active.delete(key)
      if (["request_deadline", "owner_deadline", "response_bytes_cap", "observer_aborted"].includes(issueCode(error))) stopped = true
      throw error
    } finally {
      const cpuAfter = clock.cpu()
      cost.wall_ms += Math.max(0, clock.now() - dispatch)
      cost.cpu_user_us += Math.max(0, cpuAfter.user - cpuBefore.user)
      cost.cpu_system_us += Math.max(0, cpuAfter.system - cpuBefore.system)
    }
  }
  async function negotiate(signal) {
    if (version) return
    const response = await request("/version", undefined, signal)
    const input = response.value
    const parse = (value) => { const match = /^1\.(\d{2})$/.exec(value || ""); if (!match) fault("api_version"); return Number(match[1]) }
    const maximum = Math.min(51, parse(input.ApiVersion)), minimum = Math.max(41, parse(input.MinAPIVersion))
    if (input.Os !== "linux") fault("unsupported_platform")
    if (minimum > maximum) fault("unsupported_api_version")
    version = `1.${maximum}`
    append({ type: "version", api_version: version, mode: "stream=false;one-shot=true", platform: "linux", body_sha256: response.sha256 })
  }
  function authorizeMutation(capability) {
    if (claimedBy && capability !== claimedBy) {
      if (!closed) issues.add("observer_mutation_outside_session")
      fault("observer_mutation_outside_session")
    }
  }
  async function captureBoundary(metadata, capability) {
    authorizeMutation(capability)
    const boundary = { id: name(metadata.id), kind: metadata.kind === "idle" ? "idle" : "phase", complete: true, samples: [], issues: [] }
    if (closed || ++boundaries > limits.maxBoundaries) {
      const code = closed ? "observer_closed" : "boundary_count_cap"
      issues.add(code); boundary.complete = false; boundary.issues.push(code)
      return boundary
    }
    try { await negotiate(metadata.signal) }
    catch (error) { issue(error); boundary.complete = false; boundary.issues.push(issueCode(error)); append({ type: "boundary", ...boundary }); return boundary }
    for (const entry of expected) {
      try {
        if (lastStats.has(entry.cid) && clock.now() - lastStats.get(entry.cid) < 1000) fault("sample_interval_cap")
        const inspected = await request(`/v${version}/containers/${entry.cid}/json`, entry.cid, metadata.signal)
        const identity = projectInspect(inspected.value, entry)
        if (identities.has(entry.cid) && JSON.stringify(identities.get(entry.cid)) !== JSON.stringify(identity)) fault("identity_drift")
        identities.set(entry.cid, identity)
        lastStats.set(entry.cid, clock.now())
        const sampled = await request(`/v${version}/containers/${entry.cid}/stats?stream=false&one-shot=true`, entry.cid, metadata.signal)
        const stats = projectStats(sampled.value, entry.cid, limits.maxDeviceEntries)
        boundary.samples.push({ cid: entry.cid, identity, stats, inspect_body_sha256: inspected.sha256, stats_body_sha256: sampled.sha256, inspect_window: inspected.window, stats_window: sampled.window })
      } catch (error) { issue(error); boundary.complete = false; boundary.issues.push(issueCode(error)) }
    }
    append({ type: "boundary", ...boundary })
    return structuredClone(boundary)
  }
  async function finalize(metadata, capability) {
    authorizeMutation(capability)
    if (closed) return receipt()
    finalizedAt = clock.now()
    if (finalizedAt > ownerDeadline) issues.add("owner_deadline")
    closed = true
    for (const { controller } of active.values()) controller.abort()
    if (active.size) issues.add("unsettled_request_at_finalization")
    if (phases.size) issues.add("unclosed_phase")
    terminal = terminalProjection(metadata, issues.size === 0 && droppedEntries === 0)
    finalReceipt = receipt()
    return receipt()
  }
  function receipt() {
    if (finalReceipt) return structuredClone(finalReceipt)
    return structuredClone({ schema: "mount-rs.backing-observer.v1", complete: issues.size === 0 && droppedEntries === 0 && Boolean(terminal), api_version: version ?? null, allowlist: expected, caps: limits, journal, journal_bytes: journalBytes, dropped_entries: droppedEntries, issues: [...issues], terminal: terminal ? { ...terminal, safe_to_continue_pair: terminal.safe_to_continue_pair && issues.size === 0 } : null, cost: { ...cost, owner_lifetime_ms: Math.max(0, (finalizedAt ?? clock.now()) - started), cpu_scope: "process_cpu_during_observer_calls; overlapping_work_not_isolated" }, daemon_overhead: "unisolated", retention: "selected_projections_and_raw_body_sha256; raw_bodies_discarded" })
  }
  const observer = {
    captureBoundary, receipt, finalize,
    async beginPhase(metadata, capability) {
      authorizeMutation(capability)
      const key = `${name(metadata.provider)}:${name(metadata.name)}`
      if (phases.has(key)) fault("phase_already_started")
      const boundary = await captureBoundary({ id: `${metadata.name}:begin`, kind: "phase", signal: metadata.signal }, capability)
      phases.set(key, boundary)
      return { complete: boundary.complete }
    },
    async endPhase(metadata, capability) {
      authorizeMutation(capability)
      const key = `${name(metadata.provider)}:${name(metadata.name)}`
      const first = phases.get(key)
      phases.delete(key)
      const last = await captureBoundary({ id: `${metadata.name}:end`, kind: "phase", signal: metadata.signal }, capability)
      if (!first) { issues.add("phase_begin_unavailable"); return { complete: false } }
      const interval = summarizeInterval(expected, first, last, { workload_elapsed_ms: metadata.measured_elapsed_ms })
      interval.native_quiescent = typeof metadata.quiescent === "boolean" ? metadata.quiescent : null
      interval.owned_operations_settled = metadata.owned_operations_settled === true
      interval.native_evidence_state = ["complete", "nonquiescent", "unavailable", "unobserved"].includes(metadata.native_evidence_state) ? metadata.native_evidence_state : "unobserved"
      if (interval.native_quiescent !== true) interval.complete = false
      if (!interval.owned_operations_settled || interval.native_evidence_state === "nonquiescent") issues.add("owned_or_native_operation_unsettled")
      append({ type: "interval", ...interval })
      return { complete: first.complete && last.complete && interval.owned_operations_settled && interval.native_evidence_state !== "nonquiescent" }
    },
  }
  const hooks = { beginPhase: observer.beginPhase, endPhase: observer.endPhase, finalize }
  FACTORY_OBSERVERS.set(observer, {
    maxReceiptBytes: limits.maxJournalBytes,
    claim(session) {
      if (claimedBy) fault("observer_provider_binding_reused")
      if (closed) fault("observer_factory_prefinalized")
      if (boundaries || cost.requests || journal.length || phases.size || active.size || identities.size || issues.size) fault("observer_factory_prepopulated")
      for (const [method, original] of Object.entries(hooks)) {
        if (observer[method] !== original) fault("observer_factory_hook_override")
      }
      claimedBy = session
    },
    async invoke(session, method, metadata) {
      if (claimedBy !== session) fault("observer_provider_binding_reused")
      if (observer[method] !== hooks[method]) fault("observer_factory_hook_override")
      if (closed) fault("observer_finalization_unproven")
      const result = await hooks[method](metadata, session)
      if (method === "finalize") {
        if (!closed) fault("observer_finalization_unproven")
        finalizedBy = session
      }
      return result
    },
    receiptFor(session) {
      if (claimedBy !== session || finalizedBy !== session || !closed) fault("observer_finalization_unproven")
      return receipt()
    },
  })
  return observer
}

/** Only a fresh, privately claimed factory can export its original bounded evidence. */
export function createRunnerObserverSession(observer, { provider, runId, clock = defaultClock(), timeoutMs = 2000 }) {
  clock = validateClock(clock)
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 2000) fault("observer_hook_timeout")
  provider = name(provider)
  runId = name(runId)
  const events = [], issues = new Set(), active = new Map()
  const registration = FACTORY_OBSERVERS.get(observer), claim = {}
  let finalized = false, terminal, backingEvidence, registrationIssue
  if (registration) {
    try { registration.claim(claim) }
    catch (error) { registrationIssue = issueCode(error); issues.add(registrationIssue) }
  }
  async function hook(method, metadata) {
    const started = clock.now(), cpuBefore = clock.cpu(), controller = new AbortController()
    const event = { hook: method, ...(metadata.name ? { phase: name(metadata.name) } : {}), ...(method === "endPhase" ? { owned_operations_settled: metadata.owned_operations_settled, native_quiescent: metadata.quiescent, native_evidence_state: metadata.native_evidence_state, pending_count: metadata.pending_count } : {}), status: "ok", started_ms: started }
    try {
      if (registrationIssue) fault(registrationIssue)
      if (active.size && method !== "finalize") fault("prior_observer_hook_pending")
      if (typeof observer?.[method] !== "function") fault("observer_hook_missing")
      const outcome = await deadlineCall(() => {
        const scoped = { ...metadata, provider, run_id: runId, signal: controller.signal }
        const pending = Promise.resolve().then(() => registration ? registration.invoke(claim, method, scoped) : observer[method](scoped))
        active.set(pending, controller)
        pending.then(() => active.delete(pending), () => active.delete(pending))
        return pending
      }, clock, started + timeoutMs, controller)
      if (outcome?.complete === false) fault("observer_hook_incomplete")
    } catch (error) { event.status = "incomplete"; event.issue = issueCode(error); issues.add(event.issue) }
    const cpuAfter = clock.cpu()
    event.wall_ms = Math.max(0, clock.now() - started)
    event.cpu_user_us = Math.max(0, cpuAfter.user - cpuBefore.user)
    event.cpu_system_us = Math.max(0, cpuAfter.system - cpuBefore.system)
    if (events.length < 131) events.push(event)
    else issues.add("hook_event_cap")
  }
  return {
    begin: (name) => hook("beginPhase", { name }),
    end(name, ownedSettled, measuredElapsedMs = null, pendingCount = 0, nativeEvidenceState = "unobserved", deadlineFailed = false) {
      const state = ["complete", "nonquiescent", "unavailable", "unobserved"].includes(nativeEvidenceState) ? nativeEvidenceState : "unobserved"
      const quiescent = !ownedSettled || state === "nonquiescent" ? false : state === "complete" && !deadlineFailed ? true : null
      return hook("endPhase", { name, quiescent, owned_operations_settled: ownedSettled === true, native_evidence_state: state, measured_elapsed_ms: measuredElapsedMs, pending_count: pendingCount })
    },
    async finalize(metadata) {
      if (finalized) return
      finalized = true
      for (const controller of active.values()) controller.abort()
      if (active.size) issues.add("unsettled_hook_at_finalization")
      await hook("finalize", metadata)
      if (registration && !registrationIssue) {
        try {
          const evidence = registration.receiptFor(claim)
          if (Buffer.byteLength(JSON.stringify(evidence)) > registration.maxReceiptBytes) fault("observer_evidence_bytes_cap")
          backingEvidence = evidence
          if (evidence.complete !== true) fault("observer_evidence_incomplete")
        } catch (error) { issues.add(issueCode(error)) }
      }
      terminal = terminalProjection(metadata, issues.size === 0)
    },
    receipt: () => structuredClone({ schema: "mount-rs.runner-backing-observer.v1", complete: finalized && issues.size === 0, events, issues: [...issues], terminal: terminal ?? null, ...(backingEvidence ? { backing_evidence: backingEvidence } : {}), observer_cost_scope: "node_process_cpu_and_wall_during_hooks; daemon_overhead_unisolated" }),
  }
}
