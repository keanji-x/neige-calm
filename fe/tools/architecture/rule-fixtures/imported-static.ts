import { IMPORTED_FROZEN_CONST, IMPORTED_LITERAL_CONST } from './imported-static-target.ts';

export const frozenReference = Object.freeze({ a: IMPORTED_FROZEN_CONST });
export const literalReference = Object.freeze({ a: IMPORTED_LITERAL_CONST });
