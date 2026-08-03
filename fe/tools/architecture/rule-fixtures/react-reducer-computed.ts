import React from 'react';
export function useValue() { return React['useReducer']((x: number) => x + 1, 0); }
