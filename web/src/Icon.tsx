// Tiny shared icon set — match the design's `<I n="name" s={size} sw={stroke}/>`.

export type IconName = 'home' | 'arrow' | 'back' | 'send' | 'moon' | 'sun';

interface IconProps {
  n: IconName;
  s?: number;
  sw?: number;
}

export function Icon({ n, s = 16, sw = 1.6 }: IconProps) {
  const common = {
    width: s,
    height: s,
    viewBox: '0 0 24 24',
    fill: 'none',
    stroke: 'currentColor',
    strokeWidth: sw,
    strokeLinecap: 'round' as const,
    strokeLinejoin: 'round' as const,
  };
  switch (n) {
    case 'home':
      return (
        <svg {...common}>
          <path d="M3 12 12 4l9 8M5 11v8a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-8" />
        </svg>
      );
    case 'arrow':
      return (
        <svg {...common}>
          <path d="M5 12h14M13 6l6 6-6 6" />
        </svg>
      );
    case 'back':
      return (
        <svg {...common}>
          <path d="M19 12H5M11 18l-6-6 6-6" />
        </svg>
      );
    case 'send':
      return (
        <svg {...common}>
          <path d="m5 12 6 2 2 6 8-16z" fill="currentColor" stroke="none" />
        </svg>
      );
    case 'moon':
      return (
        <svg {...common}>
          <path d="M21 13A9 9 0 1 1 11 3a7 7 0 0 0 10 10z" />
        </svg>
      );
    case 'sun':
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="4" />
          <path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4 7 17M17 7l1.4-1.4" />
        </svg>
      );
    default:
      return null;
  }
}
