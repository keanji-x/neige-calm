import type { ComponentType } from 'react';
import { ArrowLeft, ArrowRightLeft, ArrowUp, Bell, Bot, ChevronLeft, ChevronRight, CircleCheck, CircleStop, CircleX, Clock3, Ellipsis, File, Folder, ListChecks, LoaderCircle, Maximize, Menu, MessageSquare, Minimize, Paperclip, Pin, Plus, SquareTerminal, Wrench, X, Icon as LucideIcon, type LucideProps } from 'lucide-react';
import { PROVIDER_GLYPHS, type GlyphNode } from './provider-glyphs.ts';
import styles from './icon.module.css';

function SvgGlyph({ nodes, ...props }: LucideProps & { nodes: readonly GlyphNode[] }) {
  return <LucideIcon {...props} iconNode={nodes.map(node => [node.tag, { ...node.attributes }])} />;
}

const glyphs = Object.freeze({
  'chevron-left': (props: LucideProps) => <ChevronLeft {...props} />,
  'chevron-right': (props: LucideProps) => <ChevronRight {...props} />,
  'arrow-left': (props: LucideProps) => <ArrowLeft {...props} />,
  'arrow-up': (props: LucideProps) => <ArrowUp {...props} />,
  'plus': (props: LucideProps) => <Plus {...props} />,
  'close': (props: LucideProps) => <X {...props} />,
  'more': (props: LucideProps) => <Ellipsis {...props} />,
  'chat': (props: LucideProps) => <MessageSquare {...props} />,
  'notification': (props: LucideProps) => <Bell {...props} />,
  'folder': (props: LucideProps) => <Folder {...props} />,
  'file': (props: LucideProps) => <File {...props} />,
  'agent': (props: LucideProps) => <Bot {...props} />,
  'terminal': (props: LucideProps) => <SquareTerminal {...props} />,
  'tools': (props: LucideProps) => <Wrench {...props} />,
  'tasks': (props: LucideProps) => <ListChecks {...props} />,
  'pin': (props: LucideProps) => <Pin {...props} />,
  'status-running': (props: LucideProps) => <LoaderCircle {...props} />,
  'status-waiting': (props: LucideProps) => <Clock3 {...props} />,
  'status-failed': (props: LucideProps) => <CircleX {...props} />,
  'status-done': (props: LucideProps) => <CircleCheck {...props} />,
  'status-exited': (props: LucideProps) => <CircleStop {...props} />,
  'paperclip': (props: LucideProps) => <Paperclip {...props} />,
  'fullscreen': (props: LucideProps) => <Maximize {...props} />,
  'compact': (props: LucideProps) => <Minimize {...props} />,
  'menu': (props: LucideProps) => <Menu {...props} />,
  'switch': (props: LucideProps) => <ArrowRightLeft {...props} />,
  'claude': (props: LucideProps) => <SvgGlyph {...props} nodes={PROVIDER_GLYPHS.claude} />,
  'codex': (props: LucideProps) => <SvgGlyph {...props} nodes={PROVIDER_GLYPHS.codex} />,
} satisfies Readonly<Record<string, ComponentType<LucideProps>>>);

export type IconName = keyof typeof glyphs;

/** Every icon uses the same Lucide SVG renderer, theme size and stroke contract. */
export function Icon({ name, size = 'md' }: { name: IconName; size?: 'sm' | 'md' }) {
  const Glyph = glyphs[name];
  return <Glyph className={styles[size]} aria-hidden="true" focusable="false" />;
}
