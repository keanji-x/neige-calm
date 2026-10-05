"""The App's one error system (docs/conventions/agent-commands.md §5): every refusal is a JSON-RPC
error with its code, its message starts with the served tool name, and its `data` carries the
machine fields the message states (`refusal`, plus the current version or key where one applies).
A plain ValueError raised by an argument check is an invalid argument (-32602) at the tool boundary."""

UNKNOWN_TOOL = -32601  # not an invest tool
INVALID = -32602       # missing, malformed or unknown argument, or a bad value
FORBIDDEN = -32403     # role, Track scope or provenance refuses this caller
NOT_FOUND = -32404     # the named entity does not exist
CONFLICT = -32409      # current state refuses the call: re-read and act on it
INTERNAL = -32603
SERVED = 'plugin_invest_'  # the kernel serves tool `<tool>` as `plugin_invest_<tool>`


class Refused(Exception):
    def __init__(self, code, message, refusal, **fields):
        super().__init__(message)
        self.code = code
        self.data = {'refusal': refusal, **fields}

    def named(self, name):
        """The same refusal, its message prefixed with the served tool name."""
        return Refused(self.code, f'{served(name)}: {self}', **self.data)


def served(name):
    return SERVED + (name if isinstance(name, str) else '?')
