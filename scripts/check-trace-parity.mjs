import {execFileSync} from 'node:child_process';
import {pathToFileURL, fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error('MOUNTX_SOURCE is required');
const {createMemoryDriver} = await import(pathToFileURL(`${source}/src/drivers/memory.ts`));
const {createLoopback} = await import(pathToFileURL(`${source}/src/harness.ts`));
const seeds = (process.env.MOUNT_RS_TRACE_SEEDS ?? '4182,1,42,65535,4294967295').split(',').map(Number);
if (seeds.some(seed => !Number.isInteger(seed) || seed < 0 || seed > 0xffffffff)) {
  throw new Error('MOUNT_RS_TRACE_SEEDS must contain unsigned 32-bit integers');
}
for (const initialSeed of seeds) {
const fs = createLoopback(createMemoryDriver());
const commands = [['mkdir','/dir'],['write','/dir/file','data'],['symlink','dir','/alias']];
let seed = initialSeed;
// Use high bits: low LCG bits alternate predictably and under-exercise operations.
const next = n => {seed=(Math.imul(seed,1664525)+1013904223)>>>0;return Math.floor(seed / 0x100000000 * n);};
const paths=['/a','/b','/dir','/dir/file','/dir/a','/alias/file','/missing/x','/z','/'];
const ops=['write','mkdir','rename','link','symlink','unlink','rmdir','truncate','read','list','stat','lstat'];
for(let i=0;i<500;i++) {
  const op=ops[next(ops.length)], path=paths[next(paths.length)];
  commands.push([op,path,op==='write'?`value-${i}`:op==='truncate'?next(12):paths[next(paths.length)]]);
  if(i%10===0) commands.push(['list','/']);
}
const expected=[];
for(const [op,path,arg] of commands) {
  try {
    let value=null;
    switch(op) {
      case 'write': await fs.writeFile(path,arg);break;
      case 'mkdir': await fs.mkdir(path,{mode:0o755});break;
      case 'rename': await fs.rename(path,arg);break;
      case 'link': await fs.link(path,arg);break;
      case 'symlink': await fs.symlink(path,arg);break;
      case 'unlink': await fs.unlink(path);break;
      case 'rmdir': await fs.rmdir(path);break;
      case 'truncate': await fs.truncate(path,arg);break;
      case 'read': value=Array.from(await fs.readFile(path));break;
      case 'list': value=(await fs.readdir(path)).map(entry=>entry.name);break;
      case 'stat': case 'lstat': {const s=await fs[op](path);value=[s.mode,s.size,s.nlink];break;}
    }
    expected.push({ok:value});
  } catch(error) {expected.push({error:error.code});}
}
const backends=['memory','sqlite','object-store'];
if(process.env.PGLITE_DATABASE_URL) backends.push('pglite');
for(const backend of backends) {
const output=execFileSync('cargo',['run','--quiet','--locked','--example','trace_oracle','--',backend],{
  cwd:fileURLToPath(new URL('..',import.meta.url)),input:commands.map(c=>JSON.stringify(c)).join('\n')+'\n',encoding:'utf8',
}).trim().split('\n').map(line=>JSON.parse(line));
assert.equal(output.length,expected.length);
for(let i=0;i<commands.length;i++) assert.deepEqual(output[i],expected[i],`${backend} seed ${initialSeed} trace step ${i}: ${JSON.stringify(commands[i])}`);
console.log(`Seeded filesystem trace parity (${backend}, seed ${initialSeed}): PASS (${commands.length} operations)`);
}
}
