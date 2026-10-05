"""Refusals answered as JSON-RPC errors with their agent-command codes, not as tool results."""

FORBIDDEN = -32403  # role, scope or provenance refuses this caller
CONFLICT = -32409   # current state refuses the call: re-read and act on it


class Refused(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
