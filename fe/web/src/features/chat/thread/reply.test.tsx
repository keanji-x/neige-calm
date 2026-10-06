import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

import { trackWorkspaceRawFileUrl } from '../../../../../core/domain/fs.ts';
import { Reply, type ReplyImageFiles } from './reply.tsx';

afterEach(cleanup);

function imageFiles(root = '/work/track', trackId = 'track-1'): ReplyImageFiles {
  return { root, files: { rawUrl: (path) => trackWorkspaceRawFileUrl(trackId, path) } };
}

it.each([
  ['screenshots/page.png', 'screenshots%2Fpage.png'],
  ['./screenshots/page.png', 'screenshots%2Fpage.png'],
  ['/work/track/screenshots/page.png', 'screenshots%2Fpage.png'],
  ['file:///work/track/screenshots/page.png', 'screenshots%2Fpage.png'],
  ['/work/track/screenshots/%E6%88%AA%E5%9B%BE%20one.png', 'screenshots%2F%E6%88%AA%E5%9B%BE%20one.png'],
])('resolves local image destination %s through the Track file port', (destination, path) => {
  render(<Reply text={`![Screenshot](${destination})`} imageFiles={imageFiles()} />);
  expect(screen.getByRole('img', { name: 'Screenshot' }).getAttribute('src'))
    .toBe(`/api/tracks/track-1/workspace/readfile-raw?path=${path}`);
});

it.each([
  '../outside.png', '/work/other/secret.png', '/work/track-other/secret.png',
  'file:///etc/secret.png', '%2e%2e/outside.png', 'ftp://example.com/image.png',
])('does not request a local image outside the workspace: %s', (destination) => {
  const rawUrl = vi.fn(() => '/must-not-be-requested');
  render(<Reply text={`![Screenshot](${destination})`} imageFiles={{ root: '/work/track', files: { rawUrl } }} />);
  expect(rawUrl).not.toHaveBeenCalled();
  expect(screen.queryByRole('img')).toBeNull();
  expect(screen.getByText('Image unavailable: Screenshot')).toBeTruthy();
});

it('waits for the workspace instead of loading a browser-relative image', () => {
  const text = '![Screenshot](screenshots/page.png)';
  const { rerender } = render(<Reply text={text} imageFiles={null} />);
  expect(screen.queryByRole('img')).toBeNull();
  rerender(<Reply text={text} imageFiles={imageFiles()} />);
  expect(screen.getByRole('img', { name: 'Screenshot' }).getAttribute('src'))
    .toBe('/api/tracks/track-1/workspace/readfile-raw?path=screenshots%2Fpage.png');
});

it('keeps network images and leaves code fences and ordinary links untouched', () => {
  const { container } = render(<Reply imageFiles={imageFiles()} text={
    '![Remote](https://example.com/image.png)\n\n[Download](screenshots/page.png)\n\n'
    + '```md\n![Example](screenshots/page.png)\n```'
  } />);
  expect(screen.getByRole('img', { name: 'Remote' }).getAttribute('src')).toBe('https://example.com/image.png');
  expect(screen.getByRole('link', { name: 'Download' }).getAttribute('href')).toBe('screenshots/page.png');
  expect(container.querySelector('pre')?.textContent).toContain('![Example](screenshots/page.png)');
  expect(screen.getAllByRole('img')).toHaveLength(1);
});

it('shows a failed image as unavailable and recovers when the streamed destination changes', () => {
  const files = imageFiles();
  const { rerender } = render(<Reply text="![Screenshot](missing.png)" imageFiles={files} />);
  fireEvent.error(screen.getByRole('img', { name: 'Screenshot' }));
  expect(screen.queryByRole('img')).toBeNull();
  expect(screen.getByText('Image unavailable: Screenshot')).toBeTruthy();
  rerender(<Reply text="![Screenshot](screenshots/page.png)" imageFiles={files} />);
  expect(screen.getByRole('img', { name: 'Screenshot' }).getAttribute('src'))
    .toBe('/api/tracks/track-1/workspace/readfile-raw?path=screenshots%2Fpage.png');
});

it('reuses a stored reply through unrelated parent updates and still accepts changed content', () => {
  const rawUrl = vi.fn(() => '/bound-image.png');
  const files: ReplyImageFiles = { root: '/work/track', files: { rawUrl } };
  const text = '![Screenshot](image.png)';
  const { rerender } = render(<div><Reply text={text} imageFiles={files} /><span>Parent one</span></div>);
  const reads = rawUrl.mock.calls.length;
  expect(reads).toBeGreaterThan(0);
  rerender(<div><Reply text={text} imageFiles={files} /><span>Parent two</span></div>);
  expect(rawUrl).toHaveBeenCalledTimes(reads);
  rerender(<div><Reply text={`${text}\n\nAdded live words.`} imageFiles={files} /><span>Parent three</span></div>);
  expect(screen.getByText('Added live words.')).toBeTruthy();
  expect(rawUrl.mock.calls.length).toBeGreaterThan(reads);
});
