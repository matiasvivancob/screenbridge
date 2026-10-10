#!/bin/sh
# Needs Xcode Command Line Tools (xcode-select --install). macOS 12.3+.
cd "$(dirname "$0")" && swiftc -O -import-objc-header CGVirtualDisplay.h main.swift -o screenbridge && echo "built ./screenbridge"
