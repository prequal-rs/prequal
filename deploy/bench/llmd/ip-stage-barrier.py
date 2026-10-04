# inference-perf whose stages start together across a run's concurrent generators: each waits at a file barrier on the
# shared /reports mount before every stage. Otherwise data generation (seconds to minutes, by prompt count) and
# per-generator drains desync their stages. Usage: python ip-stage-barrier.py <run tag> <generators> <inference-perf args>
import asyncio
import glob
import socket
import sys

from inference_perf import main_cli
from inference_perf.loadgen.load_generator import LoadGenerator

run_stage = LoadGenerator.run_stage


def aligned(tag, parties):
    async def aligned_run_stage(self, stage_id, *args, **kwargs):
        open(f"/reports/.barrier-{tag}-{stage_id}-{socket.gethostname()}", "w").close()
        while len(glob.glob(f"/reports/.barrier-{tag}-{stage_id}-*")) < parties:
            await asyncio.sleep(0.1)
        return await run_stage(self, stage_id, *args, **kwargs)

    return aligned_run_stage


if __name__ == "__main__":  # load workers may be spawned, re-importing this module
    LoadGenerator.run_stage = aligned(sys.argv[1], int(sys.argv[2]))
    sys.argv = ["inference-perf", *sys.argv[3:]]
    sys.exit(main_cli())
