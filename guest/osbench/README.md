# OS-heavy host-performance workload

`osbench` produces the mix a build tool does inside AmigaOS, the workload
behind issue #796: user-mode computation, exec allocator calls, small-file
filesystem traffic and process launches. It exists to measure the host cost
and the emulated time of that mix, phase by phase, on a fixed machine.

```
osbench <dir> <phases> <scale>
```

`<dir>` is the volume or directory to work in, with its trailing `:` or `/`
(`DH0:`). `<phases>` is any of:

| Letter | Phase | What it exercises |
|---|---|---|
| `F` | format | `Inhibit` + `Format` of `<dir>` as FFS, so every run starts from the same disk |
| `c` | cpu | compiled C in fast RAM: tokenise and hash 32 KiB of text, CRC it, a recursive walk |
| `s` | exec | `AllocMem`/`FreeMem` pairs: Kickstart code, with its `AbsExecBase` reads from chip RAM |
| `f` | files | 100 files in a directory: create and write, scan, read, rename, lock, delete |
| `p` | launch | `SystemTagList` of a command loaded from `<dir>` that creates and deletes one file |

`<scale>` multiplies every phase's iteration count. Each phase prints the
emulated time it took in 1/50 s ticks and a checksum, and the same lines go
to `osbench.log` beside the program. `osbench x <file>` is the child the
launch phase runs. The program names its own directory as `RunProg:`, the
volume `--run` mounts, so it is meant to be started that way.

## Running it

It calls AmigaOS through two register stubs in `start.s` and needs no NDK
headers. `make` uses the dockerized toolchain; the committed binary was built
with `make CC=/opt/amiga/bin/m68k-amigaos-gcc`.

```sh
mkdir run && cp guest/osbench/osbench run/
truncate -s 64M run/bench.hdf            # mkfile -n 64m on macOS
printf 'DOS\001' | dd of=run/bench.hdf conv=notrunc
cd run && copperline --config ../guest/osbench/a3000.example.toml --noaudio \
    --jit --cpu-clock 2500 --run osbench --run-args "DH0: Fcsfp 8" \
    --exit-on-return --screenshot-after 3000 end.png
cat osbench.log
```

Use a fresh hardfile for every run. The process's CPU time is the host
cost; `osbench.log` holds the emulated time, which is what a paced session
waits for. Run the phases one at a time (`Fc`, `Fs`, `Ff`, `Fp`) to
separate them, and subtract a bare `F` run for the boot and format.
For a fixed emulated interval instead of fixed work (the precise core at
25 MHz is too slow to finish a large scale), give a scale that cannot finish
and stop the run with `--screenshot-after`.
