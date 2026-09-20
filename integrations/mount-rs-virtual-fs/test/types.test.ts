import type { Bash, IFileSystem } from "just-bash";
import type {
  FileEntry,
  Workspace,
  WorkspaceFilesystem,
} from "@mastra/core/workspace";
import type { Filesystem } from "@mount-rs/core";
import {
  createBash,
  createJustBashFilesystem,
  createMastraFilesystem,
} from "@mount-rs/virtual-fs";
import {
  createJustBashFilesystem as createJustBashSubpath,
} from "@mount-rs/virtual-fs/just-bash";
import {
  createMastraFilesystem as createMastraSubpath,
} from "@mount-rs/virtual-fs/mastra";

declare const nodeRustFs: Filesystem;
declare const workspace: Workspace;

const justBash = await createJustBashFilesystem(nodeRustFs);
const justBashSubpath = await createJustBashSubpath(nodeRustFs, {
  readOnly: true,
  refreshPaths: false,
});
const bash: Bash = await createBash(nodeRustFs, { cwd: "/" });
const justInterface: IFileSystem = justBash;

const mastra = createMastraFilesystem(nodeRustFs, { id: "drive" });
const mastraSubpath = createMastraSubpath(nodeRustFs);
const workspaceFilesystem: WorkspaceFilesystem = mastra;
const entry: FileEntry = {
  name: "file.txt",
  type: "file",
};

void workspace;
void bash;
void justInterface;
void justBashSubpath;
void mastraSubpath;
void workspaceFilesystem;
void entry;

// @ts-expect-error adapters require the public filesystem backend contract
await createJustBashFilesystem({});
// @ts-expect-error adapters require the public filesystem backend contract
createMastraFilesystem(undefined);
