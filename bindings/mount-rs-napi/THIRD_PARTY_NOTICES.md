# Upstream attribution

## Quinn protocol dependency

Workspace builds use the scoped `quinn-proto 0.11.18` copy in
`vendor/quinn-proto-0.11.18`, with a local first-packet CLOSE draining fix.
The complete upstream package and its MIT and Apache-2.0 licenses are retained.
See that directory's `MOUNT_RS_PATCH.md` and `MOUNT_RS_UPSTREAM.json` for the
verified archive checksum, source commit, original file hashes and patch scope.

mount-rs ports and adapts filesystem behavior and implementation from
[pithings/mountx](https://github.com/pithings/mountx), pinned for differential
testing at commit `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`.

The upstream license and copyright notice are reproduced below. These notices
apply to the upstream-derived portions; they do not replace the repository's
top-level LICENSE.

## mountx

MIT License

Copyright (c) Pooya Parsa <pooya@pi0.io>

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
