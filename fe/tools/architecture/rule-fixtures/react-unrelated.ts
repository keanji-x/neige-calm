const React = { useState: (value: number) => value, createContext: (value: unknown) => value };
export function useValue() { return React.useState(0); }
