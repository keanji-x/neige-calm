// Tiny shared icon set — match the design's `<I n="name" s={size} sw={stroke}/>`.

export type IconName =
  | 'home'
  | 'arrow'
  | 'back'
  | 'send'
  | 'moon'
  | 'sun'
  | 'gear'
  | 'refresh'
  | 'reset';

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
    case 'gear':
      // Stylized 8-tooth gear + central pivot. Stroke-only so it picks up
      // the surrounding text color (TitleBar `.go.ghost`).
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.87l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.87-.34 1.7 1.7 0 0 0-1.03 1.55V21a2 2 0 0 1-4 0v-.09a1.7 1.7 0 0 0-1.11-1.55 1.7 1.7 0 0 0-1.87.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.7 1.7 0 0 0 .34-1.87 1.7 1.7 0 0 0-1.55-1.03H3a2 2 0 0 1 0-4h.09a1.7 1.7 0 0 0 1.55-1.11 1.7 1.7 0 0 0-.34-1.87l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.7 1.7 0 0 0 1.87.34h0a1.7 1.7 0 0 0 1.03-1.55V3a2 2 0 0 1 4 0v.09a1.7 1.7 0 0 0 1.03 1.55h0a1.7 1.7 0 0 0 1.87-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.7 1.7 0 0 0-.34 1.87v0a1.7 1.7 0 0 0 1.55 1.03H21a2 2 0 0 1 0 4h-.09a1.7 1.7 0 0 0-1.55 1.03z" />
        </svg>
      );
    case 'refresh':
      return (
        <svg {...common}>
          <path d="M20 6v5h-5" />
          <path d="M4 18v-5h5" />
          <path d="M18.5 9A7 7 0 0 0 6.3 6.8L4 9" />
          <path d="M5.5 15a7 7 0 0 0 12.2 2.2L20 15" />
        </svg>
      );
    case 'reset':
      return (
        <svg {...common}>
          <path d="M20 6v5h-5" />
          <path d="M18.5 9A7 7 0 0 0 6.3 6.8L4 9" />
          <path d="m8 16 4 4 4-4" />
          <path d="M12 20V10" />
        </svg>
      );
    default:
      return null;
  }
}
