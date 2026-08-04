import { builtinEnvironments, type Environment } from 'vitest/environments';

const jsdomSsrEnvironment: Environment = {
  ...builtinEnvironments.jsdom,
  name: 'jsdom-ssr',
  transformMode: 'ssr',
  viteEnvironment: 'ssr',
};

export default jsdomSsrEnvironment;
