"""Largest single CUDA allocation WSL grants, then how much in 256 MiB chunks, optionally with host RAM held first.
Usage: gpu-alloc-probe.py [host-GiB-to-hold]"""
import sys

import torch

held = bytearray(int(float(sys.argv[1]) * 2**30)) if len(sys.argv) > 1 else None
if held:
    held[::4096] = b"\1" * len(held[::4096])
    print(f"holding {len(held) / 2**30:.1f} GiB of host RAM")

free, total = torch.cuda.mem_get_info()
print(f"free {free / 2**30:.2f} GiB of {total / 2**30:.2f}")
for gib in (0.5, 1, 2, 3, 4, 6, 8):
    try:
        block = torch.empty(int(gib * 2**30), dtype=torch.uint8, device="cuda")
        print(f"single {gib} GiB: ok")
        del block
        torch.cuda.empty_cache()
    except torch.OutOfMemoryError:
        print(f"single {gib} GiB: OOM")
        break
chunks = []
try:
    while len(chunks) < 44:
        chunks.append(torch.empty(2**28, dtype=torch.uint8, device="cuda"))
except torch.OutOfMemoryError:
    pass
print(f"chunked: {len(chunks) * 0.25:.2f} GiB in 256 MiB blocks")
