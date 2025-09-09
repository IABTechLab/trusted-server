import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    environment: 'jsdom',
    globals: true,
    // Run tests in the main thread to avoid spawning
    // child processes/workers, which are blocked in this sandbox.
    threads: false,
    // Explicitly use thread pool (no forks) when workers are enabled.
    // Kept for clarity if threads are re-enabled later.
    pool: 'threads',
    setupFiles: [],
    coverage: {
      provider: 'v8',
    },
  },
})
