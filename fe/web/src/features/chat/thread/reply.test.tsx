import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

import { trackWorkspaceRawFileUrl } from '../../../../../core/domain/fs.ts';
import { Reply, type ReplyImageFiles } from './reply.tsx';

afterEach(cleanup);

function imageFiles(root = '/work/track', trackId = 'track-1'): ReplyImageFiles {
  return { root, files: { rawUrl: (path) => trackWorkspaceRawFileUrl(trackId, path) } };
}

it.each(['https://attack.invalid/image.png','//attack.invalid/image.png','screenshots/page.png','/work/track/file.png','file:///work/track/file.png','data:image/png;base64,AAAA','blob:https://attack.invalid/id'])('does not read Markdown image %s', destination=>{
  const rawUrl=vi.fn(()=>'/forbidden');
  render(<Reply imageFiles={{root:'/work/track',files:{rawUrl}}} text={`![Output](${destination})`} />);
  expect(screen.queryByRole('img')).toBeNull();
  expect(screen.getByText('Image unavailable: Output')).toBeTruthy();
  expect(rawUrl).not.toHaveBeenCalled();
});
it('allows only explicit http(s) links and never delegates output links to workspace navigation',()=>{
  const renderLink=vi.fn();
  render(<Reply imageFiles={imageFiles()} renderLink={renderLink} text={'[Local](/file) [Remote](https://example.com)'} />);
  expect(screen.getAllByRole('link')).toHaveLength(1);
  expect(screen.getByRole('link').getAttribute('href')).toBe('https://example.com');
  expect(renderLink).not.toHaveBeenCalled();
});

it('renders inline and display formulas in replies while keeping code literal', () => {
  const { container } = render(<Reply imageFiles={null} text={'Inline $x^2$.\n\n$$\n\\frac{1}{2}\n$$\n\n`$x$`'} />);
  expect(container.querySelectorAll('math')).toHaveLength(2);
  expect(container.querySelector('code')?.textContent).toBe('$x$');
});

it.each(['', '  ', '\t'])('treats a standalone single-line double-dollar formula with suffix %j as display math', (suffix) => {
  const { container } = render(<Reply imageFiles={null} text={`$$x^2$$${suffix}`} />);
  expect(container.querySelector('[data-nc-math-display] math')).not.toBeNull();
});

it('keeps currency text and still renders a later formula after emoji', () => {
  const { container } = render(<Reply imageFiles={null} text={'📈 &pound; Costs $5 or $x$ and $10.'} />);
  expect(container.querySelectorAll('math')).toHaveLength(1);
  expect(container.textContent).toContain('£');
  expect(container.textContent).toContain('Costs $5 or');
  expect(container.textContent).toContain('and $10.');
});

it('preserves currency in shortcut reference labels and their destinations', () => {
  render(<Reply imageFiles={null} text={'[cost $5 and $10]\n\n[cost $5 and $10]: https://example.com/details'} />);
  expect(screen.getByRole('link', { name: 'cost $5 and $10' }).getAttribute('href')).toBe('https://example.com/details');
});

it('keeps raw HTML literal and refuses dangerous links', () => {
  const { container } = render(<Reply imageFiles={null} text={'hello <b>world</b> [unsafe](javascript:alert(1))'} />);
  expect(container.textContent).toContain('<b>world</b>');
  expect(container.querySelector('a')).toBeNull();
});

it('preserves top-level heading anchors including duplicate and empty labels', () => {
  const { container } = render(<Reply imageFiles={null} text={'# Setup\n\n# Setup\n\n# !!!\n\n> # Nested'} />);
  expect([...container.querySelectorAll('h3')].map(heading => heading.getAttribute('id')))
    .toEqual(['setup', 'setup-1', 'section', null]);
});

it('does not turn untrusted TeX into links and keeps invalid expressions readable', () => {
  const { container } = render(<Reply imageFiles={null} text={'$\\href{javascript:alert(1)}{click}$ and $\\unknowncommand{x}$'} />);
  expect(container.querySelector('a')).toBeNull();
  expect(container.textContent).toContain('\\unknowncommand{x}');
});


it.each([
  '[item $5] costs $10\n\n[item $5]: https://example.com/url',
  '[item $5]: https://example.com/url "$10"\n\nSee [item $5]',
])('keeps currency reference links usable after math masking: %s', (text) => {
  render(<Reply imageFiles={null} text={text} />);
  expect(screen.getByRole('link', { name: 'item $5' }).getAttribute('href')).toBe('https://example.com/url');
});


