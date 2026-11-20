#!/usr/bin/env python3
"""Cargo runner for the Unix CI test lanes."""

import os
import resource
import sys


def capped(limit):
    return 256 if limit == resource.RLIM_INFINITY else min(limit, 256)


if os.environ["CARGO_PKG_NAME"] == "lingrep-web":
    limits = resource.getrlimit(resource.RLIMIT_NOFILE)
    resource.setrlimit(resource.RLIMIT_NOFILE, tuple(map(capped, limits)))

os.execv(sys.argv[1], sys.argv[1:])
