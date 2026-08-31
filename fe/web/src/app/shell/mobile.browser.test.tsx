import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { render } from '@testing-library/react';
import { useEffect, type ReactNode } from 'react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import type { Cove } from '../../../../core/domain/cove.ts';
import { useState } from '../../ui/state/public.ts';
import { subscribeMobileSecondary } from '../../ui/mobile-page/public.ts';
import { WavePage } from '../../features/wave/page/public.tsx';
import { card, wave } from '../../features/wave/page/test-fixtures.tsx';
import { MobileCoves } from './mobile-coves.tsx';
import { MobilePages } from './mobile-pages.tsx';
import shellStyles from './shell.module.css';

afterEach(() => { document.body.replaceChildren(); });

const settlePaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

function MobileShellFrame({ children }: { children: (backFromReport: () => void) => ReactNode }) {
  const [mobileSection, setMobileSection] = useState<'pages' | 'coves' | null>(null);
  const navigationOpen = mobileSection !== null;
  const [secondaryOpen, setSecondaryOpen] = useState(true);
  useEffect(() => subscribeMobileSecondary(setSecondaryOpen), []);
  const coves: readonly Cove[] = [
    { id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0 },
    { id: 'c2', name: 'Frontend', color: '#8B7FE8', sort: 2, kind: 'user', createdAt: 0, updatedAt: 0 },
  ];
  const productWaves = [
    wave({ id: 'w-mobile', coveId: 'c1', title: 'Responsive mobile UI', lifecycle: 'working', pinnedAt: 30 }),
    wave({ id: 'w-remote', coveId: 'c1', title: 'Remote access', lifecycle: 'draft' }),
  ];
  return (
    <div className={`${shellStyles.shell} ${shellStyles.shellCollapsed} ${secondaryOpen ? shellStyles.shellMobileSecondary : ''}`}>
      <div
        className={`${shellStyles.navigation} ${navigationOpen ? shellStyles.navigationOpen : ''}`}
        role="dialog"
        aria-modal="true"
        aria-label={mobileSection === 'pages' ? 'Pages' : 'Coves'}
        aria-hidden={navigationOpen ? undefined : true}
      >
        <div className={shellStyles.navigationPanel} data-testid="mobile-navigation-panel">
          {mobileSection === 'pages' ? <MobilePages coves={coves} waves={productWaves} onOpenWave={() => {
            setMobileSection(null);
            queueMicrotask(() => setSecondaryOpen(true));
          }} />
            : mobileSection === 'coves' ? <MobileCoves
              coves={coves}
              wavesByCove={new Map([['c1', productWaves], ['c2', []]])}
              onOpenWave={() => {
                setMobileSection(null);
                queueMicrotask(() => setSecondaryOpen(true));
              }}
            /> : null}
        </div>
      </div>
      <main className={shellStyles.main} inert={navigationOpen}>
        <div className={shellStyles.stage}>{children(() => {
          setSecondaryOpen(false);
          setMobileSection('pages');
        })}</div>
      </main>
      <nav className={`${shellStyles.mobileDock} ${secondaryOpen ? shellStyles.mobileDockHidden : ''}`} aria-label="Primary">
        {(['pages', 'today', 'coves', 'me'] as const).map((item) => {
          const icon = item === 'pages' ? 'viewColumns' : item === 'today' ? 'calendar' : item === 'coves' ? 'menu' : 'wrench';
          const contents = <>
            <AstryxIcon icon={icon} size="md" color="inherit" />
            <span>{item[0]?.toUpperCase()}{item.slice(1)}</span>
          </>;
          return (
            <button
              key={item}
              type="button"
              className={shellStyles.mobileDockItem}
              aria-current={mobileSection === item || (item === 'pages' && mobileSection === null) ? 'page' : undefined}
              aria-expanded={item === 'pages' || item === 'coves' ? mobileSection === item : undefined}
              onClick={() => {
                if (item === 'pages' || item === 'coves') setMobileSection(mobileSection === item ? null : item);
              }}
            >
              {contents}
            </button>
          );
        })}
      </nav>
    </div>
  );
}

