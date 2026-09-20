import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';
export default defineConfig({
  resolve: { alias: { vitest: fileURLToPath(import.meta.resolve('vitest')) } },
  test: {
    pool: 'forks',
    include: ['*.test.mjs'],
    testTimeout: 15000,
    hookTimeout: 15000,
    maxWorkers: 1,
  },
});
