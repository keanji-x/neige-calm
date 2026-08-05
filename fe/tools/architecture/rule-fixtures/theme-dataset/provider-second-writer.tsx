declare const resolved: string;
declare function useEffect(effect: () => void, dependencies: unknown[]): void;
export function ThemeProvider() {
  useEffect(() => { document.documentElement.dataset.theme = resolved; }, [resolved]);
  useEffect(() => { document.documentElement.setAttribute('data-theme', resolved); }, [resolved]);
}
