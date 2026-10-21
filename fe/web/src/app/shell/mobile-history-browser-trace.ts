/** Test-side native event evidence; observes without changing input or lifecycle. */
export function observeHistory(readRoute: () => string) {
  const records: unknown[] = [];
  const start = performance.now();
  const describe = (node: EventTarget | null) => node instanceof Element ? {
    tag: node.tagName, id: node.id, role: node.getAttribute('role'),
    label: node.getAttribute('aria-label'), text: node.textContent?.slice(0, 80),
  } : null;
  const record = (phase: string, event?: Event) => {
    const dialog = document.querySelector<HTMLDialogElement>('dialog#mobile-conversation-history');
    const rect = dialog?.getBoundingClientRect();
    records.push({ t: performance.now() - start, phase, route: readRoute(),
      target: describe(event?.target ?? null),
      control: event?.target instanceof Element ? describe(event.target.closest('button, a, [role=button]')) : null,
      trusted: event?.isTrusted, defaultPrevented: event?.defaultPrevented, focus: describe(document.activeElement),
      input: event instanceof MouseEvent ? { x: event.clientX, y: event.clientY, button: event.button,
        hit: describe(document.elementFromPoint(event.clientX, event.clientY)) } : null,
      pointer: event instanceof PointerEvent ? { id: event.pointerId, type: event.pointerType } : null,
      dialog: dialog === null ? null : { open: dialog.open, modal: dialog.matches(':modal'),
        class: dialog.className, rect: rect === undefined ? null : { left: rect.left, top: rect.top, width: rect.width, height: rect.height } },
    });
  };
  const types = ['pointerdown', 'pointerup', 'click', 'close', 'cancel', 'focusin'] as const;
  const listeners = types.map(type => {
    const listener = (event: Event) => { record(type, event); };
    document.addEventListener(type, listener, true);
    return () => { document.removeEventListener(type, listener, true); };
  });
  const observer = new MutationObserver(changes => {
    for (const change of changes) {
      if (change.target instanceof HTMLDialogElement && change.target.id === 'mobile-conversation-history') {
        record(`dialog:${change.attributeName}`);
      }
    }
  });
  observer.observe(document.body, { attributes: true, subtree: true, attributeFilter: ['open', 'class'] });
  record('start');
  return {
    record,
    failure: () => {
      record('failure');
      // Missing click -> input/hit; click without close -> History close request/native lifecycle;
      // close without route -> MobileHistory deferred action/navigation boundary.
      console.error('HISTORY_SETTINGS_TRACE', JSON.stringify(records));
    },
    dispose: () => { observer.disconnect(); listeners.forEach(dispose => { dispose(); }); },
  };
}
