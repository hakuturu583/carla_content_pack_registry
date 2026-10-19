import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    silent: 'passed-only',
    projects: [
      { test: { name: 'unit', include: ['test/unit/**/*.test.ts'], environment: 'node'} },
      {
        // Requires `npm run dev:storage` (SeaweedFS) and, for AWS tests, DynamoDB Local
        // (`docker compose -f local/docker-compose.yml --profile aws up -d`).
        test: { name: 'integration', include: ['test/integration/**/*.test.ts'], environment: 'node', testTimeout: 30_000, fileParallelism: false},
      },
    ],
  },
});
