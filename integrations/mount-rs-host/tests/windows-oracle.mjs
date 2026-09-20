// Live platform oracle: Node's own Win32/libuv implementation, never a mock.
import * as fs from "node:fs"
const [operation, path] = process.argv.slice(2)
if (operation === "mode") console.log(fs.statSync(path).mode & 0o777)
else if (operation === "stat") {
  const s = fs.statSync(path, { bigint: true })
  console.log([s.dev, s.ino, s.nlink, s.mode, s.size, s.atimeMs,
    s.mtimeMs, s.ctimeMs, s.birthtimeMs, s.blocks].join(","))
} else if (operation === "statfs") {
  const s = fs.statfsSync(path)
  console.log([s.type, s.bsize, s.blocks, s.files, s.ffree].join(","))
} else if (operation === "chown") {
  fs.chownSync(path, 123, 456)
  console.log("ok")
} else if (operation === "symlinks") {
  fs.symlinkSync("directory", `${path}/node-inferred`)
  fs.symlinkSync("later", `${path}/node-dangling-dir`, "dir")
  fs.symlinkSync("later-file", `${path}/node-dangling-file`, "file")
  console.log("ok")
} else if (operation === "read-create") {
  const fd = fs.openSync(path, fs.constants.O_RDONLY | fs.constants.O_CREAT, 0o600)
  let code
  try { fs.writeSync(fd, Buffer.from("x")) } catch (error) { code = error.code }
  const size = fs.fstatSync(fd).size
  fs.closeSync(fd)
  console.log(`${size},${code}`)
} else throw new Error(`Unknown oracle operation: ${operation}`)
