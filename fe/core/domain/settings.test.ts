import { describe, expect, it } from 'vitest';

import {
  FALLBACK_TASK_BUDGET_DEFAULT, TASK_BUDGET_DEFAULT_KEY, taskBudgetDefaultFrom,
} from './settings.js';

describe('taskBudgetDefaultFrom', () => {
  it('decodes the kernel-supplied effective positive integer', () => {
    expect(taskBudgetDefaultFrom({ [TASK_BUDGET_DEFAULT_KEY]: '4' })).toBe(4);
  });

  it('falls back for an older kernel or a malformed value', () => {
    expect(taskBudgetDefaultFrom({})).toBe(FALLBACK_TASK_BUDGET_DEFAULT);
    expect(taskBudgetDefaultFrom({ [TASK_BUDGET_DEFAULT_KEY]: '0' }))
      .toBe(FALLBACK_TASK_BUDGET_DEFAULT);
    expect(taskBudgetDefaultFrom({ [TASK_BUDGET_DEFAULT_KEY]: '1.5' }))
      .toBe(FALLBACK_TASK_BUDGET_DEFAULT);
    expect(taskBudgetDefaultFrom({ [TASK_BUDGET_DEFAULT_KEY]: '1e3' }))
      .toBe(FALLBACK_TASK_BUDGET_DEFAULT);
    expect(taskBudgetDefaultFrom({ [TASK_BUDGET_DEFAULT_KEY]: '0x10' }))
      .toBe(FALLBACK_TASK_BUDGET_DEFAULT);
  });
});
