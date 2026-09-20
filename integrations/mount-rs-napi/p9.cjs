"use strict"

// The package's ./9p entrypoint is deliberately a shallow facade over the
// normal binding.  Installing the codec aliases on a copy keeps the root
// namespace (and its NFS codec/server names) unchanged while retaining the
// existing P9 server exports.
const root = require("./index.js")
const binding = { ...root }

require("./postlude-p9-codec.cjs")(binding)

module.exports = binding
// Preserve statically discoverable named exports for Node's ESM interop.
module.exports.createP9Server = binding.createP9Server
module.exports.P9Server = binding.P9Server
module.exports.P9Connection = binding.P9Connection
module.exports.P9Session = binding.P9Session
