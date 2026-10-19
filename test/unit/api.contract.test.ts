// API contract scenarios, in-process, against the memory and D1 (SQLite) repositories.
import { D1Repository } from '../../src/repository/d1.ts';
import { MemoryRepository } from '../../src/repository/memory.ts';
import { createSqliteD1 } from '../helpers/d1-sqlite.ts';
import { inProcess } from '../helpers/harness.ts';
import { defineApiScenarios } from '../helpers/scenarios.ts';

defineApiScenarios('in-process / MemoryRepository / fake S3', async () => inProcess(new MemoryRepository()).client);
defineApiScenarios('in-process / D1Repository(SQLite) / fake S3', async () => inProcess(new D1Repository(createSqliteD1())).client);