function ReportPreview() {
  return (
    <article data-testid="report-preview" style={{ display: 'flex', flexDirection: 'column', gap: '20px', paddingBlock: '12px 40px' }}>
      <p style={{ color: 'var(--text-3)', fontSize: '12px', letterSpacing: '0.08em', textTransform: 'uppercase' }}>
        Live report
      </p>
      <h2 style={{ color: 'var(--text)', fontFamily: 'var(--font-serif)', fontSize: '28px', lineHeight: 1.15 }}>
        Mobile workspace direction
      </h2>
      <p style={{ color: 'var(--text-2)', fontFamily: 'var(--font-serif)', fontSize: '18px', lineHeight: 1.65 }}>
        Report stays at the root. Navigation, cards and conversations arrive as focused pages instead of squeezing the document.
      </p>
      <section style={{ display: 'flex', flexDirection: 'column', gap: '8px', padding: '16px', borderRadius: '12px', background: 'var(--surface-code)' }}>
        <strong>Current task</strong>
        <span style={{ color: 'var(--text-2)', lineHeight: 1.5 }}>Validate the right-push interaction on a 390 × 844 viewport.</span>
      </section>
      <h3 style={{ color: 'var(--text)', fontFamily: 'var(--font-serif)', fontSize: '20px' }}>Why this shape</h3>
      <p style={{ color: 'var(--text-2)', fontFamily: 'var(--font-serif)', fontSize: '17px', lineHeight: 1.65 }}>
        The phone gets one clear reading surface. Secondary work remains one gesture away and always has an explicit route back to Report.
      </p>
    </article>
  );
}

