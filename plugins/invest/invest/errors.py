"""The App's one error system (docs/conventions/agent-commands.md §5): every refusal is a JSON-RPC
error with its code, and its message starts with the served tool name. A plain ValueError raised by
an argument validator is an invalid argument (-32602) at the tool boundary."""

UNKNOWN_TOOL = -32601  # not an invest tool
INVALID = -32602       # missing, malformed or unknown argument, or a bad value
FORBIDDEN = -32403     # role, Track scope or provenance refuses this caller
NOT_FOUND = -32404     # the named entity does not exist
CONFLICT = -32409      # current state refuses the call: re-read and act on it
INTERNAL = -32603
SERVED = 'plugin_invest_'  # the kernel serves tool `<tool>` as `plugin_invest_<tool>`


class Refused(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def served(name):
    return SERVED + (name if isinstance(name, str) else '?')
