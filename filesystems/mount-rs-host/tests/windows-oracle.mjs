// Live platform oracle: Node's own Win32/libuv implementation, never a mock.
import * as fs from "node:fs"
const [operation, path, modeArg] = process.argv.slice(2)
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
} else if (operation === "read-create-mode") {
  const mode = Number.parseInt(modeArg, 8)
  const fd = fs.openSync(path, fs.constants.O_RDONLY | fs.constants.O_CREAT, mode)
  let code
  try { fs.writeSync(fd, Buffer.from("x")) } catch (error) { code = error.code }
  const stats = fs.fstatSync(fd)
  fs.closeSync(fd)
  console.log(`${stats.size},${code},${stats.mode & 0o777}`)
} else if (operation === "hard-links") {
  fs.writeFileSync(path, "payload")
  const first = `${path}.first`
  const second = `${path}.second`
  const moved = `${path}.moved`
  fs.linkSync(path, first)
  fs.linkSync(path, second)
  const before = [path, first, second].map((entry) => fs.statSync(entry))
  fs.unlinkSync(first)
  const afterUnlink = fs.statSync(path)
  fs.renameSync(path, moved)
  const afterRename = fs.statSync(moved)
  console.log([
    before.every((stats) => stats.ino === before[0].ino),
    ...before.map((stats) => stats.nlink),
    afterUnlink.nlink,
    afterRename.nlink,
    afterRename.ino === before[0].ino,
  ].join(","))
  fs.unlinkSync(second)
  fs.unlinkSync(moved)
} else if (operation === "readonly-hard-links") {
  const fd = fs.openSync(path, fs.constants.O_RDONLY | fs.constants.O_CREAT, 0o400)
  let writeCode
  try { fs.writeSync(fd, Buffer.from("x")) } catch (error) { writeCode = error.code }
  const first = `${path}.first`
  fs.linkSync(path, first)
  const before = [fs.statSync(path), fs.statSync(first)]
  fs.unlinkSync(first)
  const afterAlias = fs.fstatSync(fd)
  fs.unlinkSync(path)
  const afterFinal = fs.fstatSync(fd)
  fs.closeSync(fd)
  let missing
  try { fs.statSync(path) } catch (error) { missing = error.code }
  console.log([
    writeCode,
    ...before.map((stats) => stats.nlink),
    afterAlias.nlink,
    afterFinal.nlink,
    missing,
  ].join(","))
} else if (operation === "handle-lifecycle") {
  const fd = fs.openSync(path, "w+")
  fs.writeSync(fd, Buffer.from("a"))
  const moved = `${path}.moved`
  fs.renameSync(path, moved)
  fs.writeSync(fd, Buffer.from("b"))
  const data = fs.readFileSync(moved, "utf8")
  fs.unlinkSync(moved)
  const size = fs.fstatSync(fd).size
  fs.closeSync(fd)
  let missing
  try { fs.statSync(moved) } catch (error) { missing = error.code }
  console.log(`${data},${size},${missing}`)
} else throw new Error(`Unknown oracle operation: ${operation}`)
