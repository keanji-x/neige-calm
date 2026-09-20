#!/usr/bin/env python3
"""Neige stdio app entry point; no server or agent process is required."""
from barra.rpc import serve

if __name__ == "__main__":
    serve()