describe('Wave mobile presentation', () => {
  it('keeps Report as the root and pushes Cards in as a full-width page', async () => {
    await page.viewport(390, 844);
    render(
      <MobileShellFrame>{(backFromReport) => <WavePage
        wave={wave({ title: 'Responsive mobile UI' })}
        cards={[
          card({ id: 'terminal-1', title: 'Implementation terminal' }),
          card({ id: 'review-1', kind: 'codex', title: 'Design review', deletable: false }),
        ]}
        tasks={[
          {
            blockId: 'task-layout', key: 'mobile-layout', state: 'ready',
            declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null,
          },
          {
            blockId: 'task-touch', key: 'touch-targets', state: 'not-ready',
            declaration: 'Not ready', status: null, statusDetail: null, kind: 'codex', workerCardId: null,
          },
        ]}
        outlineItems={[
          {
            blockId: 'section-shape', label: 'Why this shape', number: 1,
            children: [{ blockId: 'task-layout', label: 'Current task' }],
          },
        ]}
        report={<ReportPreview />}
        conversationList={<button type="button">Mobile UI conversation</button>}
        conversationAction={<button type="button" aria-label="New conversation">Chat</button>}
        onStartConversation={vi.fn()}
        onOpenCard={vi.fn()}
        onOpenTask={vi.fn()}
        mobileBackLabel="Pages"
        onMobileBack={backFromReport}
        onRenameWave={vi.fn()}
        onDeleteWave={vi.fn()}
      />}</MobileShellFrame>,
    );

    const root = document.querySelector('[data-nc-wave-page]')!;
    const panel = document.querySelector<HTMLElement>('[data-nc-mobile-page]')!;
    const opener = page.getByRole('button', { name: 'Wave actions' });

    expect(root.getBoundingClientRect().width).toBeLessThanOrEqual(window.innerWidth);
    expect(getComputedStyle(panel).visibility).toBe('hidden');
    expect((await opener.findElement()).getBoundingClientRect().height).toBeGreaterThanOrEqual(44);
    expect((await page.getByRole('button', { name: 'Back to Pages' }).findElement()).getBoundingClientRect().height)
      .toBeGreaterThanOrEqual(44);
    expect(document.querySelector('nav[aria-label="Primary"]')?.getBoundingClientRect().height).toBe(0);
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-report.png' });

    const mobileHeader = document.querySelector<HTMLElement>('[data-nc-mobile-header]')!;
    const reportArticle = document.querySelector<HTMLElement>('[data-testid="report-preview"]')!;
    reportArticle.style.minHeight = '1200px';
    const headerTop = mobileHeader.getBoundingClientRect().top;
    root.scrollTop = 320;
    root.dispatchEvent(new Event('scroll'));
    await settlePaint();
    expect(Math.abs(mobileHeader.getBoundingClientRect().top - headerTop)).toBeLessThan(1);
    expect(page.getByRole('button', { name: 'New conversation' })).toBeTruthy();
    root.scrollTop = 0;
    reportArticle.style.minHeight = '';

    const navigation = document.querySelector<HTMLElement>('[data-testid="mobile-navigation-panel"]')!;
    await page.getByRole('button', { name: 'Back to Pages' }).click();
    await Promise.all(navigation.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('dialog', { name: 'Pages' })).toBeTruthy();
    expect(page.getByRole('heading', { name: 'Pinned' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-pages.png' });

    await page.getByRole('button', { name: 'Coves', exact: true }).click();
    await Promise.all(navigation.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('dialog', { name: 'Coves' })).toBeTruthy();
    const dockItems = document.querySelectorAll<HTMLElement>('nav[aria-label="Primary"] > *');
    expect(dockItems).toHaveLength(4);
    for (const item of dockItems) {
      expect(getComputedStyle(item).visibility).toBe('visible');
      expect(item.getBoundingClientRect().width).toBeGreaterThan(0);
    }
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-navigation.png' });
    await page.getByRole('button', { name: /Product.*2 waves/ }).click();
    expect(page.getByRole('heading', { name: 'Product' })).toBeTruthy();
    expect(document.querySelector('nav[aria-label="Primary"]')?.getBoundingClientRect().height).toBe(0);
    await Promise.all(document.getAnimations().map((animation) => animation.finished));
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-waves.png' });
    await page.getByRole('button', { name: /Responsive mobile UI.*Working/ }).click();
    expect(document.querySelector('nav[aria-label="Primary"]')?.getBoundingClientRect().height).toBe(0);

    await opener.click();
    expect(page.getByRole('menuitem', { name: 'Outline' })).toBeTruthy();
    expect(page.getByRole('menuitem', { name: 'Cards' })).toBeTruthy();
    expect(page.getByRole('menuitem', { name: 'Tasks' })).toBeTruthy();
    expect(page.getByRole('menuitem', { name: 'Conversations' })).toBeTruthy();
    expect(page.getByRole('menuitem', { name: 'Delete wave' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-wave-menu.png' });
    await page.getByRole('menuitem', { name: 'Outline' }).click();
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('heading', { name: 'Outline' })).toBeTruthy();
    expect(page.getByRole('button', { name: /Current task.*Under Why this shape/ })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-outline.png' });
    await page.getByRole('button', { name: 'Back to Report' }).click();
    await opener.click();
    await page.getByRole('menuitem', { name: 'Cards' }).click();
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    const panelBox = panel.getBoundingClientRect();
    expect(getComputedStyle(panel).visibility).toBe('visible');
    expect(panelBox.left).toBe(0);
    expect(panelBox.width).toBe(window.innerWidth);
    expect(document.querySelector('nav[aria-label="Primary"]')?.getBoundingClientRect().height).toBe(0);
    expect(page.getByRole('heading', { name: 'Cards' })).toBeTruthy();
    const cardLink = page.getByRole('button', { name: /Implementation terminal.*terminal/ });
    expect((await page.getByRole('button', { name: 'Back to Report' }).findElement()).getBoundingClientRect().height)
      .toBeGreaterThanOrEqual(44);
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-cards.png' });
    await cardLink.click();
    expect(page.getByRole('heading', { name: 'Implementation terminal' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-card-detail.png' });
    await page.getByRole('button', { name: 'Back to Cards' }).click();
    await page.getByRole('button', { name: 'Back to Report' }).click();
    await opener.click();
    await page.getByRole('menuitem', { name: 'Tasks' }).click();
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('heading', { name: 'Tasks' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-tasks.png' });
    await page.getByRole('button', { name: 'Back to Report' }).click();
    await opener.click();
    await page.getByRole('menuitem', { name: 'Conversations' }).click();
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('heading', { name: 'Conversations' })).toBeTruthy();
    expect(page.getByRole('button', { name: 'Mobile UI conversation' })).toBeTruthy();
    expect(document.querySelector('[data-nc-mobile-report-chat]')).toBeNull();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-conversations.png' });
  });
});
