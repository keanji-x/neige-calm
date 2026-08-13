// @vitest-environment jsdom
import { renderHook } from '@testing-library/react';
import { expect, it } from 'vitest';
import { useTypedConfirm } from './public.tsx';

it('requires an exact confirmation string including surrounding whitespace', () => {
  const { result } = renderHook(() => useTypedConfirm(' exact '));
  result.current.setValue('exact');
  expect(result.current.matches).toBe(false);
});