it.each([
  '[item $5] with',
  '[item $5] costs $10 and',
  '[shown][item $5] with',
  '[item $5][] with',
])('renders currency reference links alongside real math: %s', (prefix) => {
  const { container } = render(<Reply imageFiles={null} text={`${prefix} $x$\n\nSee [item $5]\n\n[item $5]: https://example.com/url`} />);
  expect(screen.getAllByRole('link')).toHaveLength(2);
  for (const link of screen.getAllByRole('link')) expect(link.getAttribute('href')).toBe('https://example.com/url');
  expect(container.querySelectorAll('math')).toHaveLength(1);
  expect(container.querySelector('annotation')?.textContent).toBe('x');
  expect(container.textContent).not.toContain('£');
});

it('renders a currency reference image alongside real math', () => {
  const { container } = render(<Reply imageFiles={imageFiles('/')} text={'![item $5] costs $10 and $x$\n\n[item $5]: https://example.com/url'} />);
  expect(screen.queryByRole('img')).toBeNull();
  expect(screen.getByText('Image unavailable: item $5')).toBeTruthy();
  expect(container.querySelectorAll('math')).toHaveLength(1);
  expect(container.querySelector('annotation')?.textContent).toBe('x');
  expect(container.textContent).toContain('costs $10 and');
  expect(container.textContent).not.toContain('£');
});


it.each([
  ['[item \\$5] with $x$\n\n[item \\$5]: https://example.com/url', 'link', 'item $5', true],
  ['![item \\$5] with $x$\n\n[item \\$5]: https://example.com/url', 'image', 'item $5', true],
  ['[shown \\$5][ref]\n\n[ref]: https://example.com/url', 'link', 'shown $5', false],
  ['[shown \\$5][item \\$5] with $x$\n\n[item \\$5]: https://example.com/url', 'link', 'shown $5', true],
  ['[item \\$5][] with $x$\n\n[item \\$5]: https://example.com/url', 'link', 'item $5', true],
  ['[item \\[1\\] $5] with $x$\n\n[item \\[1\\] $5]: https://example.com/url', 'link', 'item [1] $5', true],
] as const)('renders escaped currency references intact: %s', (text, type, label, hasMath) => {
  const { container } = render(<Reply imageFiles={imageFiles('/')} text={text} />);
  if (type === 'link') expect(screen.getByRole('link', { name: label }).getAttribute('href')).toBe('https://example.com/url');
  else expect(screen.getByText(`Image unavailable: ${label}`)).toBeTruthy();
  expect(container.querySelectorAll('math')).toHaveLength(hasMath ? 1 : 0);
  if (hasMath) expect(container.querySelector('annotation')?.textContent).toBe('x');
  expect(container.textContent).not.toContain('£');
});

it('shows unsupported audio, blob, terminal and unknown output without creating media or execution elements',async()=>{
  const {decodeOutput}=await import('../../../../../core/domain/conversation-output.ts');
  const output=decodeOutput({version:1,blocks:[
    {type:'text',text:'Ordered head',fields:{}},
    {type:'audio',mimeType:'audio/wav',data:'AAA=',fields:{}},
    {type:'resource',resource:{uri:'urn:blob',blob:'AAA=',fields:{}},fields:{}},
    {type:'terminal_reference',terminalId:'terminal-1',fields:{}},
    {type:'unknown_extension',raw:{uri:'https://attack.invalid/never-read'}},
  ]},'unsupported');
  const {container}=render(<Reply text="compatibility must not win" output={output} imageFiles={null} />);
  expect(screen.getByText('Ordered head')).toBeTruthy();
  expect(screen.getByText('Unsupported audio · audio/wav')).toBeTruthy();
  expect(screen.getByText('Unsupported binary resource · urn:blob')).toBeTruthy();
  expect(screen.getByText('Unsupported terminal reference · terminal-1')).toBeTruthy();
  expect(screen.getByText('Unsupported output extension')).toBeTruthy();
  expect(container.querySelector('audio, video, img, iframe, a')).toBeNull();
  expect(container.textContent).not.toContain('AAA=');
  expect(container.textContent).not.toContain('compatibility must not win');
});
