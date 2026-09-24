"""Polling and publication continue without an active agent conversation."""
import threading

from .report import tables


class Runtime:
    def __init__(self, portfolio, publish):
        self.portfolio = portfolio
        self.publish = publish
        self.wake = threading.Event()
        self.closed = threading.Event()
        self.thread = None

    def run(self):
        while not self.closed.is_set():
            self.wake.clear()
            try:
                for track, state in self.portfolio.process_once():
                    for kind, payload in tables(state).items():
                        if self.closed.is_set():
                            return
                        self.publish(track, kind, payload)
            except Exception:
                # Publication has no broker side effect; retry projections next tick.
                pass
            self.wake.wait(self.portfolio.account.poll_seconds)

    def launch(self):
        self.thread = threading.Thread(target=self.run, name="paper-reconcile", daemon=True)
        self.thread.start()

    def close(self):
        self.closed.set()
        self.wake.set()
