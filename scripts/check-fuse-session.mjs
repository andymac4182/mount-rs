import {execFileSync} from 'node:child_process';
import {pathToFileURL, fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error('MOUNTX_SOURCE is required');
const {FuseSession} = await import(pathToFileURL(`${source}/src/fuse/session.ts`));
const {createMemoryDriver} = await import(pathToFileURL(`${source}/src/drivers/memory.ts`));
const fs = createMemoryDriver({uid: 0, gid: 0});
const file = await fs.open('/file', 'w', 0o644);
await file.write(Buffer.from('abcdef'), 0, 6, 0);
await file.close();
const flags = (1 << 0) | (1 << 3) | (1 << 5) | (1 << 12) | (1 << 22);
const session = new FuseSession(fs, {init: {flags: BigInt(flags), maxWrite: 1048496}});
const messages = [];
function frame(opcode, node, body = Buffer.alloc(0)) {
  const header = Buffer.alloc(40);
  header.writeUInt32LE(40 + body.length); header.writeUInt32LE(opcode, 4);
  header.writeBigUInt64LE(BigInt(messages.length + 1), 8); header.writeBigUInt64LE(BigInt(node), 16);
  messages.push(Buffer.concat([header, body]));
}
function words(...values) {const b = Buffer.alloc(values.length * 4); values.forEach((v,i) => b.writeUInt32LE(v,i*4)); return b;}
function io(handle, offset, size) {const b = Buffer.alloc(40); b.writeBigUInt64LE(BigInt(handle)); b.writeBigUInt64LE(BigInt(offset),8); b.writeUInt32LE(size,16); return b;}
frame(3, 1, Buffer.alloc(16)); // pre-init error
frame(26, 0, words(7,41,65536,flags));
frame(1, 1, Buffer.from('file\0'));
frame(14, 2, words(2,0));
frame(15,2,Buffer.alloc(20)); // truncated read body
frame(16,2,Buffer.concat([io(1,0,1),Buffer.from('too much')]));
frame(10,1,Buffer.from('file\0trailing')); // must not unlink
frame(18,2,Buffer.alloc(8)); // must not release
frame(25,2,Buffer.alloc(24)); // memfs validates unknown flush handles
const mknod = Buffer.alloc(16); mknod.writeUInt32LE(0o10600);
frame(8,1,Buffer.concat([mknod,Buffer.from('fifo\0')]));
messages.at(-1).writeUInt32LE(1234,24); messages.at(-1).writeUInt32LE(5678,28);
frame(2,2,Buffer.alloc(3)); // malformed FORGET still has no reply
frame(16, 2, Buffer.concat([io(1,1,2),Buffer.from('XY')]));
frame(15, 2, io(1,0,20));
const setattr = Buffer.alloc(88);
setattr.writeUInt32LE(1 | 2 | 8 | 16 | 32 | 64);
setattr.writeBigUInt64LE(1n,8); setattr.writeBigUInt64LE(4n,16);
setattr.writeBigInt64LE(-315619200n,32); setattr.writeBigUInt64LE(1234567890n,40);
setattr.writeUInt32LE(123000000,56); setattr.writeUInt32LE(456000000,60);
setattr.writeUInt32LE(0o600,68); setattr.writeUInt32LE(123,76);
frame(4,2,setattr);
frame(15,2,io(1,0,20));
frame(27, 1, words(0,0));
frame(28, 1, io(2,0,31));
frame(28, 1, io(2,0,32));
frame(28, 1, io(2,1,32));
frame(28, 1, io(2,2,32));
frame(28, 1, io(2,3,32));
frame(44, 1, io(2,0,159)); // no partial entry or lookup reference
frame(44, 1, io(2,0,160));
frame(44, 1, io(2,2,320));
const releaseDir = Buffer.alloc(24); releaseDir.writeBigUInt64LE(2n); frame(29,1,releaseDir);
frame(10,1,Buffer.from('file\0'));
frame(3,2,Buffer.alloc(16)); // orphan metadata without fh
const orphanSize = Buffer.alloc(88); orphanSize.writeUInt32LE(8); orphanSize.writeBigUInt64LE(2n,16);
frame(4,2,orphanSize);
frame(15,2,io(1,0,20));
frame(18, 2, Buffer.alloc(24, 0)); // invalid handle
const release = Buffer.alloc(24); release.writeBigUInt64LE(1n); frame(18,2,release);
frame(15, 2, io(1,0,20));
frame(38, 0);
frame(3, 1, Buffer.alloc(16));
const expected = [];
for (const message of messages) expected.push(await session.handleMessage(message));
const actual = execFileSync('cargo',['run','--quiet','--locked','-p','mount-rs-fuse','--example','session_oracle'],{
  cwd: fileURLToPath(new URL('..',import.meta.url)), input: messages.map(b=>b.toString('hex')).join('\n')+'\n', encoding:'utf8',
}).trim().split('\n').map(s=>s==='none'?null:Buffer.from(s,'hex'));
assert.equal(actual.length, expected.length);
for (let i=0;i<actual.length;i++) {
  const normalize = input => {
    if (!input) return input;
    const b = Buffer.from(input);
    // Mask only the three wall-clock timestamps (seconds and nanoseconds).
    if ([1,8].includes(messages[i].readUInt32LE(4)) && b.readInt32LE(4) === 0) b.fill(0,80,116);
    if (messages[i].readUInt32LE(4) === 44 && b.readInt32LE(4) === 0) {
      for (let at = 16; at < b.length;) {
        b.fill(0,at+64,at+100);
        const nameLength = b.readUInt32LE(at+144);
        at += 128 + ((24 + nameLength + 7) & ~7);
      }
    }
    // SETATTR sets deterministic atime/mtime; only ctime remains wall-clock.
    if (messages[i].readUInt32LE(4) === 4 && b.readInt32LE(4) === 0) { b.fill(0,72,80); b.fill(0,88,92); }
    // Reads update atime on each independent run.
    if (messages[i].readUInt32LE(4) === 3 && b.readInt32LE(4) === 0) { b.fill(0,56,64); b.fill(0,80,84); b.fill(0,72,80); b.fill(0,88,92); }
    if (messages[i].readUInt32LE(4) === 4 && messages[i].readUInt32LE(40) === 8 && b.readInt32LE(4) === 0) { b.fill(0,56,72); b.fill(0,80,88); }
    return b;
  };
  assert.deepEqual(normalize(actual[i]), normalize(expected[i]), `opcode ${messages[i].readUInt32LE(4)} at ${i}`);
}
console.log(`FUSE session TypeScript parity: PASS (${messages.length} messages)`);
