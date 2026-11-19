import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    include: ['tests/smoke/**/*.test.ts'],
    fileParallelism: false,
    testTimeout: 60_000,
    hookTimeout: 240_000,
  },
});
