const fields = Object.freeze([
  "schema", "marker", "backingId", "structuralGeneration", "blockAuthorityVerified",
])

function failure(code, message) {
  return Object.assign(new Error(message), { code })
}

export function validateCompactLayoutReceipt(source) {
  if (source === null) {
    throw failure("COMPACT_LAYOUT_UNAVAILABLE", "no persisted compact layout was observed")
  }
  const invalid = () => failure("COMPACT_LAYOUT_INVALID", "compact layout receipt is invalid")
  let descriptors
  try {
    if (!source || typeof source !== "object" || Array.isArray(source)) throw invalid()
    descriptors = Object.getOwnPropertyDescriptors(source)
    if (Reflect.ownKeys(descriptors).length !== fields.length ||
        !fields.every((field) => Object.hasOwn(descriptors, field) && Object.hasOwn(descriptors[field], "value"))) {
      throw invalid()
    }
  } catch { throw invalid() }
  const value = Object.fromEntries(fields.map((field) => [field, descriptors[field].value]))
  if (value.schema !== "mount-rs.compact-layout-receipt.v1" || value.marker !== "MRC5" ||
      typeof value.backingId !== "string" || !/^[0-9a-f]{32}$/u.test(value.backingId) ||
      value.backingId === "0".repeat(32) || value.blockAuthorityVerified !== true ||
      typeof value.structuralGeneration !== "string" ||
      !/^(?:0|[1-9][0-9]{0,19})$/u.test(value.structuralGeneration) ||
      BigInt(value.structuralGeneration) > 18446744073709551615n) {
    throw invalid()
  }
  return Object.freeze(value)
}

export async function observeCompactLayout(filesystem) {
  let query
  try { query = filesystem?.inspectCompactLayout } catch {
    throw failure("COMPACT_LAYOUT_QUERY_FAILED", "compact layout query failed")
  }
  if (typeof query !== "function") {
    throw failure("COMPACT_LAYOUT_UNAVAILABLE", "compact layout inspection is unavailable")
  }
  let receipt
  try { receipt = await query.call(filesystem) } catch {
    throw failure("COMPACT_LAYOUT_QUERY_FAILED", "compact layout query failed")
  }
  return validateCompactLayoutReceipt(receipt)
}
