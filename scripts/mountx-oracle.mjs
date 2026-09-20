import { pathToFileURL } from "node:url";

const source = process.env.MOUNTX_SOURCE;
if (!source) {
  throw new Error("MOUNTX_SOURCE must point to a checkout of pithings/mountx");
}

const [{ createMemoryDriver }, { createLoopback }] = await Promise.all([
  import(pathToFileURL(`${source}/src/drivers/memory.ts`).href),
  import(pathToFileURL(`${source}/src/harness.ts`).href),
]);

const fs = createLoopback(createMemoryDriver());
await fs.mkdir("/workspace/src", { recursive: true });
await fs.writeFile("/workspace/src/lib.rs", "pub fn answer() -> u8 { 42 }\n");
await fs.writeFile("/workspace/README.md", "mount-rs\n");
await fs.link("/workspace/README.md", "/workspace/README-copy.md");
await fs.rename("/workspace/src", "/workspace/source");
await fs.symlink("source/lib.rs", "/workspace/current");

const entries = async (path) =>
  (await fs.readdir(path, { withFileTypes: true }))
    .sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0))
    .map((entry) => ({
      name: entry.name,
      type: entry.isDirectory() ? "Directory" : entry.isSymbolicLink() ? "Symlink" : "File",
    }));

const stat = await fs.stat("/workspace/README.md");
const output = {
  root: await entries("/workspace"),
  source: await entries("/workspace/source"),
  current: new TextDecoder().decode(await fs.readFile("/workspace/current")),
  readme: new TextDecoder().decode(await fs.readFile("/workspace/README.md")),
  readme_nlink: stat.nlink,
  readme_size: stat.size,
  link_target: await fs.readlink("/workspace/current"),
};

process.stdout.write(`${JSON.stringify(output)}\n`);
