# Changelog

## [0.14.7](https://github.com/benletchford/m68k-rs/compare/m68k-v0.14.6...m68k-v0.14.7) (2026-10-20)


### Bug Fixes

* **cpu:** correct MC68000 division timing and overflow ([7e20ef1](https://github.com/benletchford/m68k-rs/commit/7e20ef1849bd4073543215e76d56f7a36692dc77))
* **cpu:** enter fault handlers from run_batch with the handler state ([12f3d56](https://github.com/benletchford/m68k-rs/commit/12f3d56a858169ef64f43f6097bc69d7466ec6bb))
* **fpu:** correct extended exponent and rounding behavior ([3be19f7](https://github.com/benletchford/m68k-rs/commit/3be19f7aef9c4f29142b4338d0bbc49027f3a234))
* **fpu:** round FMOVE to single, double and integer formats ([8c8a3b4](https://github.com/benletchford/m68k-rs/commit/8c8a3b4397e4fb98e63d803b7d9745bdfb134ca5))
* **jit:** hand cache-control changes to bus before batch fetch ([0103183](https://github.com/benletchford/m68k-rs/commit/01031839a5a26afe38229196bfc4d8dbd6a17ee2))
* **jit:** resume a callee-return link exit at its return address ([8b759a3](https://github.com/benletchford/m68k-rs/commit/8b759a3d0390346814e977a1373f897872fe89df))
* **mmu:** cache 68030 translations in its ATC ([f2960c8](https://github.com/benletchford/m68k-rs/commit/f2960c886c5523e4762b1e695de523842cf575c0))


### Performance Improvements

* **jit:** restore physical decoded batch-loop performance ([296196e](https://github.com/benletchford/m68k-rs/commit/296196ee18b576cabea6079cca11ab5285b0a9b6))
* **jit:** use fastmem window during PMMU translation ([e89523e](https://github.com/benletchford/m68k-rs/commit/e89523ec2835de566d44cab640ef7bb8c9b92b8d))

## [0.14.6](https://github.com/benletchford/m68k-rs/compare/m68k-v0.14.5...m68k-v0.14.6) (2026-10-15)


### Bug Fixes

* **cpu:** model the FPU-less MC68LC060/MC68EC060 PCR ([245de18](https://github.com/benletchford/m68k-rs/commit/245de18a6602f6e2f0b375a39165179c7ea3ac8b))

## [0.14.5](https://github.com/benletchford/m68k-rs/compare/m68k-v0.14.4...m68k-v0.14.5) (2026-10-12)


### Bug Fixes

* **fpu:** execute FINT and FINTRZ natively on the 68060 ([980f79b](https://github.com/benletchford/m68k-rs/commit/980f79b9c6f89d2c714e5db07ffe3829cc9a35f2))

## [0.14.4](https://github.com/benletchford/m68k-rs/compare/m68k-v0.14.3...m68k-v0.14.4) (2026-10-10)


### Bug Fixes

* **fpu:** use twelve-byte MC68060 NULL state frames ([a661575](https://github.com/benletchford/m68k-rs/commit/a6615752924028d476372ac3eb06812a868aa299))

## [0.14.3](https://github.com/benletchford/m68k-rs/compare/m68k-v0.14.2...m68k-v0.14.3) (2026-09-24)


### Performance Improvements

* **jit:** grow native regions with hot handler arms ([#190](https://github.com/benletchford/m68k-rs/issues/190)) ([385d712](https://github.com/benletchford/m68k-rs/commit/385d7120532182114f74f6612fea0091668a5383))
* **jit:** keep guest registers and flags in native values within a trace ([#191](https://github.com/benletchford/m68k-rs/issues/191)) ([933c1c2](https://github.com/benletchford/m68k-rs/commit/933c1c2bca4d8f789b2405102f62ff2a34d2cb4e))

## [0.14.2](https://github.com/benletchford/m68k-rs/compare/m68k-v0.14.1...m68k-v0.14.2) (2026-09-23)


### Performance Improvements

* **jit:** let tracked-window stores skip the bus when the host proves them plain ([#188](https://github.com/benletchford/m68k-rs/issues/188)) ([31c72a0](https://github.com/benletchford/m68k-rs/commit/31c72a06b6b2924bf1a160c57464ad69690ed7bb))

## [0.14.1](https://github.com/benletchford/m68k-rs/compare/m68k-v0.14.0...m68k-v0.14.1) (2026-09-23)


### Bug Fixes

* **cpu:** reset stale flow state before interpreted instructions ([#183](https://github.com/benletchford/m68k-rs/issues/183)) ([bfa30c5](https://github.com/benletchford/m68k-rs/commit/bfa30c59bd258010497bd4767d64afcb51b08aa8))


### Performance Improvements

* **jit:** add opt-in inlining of native dispatch regions ([#184](https://github.com/benletchford/m68k-rs/issues/184)) ([c764b5f](https://github.com/benletchford/m68k-rs/commit/c764b5f9ea787ae54583042ccf986b2adc71112f))
* **jit:** compile opt-in native regions in the background ([#185](https://github.com/benletchford/m68k-rs/issues/185)) ([be397a7](https://github.com/benletchford/m68k-rs/commit/be397a70fab145ab38235bf1714cf478e4a06e1b))
* **jit:** enable background regions by default on x86-64 ([#186](https://github.com/benletchford/m68k-rs/issues/186)) ([438edd8](https://github.com/benletchford/m68k-rs/commit/438edd8199b44757306d0d68f4e8a28d5c90a345))

## [0.14.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.13.0...m68k-v0.14.0) (2026-09-19)


### Features

* **jit:** support observed writes in native memory traces ([47da649](https://github.com/benletchford/m68k-rs/commit/47da649313c8ffd63ef53ecb63c4a6933fd79ec7))

## [0.13.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.12.1...m68k-v0.13.0) (2026-09-07)


### Features

* expose memory copy provenance to observing buses ([c6d65f4](https://github.com/benletchford/m68k-rs/commit/c6d65f4553c671aea5e488f3ad4e67888ba0b75e))


### Performance Improvements

* keep recorder checks and register reads inline ([f62d228](https://github.com/benletchford/m68k-rs/commit/f62d2284dea2bf607501bf4ab84fecb8941334fd))
* **trace:** bypass admission for compiled exits ([3db98cf](https://github.com/benletchford/m68k-rs/commit/3db98cf35399b950bd914b323c2b9d737e00f89c))
* **trace:** compile absolute-byte immediate compares ([12f309b](https://github.com/benletchford/m68k-rs/commit/12f309b7501996f059821a76126b8d31ec3c737d))
* **trace:** compile displacement indirect calls ([4c06797](https://github.com/benletchford/m68k-rs/commit/4c06797bcf370d23f63632161fcd1f2adb81e616))
* **trace:** compile indexed byte AND-immediate loops ([649468e](https://github.com/benletchford/m68k-rs/commit/649468e0e0473e19087abcb101f66e564368adf4))
* **trace:** compile PC-relative LEA ([fdd6700](https://github.com/benletchford/m68k-rs/commit/fdd6700cc6a8ee6e87cafb98ea1f49dec95109b8))
* **trace:** compile register-indirect jumps ([8e89a40](https://github.com/benletchford/m68k-rs/commit/8e89a40b26a4ca6c15a6f30b7fc6fdc8329cdc4f))
* **trace:** exit at nested forward calls ([c3a5072](https://github.com/benletchford/m68k-rs/commit/c3a5072d8b1e4e11cff2d947ef77a031aebeaccb))
* **trace:** follow guarded indirect jumps ([798a20a](https://github.com/benletchford/m68k-rs/commit/798a20a4b7597a932f1938986625e217640e3c57))
* **trace:** generate the hot 50-byte validator ([5420742](https://github.com/benletchford/m68k-rs/commit/54207424caff9182b2850832a4a6af4c1355d161))
* **trace:** salvage indexed-dispatch prefixes at call spans ([d2b9bc1](https://github.com/benletchford/m68k-rs/commit/d2b9bc1b3b1cbf82ff42a659750c3494445d2553))
* **trace:** specialize code validation by segment width ([c8e57b9](https://github.com/benletchford/m68k-rs/commit/c8e57b9a842ee5e1dc6c242e9775a5384b361344))
* **trace:** specialize common validation segment counts ([ef4f446](https://github.com/benletchford/m68k-rs/commit/ef4f446dd8f919bfcbddb3d708d1cf01903f63b3))
* **trace:** specialize hot validation lengths ([39c929b](https://github.com/benletchford/m68k-rs/commit/39c929b42247b3befd05af52407db758e49aca87))
* **trace:** specialize small watch lists ([1cf40fa](https://github.com/benletchford/m68k-rs/commit/1cf40fa53abcd416f0a440217b583bee317ce0cb))
* **trace:** tag guarded exits in generated code ([3c02c10](https://github.com/benletchford/m68k-rs/commit/3c02c1075fa298e568ed57f5499f3196d5eed457))
* **trace:** validate generated traces once per entry ([d7d57ce](https://github.com/benletchford/m68k-rs/commit/d7d57ce60238698d89b447e962055206628e90a4))

## [0.12.1](https://github.com/benletchford/m68k-rs/compare/m68k-v0.12.0...m68k-v0.12.1) (2026-09-02)


### Performance Improvements

* **decode:** decoded fast paths for immediate register bit ops, LINK/UNLK, and MOVEM.L pre/post-increment ([#162](https://github.com/benletchford/m68k-rs/issues/162)) ([3afaafa](https://github.com/benletchford/m68k-rs/commit/3afaafa248a9b16278838d225bdf3afe47c6f0db))
* **trace:** defer cold independent linear regions ([#163](https://github.com/benletchford/m68k-rs/issues/163)) ([aa8aef1](https://github.com/benletchford/m68k-rs/commit/aa8aef1f64c3258fab4996fc7f3dfe3697cf9786))
* **trace:** scope the guarded stack-frame gate to portable execution ([#161](https://github.com/benletchford/m68k-rs/issues/161)) ([c73c4cd](https://github.com/benletchford/m68k-rs/commit/c73c4cd0bb2fa76f4e789a21a9282830d28665f6))

## [0.12.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.11.6...m68k-v0.12.0) (2026-09-02)


### Features

* real-hardware timing models for the 68030 and 68040 ([#160](https://github.com/benletchford/m68k-rs/issues/160)) ([51aab10](https://github.com/benletchford/m68k-rs/commit/51aab10d84c87c05d88039ec238dee986e244644))

## [0.11.6](https://github.com/benletchford/m68k-rs/compare/m68k-v0.11.5...m68k-v0.11.6) (2026-08-28)


### Bug Fixes

* emit 68040 format-2 address-error frames ([#158](https://github.com/benletchford/m68k-rs/issues/158)) ([d0b53b5](https://github.com/benletchford/m68k-rs/commit/d0b53b5baf56630d1b64d67a42acf3e965fd3780))

## [0.11.5](https://github.com/benletchford/m68k-rs/compare/m68k-v0.11.4...m68k-v0.11.5) (2026-08-28)


### Bug Fixes

* **trace:** keep guarded stack frame paths decoded ([#155](https://github.com/benletchford/m68k-rs/issues/155)) ([c7b2f4c](https://github.com/benletchford/m68k-rs/commit/c7b2f4c61a74f83894bc6e49987f1c15453fe88c))

## [0.11.4](https://github.com/benletchford/m68k-rs/compare/m68k-v0.11.3...m68k-v0.11.4) (2026-08-28)


### Bug Fixes

* **m68k:** align immediate bit trace timing ([#152](https://github.com/benletchford/m68k-rs/issues/152)) ([6bc1272](https://github.com/benletchford/m68k-rs/commit/6bc127282da24e2d6947c5bf1d6a2eb2635194dc))

## [0.11.3](https://github.com/benletchford/m68k-rs/compare/m68k-v0.11.2...m68k-v0.11.3) (2026-08-28)


### Bug Fixes

* preserve indirect TST in portable traces ([#150](https://github.com/benletchford/m68k-rs/issues/150)) ([68b4d00](https://github.com/benletchford/m68k-rs/commit/68b4d0021957a7471f0b62eaeb582ca3c81b8b4e))

## [0.11.2](https://github.com/benletchford/m68k-rs/compare/m68k-v0.11.1...m68k-v0.11.2) (2026-08-26)


### Performance Improvements

* **jit:** admit ADDA memory sources and immediate register bit operations ([5add08b](https://github.com/benletchford/m68k-rs/commit/5add08b0f7e2cedc197821f4dc58ab189ca55abf))
* **jit:** bare RTS/RTD becomes a dynamic-exit trace terminal ([4c29568](https://github.com/benletchford/m68k-rs/commit/4c29568d387f70d38e237973b21b2c18709c8f27))
* **jit:** expand traces with effective-address ops and short-branch if-conversion ([2389334](https://github.com/benletchford/m68k-rs/commit/23893346f70cefd93fc4c189f0cce229020fdf99))
* **jit:** scope the memory-ALU amortization refusal to short regions ([386eee9](https://github.com/benletchford/m68k-rs/commit/386eee97932ffd7425e32b8ba6dd3da89ba7debb))
* **jit:** trace through bytecode dispatchers ([86e698a](https://github.com/benletchford/m68k-rs/commit/86e698a4b355df75038903983ba231597c2ebceb))

## [0.11.1](https://github.com/benletchford/m68k-rs/compare/m68k-v0.11.0...m68k-v0.11.1) (2026-08-26)


### Bug Fixes

* **cpu:** RTD raises change_of_flow so T0 trace sees it ([05fafbf](https://github.com/benletchford/m68k-rs/commit/05fafbfbd79c0ff40cee835b69c8aa9af7b59552))


### Performance Improvements

* **jit:** compile trap-punctuated segments via TrapExit terminals ([1e81706](https://github.com/benletchford/m68k-rs/commit/1e817060336fafff3bd216b4d20f364b18482dfb))

## [0.11.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.14...m68k-v0.11.0) (2026-08-20)


### Features

* **profile:** add per-head chained-call and retirement counters ([dea0b11](https://github.com/benletchford/m68k-rs/commit/dea0b1170c7be3271300fa06e822bf919a051ea6))


### Bug Fixes

* **jit:** use initialized TLS in rejection tests ([416198e](https://github.com/benletchford/m68k-rs/commit/416198ef6f7abb019fa4abde4f0373f66677f0da))


### Performance Improvements

* admit long register-mask MOVEM push/pop into traces ([#111](https://github.com/benletchford/m68k-rs/issues/111)) ([db55299](https://github.com/benletchford/m68k-rs/commit/db55299089cfe45dfbdd91b26ce151b9ea220e66))
* admit the profiled small forms (PEA abs, CMPI.W d16, MOVE #imm,Dn) ([345f8fd](https://github.com/benletchford/m68k-rs/commit/345f8fd622e558ea9e46753c3551dc99dacd0d43))
* **jit:** admit TST.B/W/L (An) into traces ([500bf92](https://github.com/benletchford/m68k-rs/commit/500bf9229ab0c9a13d74260a296cb0c327c80e14))
* **jit:** const-initialize the trace-JIT thread-local ([5b791f6](https://github.com/benletchford/m68k-rs/commit/5b791f6027edfbfd072b9e28df2ce8553d065e55))
* **jit:** make structural trace rejection durable safely ([6202aec](https://github.com/benletchford/m68k-rs/commit/6202aec49b7fa2db69f4a127a0594c268fe81a24))

## [0.10.14](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.13...m68k-v0.10.14) (2026-08-14)


### Bug Fixes

* **m68000/m68010:** issue MOVEM mem-to-reg discarded read after long transfers ([#126](https://github.com/benletchford/m68k-rs/issues/126)) ([9d1fbe0](https://github.com/benletchford/m68k-rs/commit/9d1fbe0984a7f24fbb46ddf62da1034bd6925e24))

## [0.10.13](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.12...m68k-v0.10.13) (2026-08-12)


### Performance Improvements

* admit absolute-addressed TST into the memory-TST family ([#117](https://github.com/benletchford/m68k-rs/issues/117)) ([d5af2c4](https://github.com/benletchford/m68k-rs/commit/d5af2c490533abe8ab611b5ede7616e55dd9cbf3))

## [0.10.12](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.11...m68k-v0.10.12) (2026-08-12)


### Performance Improvements

* record through constant-target JSRs and make call permission durable ([0d8cdc7](https://github.com/benletchford/m68k-rs/commit/0d8cdc7c537d2669faf8024d3838dd3176d3e3e2))

## [0.10.11](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.10...m68k-v0.10.11) (2026-08-12)


### Performance Improvements

* record through BSR calls on a retry, with per-segment SMC intervals ([8cf482f](https://github.com/benletchford/m68k-rs/commit/8cf482f03b03e684f2f666d9ee1cc94094772c8f))

## [0.10.10](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.9...m68k-v0.10.10) (2026-08-11)


### Performance Improvements

* admit AND and OR from memory into the register-ALU family ([14145e6](https://github.com/benletchford/m68k-rs/commit/14145e6184d3a1b2d287e810199337e88a804db1))

## [0.10.9](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.8...m68k-v0.10.9) (2026-08-11)


### Performance Improvements

* admit absolute-addressed CLR into the memory-CLR family ([75baded](https://github.com/benletchford/m68k-rs/commit/75baded6c8c661a965d32fd0ed7a7d750bc1da76))

## [0.10.8](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.7...m68k-v0.10.8) (2026-08-11)


### Performance Improvements

* seed trace candidacy from guard exits and chain compiled continuations ([04f41a0](https://github.com/benletchford/m68k-rs/commit/04f41a0d4957e0420383175c967b9025bcd07320))

## [0.10.7](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.6...m68k-v0.10.7) (2026-08-11)


### Performance Improvements

* salvage a blocked recording's prefix through its last branch ([0c64208](https://github.com/benletchford/m68k-rs/commit/0c64208069ed0fe4b2ccbab7841a76c4253119da))

## [0.10.6](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.5...m68k-v0.10.6) (2026-08-10)


### Performance Improvements

* admit LINK/UNLK frame ops into traces ([865ebd9](https://github.com/benletchford/m68k-rs/commit/865ebd9e5b78e1c06638a6010a9304b17fbf4973))

## [0.10.5](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.4...m68k-v0.10.5) (2026-08-09)


### Performance Improvements

* JIT immediate MOVE stores to memory destinations ([40a8cac](https://github.com/benletchford/m68k-rs/commit/40a8cace840b35f4c32f72aa044849104062db35))

## [0.10.4](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.3...m68k-v0.10.4) (2026-08-09)


### Performance Improvements

* JIT CLR to predecrement destinations ([e0368a0](https://github.com/benletchford/m68k-rs/commit/e0368a0e81a668ba367479d66f459b150cdfc9eb))

## [0.10.3](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.2...m68k-v0.10.3) (2026-08-09)


### Performance Improvements

* jit stores to brief-indexed destinations (MOVE, CLR) ([6c7e8c5](https://github.com/benletchford/m68k-rs/commit/6c7e8c5743a59a7511477c46f1fbecaa0c0e5634))

## [0.10.2](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.1...m68k-v0.10.2) (2026-08-09)


### Performance Improvements

* jit register-to-memory subtracts ([8796494](https://github.com/benletchford/m68k-rs/commit/879649408f1417a5050ff8f122e59939e1f1b1be))

## [0.10.1](https://github.com/benletchford/m68k-rs/compare/m68k-v0.10.0...m68k-v0.10.1) (2026-08-09)


### Performance Improvements

* JIT register-count shifts ([662c6a1](https://github.com/benletchford/m68k-rs/commit/662c6a1df0197aa658d1afb1dbfb3f9df7d67957))

## [0.10.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.9.1...m68k-v0.10.0) (2026-08-09)


### Features

* classify pure poll loops at trace compile time ([5ecdc72](https://github.com/benletchford/m68k-rs/commit/5ecdc72a54431f7d1cf6c98d928c098b51a721fc))

## [0.9.1](https://github.com/benletchford/m68k-rs/compare/m68k-v0.9.0...m68k-v0.9.1) (2026-08-08)


### Performance Improvements

* jit immediate word multiplies ([e1eb2ae](https://github.com/benletchford/m68k-rs/commit/e1eb2aeb3ed3c30312a36ddf7251f814de1259ea))

## [0.9.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.8.3...m68k-v0.9.0) (2026-08-08)


### Features

* **trace-profile:** report silent-rejection opcodes ([ff6455f](https://github.com/benletchford/m68k-rs/commit/ff6455f95f1c14c7a8341034c169d778921545cd))


### Performance Improvements

* **execute:** decode register CLR for boundary hooks ([b1d4891](https://github.com/benletchford/m68k-rs/commit/b1d4891f586a64d56b6ca31fbabe7fc0e84d83a1))

## [0.8.3](https://github.com/benletchford/m68k-rs/compare/m68k-v0.8.2...m68k-v0.8.3) (2026-08-06)


### Performance Improvements

* **execute:** add precise decoded dispatch for boundary-hook runs ([3a2c591](https://github.com/benletchford/m68k-rs/commit/3a2c5918ba4b9eecc112eb2a50ae56c1fedbb920))

## [0.8.2](https://github.com/benletchford/m68k-rs/compare/m68k-v0.8.1...m68k-v0.8.2) (2026-08-06)


### Bug Fixes

* **trace-profile:** attribute recordings that end with no unsupported opcode ([153436b](https://github.com/benletchford/m68k-rs/commit/153436b8bb62b5281345489a904eae135de6265b))


### Performance Improvements

* JIT brief-indexed LEA ([934f5ca](https://github.com/benletchford/m68k-rs/commit/934f5ca60438c09adfb5a90bd11b78be000dc8e5))

## [0.8.1](https://github.com/benletchford/m68k-rs/compare/m68k-v0.8.0...m68k-v0.8.1) (2026-08-06)


### Performance Improvements

* JIT displacement PEA pushes ([6313f2f](https://github.com/benletchford/m68k-rs/commit/6313f2fc49187fff335ef8e47ac89da89f28a9ac))

## [0.8.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.7.2...m68k-v0.8.0) (2026-08-05)


### Features

* report exact trace recording paths ([0edf7df](https://github.com/benletchford/m68k-rs/commit/0edf7df5bb70803671fe468ddbc85fe2f3a5c921))

## [0.7.2](https://github.com/benletchford/m68k-rs/compare/m68k-v0.7.1...m68k-v0.7.2) (2026-08-05)


### Performance Improvements

* JIT indexed immediate word compares ([bfe4a2d](https://github.com/benletchford/m68k-rs/commit/bfe4a2d9839cc3dd18942552829c541cdbd0f09d))

## [0.7.1](https://github.com/benletchford/m68k-rs/compare/m68k-v0.7.0...m68k-v0.7.1) (2026-08-05)


### Performance Improvements

* JIT brief-indexed memory-source ALU operations ([0bc6b89](https://github.com/benletchford/m68k-rs/commit/0bc6b891db8b1f99b0edf806dfa12a8e64cd8e85))
* JIT fixed-point state update loops ([303f5e3](https://github.com/benletchford/m68k-rs/commit/303f5e3309fbde2dd603c5d99b3c18d60de85046))

## [0.7.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.6.0...m68k-v0.7.0) (2026-08-04)


### Features

* add exact-width memory bit-field bus accesses ([d4ad107](https://github.com/benletchford/m68k-rs/commit/d4ad10793271fecc6f0c381d7c89cc3e38449c45))
* expose interrupt-entry cycle boundaries ([04a3bb0](https://github.com/benletchford/m68k-rs/commit/04a3bb07a907d7792eb7d445fce48066c6e5a6cf))


### Performance Improvements

* continue trace recording through full dispatch ([aba25ec](https://github.com/benletchford/m68k-rs/commit/aba25ec5dd0d41705ed0cb6b46e9690a46928ac8))
* JIT packed lookup generator loops ([95c25ac](https://github.com/benletchford/m68k-rs/commit/95c25acdda9d2691769157787c5834822f29d57b))

## [0.6.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.5.3...m68k-v0.6.0) (2026-08-04)


### Features

* add instruction-boundary cycle hooks ([#62](https://github.com/benletchford/m68k-rs/issues/62)) ([af37111](https://github.com/benletchford/m68k-rs/commit/af3711149ad2b4c1cd49511d09a6e98bd48bc758))

## [0.5.3](https://github.com/benletchford/m68k-rs/compare/m68k-v0.5.2...m68k-v0.5.3) (2026-08-04)


### Performance Improvements

* JIT guarded indexed scans ([72f69e2](https://github.com/benletchford/m68k-rs/commit/72f69e2857346c5536f394d4b447c0ac8775fac3))

## [0.5.2](https://github.com/benletchford/m68k-rs/compare/m68k-v0.5.1...m68k-v0.5.2) (2026-08-02)


### Bug Fixes

* honor bus boundaries after interrupt entry ([1e7124b](https://github.com/benletchford/m68k-rs/commit/1e7124ba3c9dfa7b6ad272e4d8dd03664f77f1ad))

## [0.5.1](https://github.com/benletchford/m68k-rs/compare/m68k-v0.5.0...m68k-v0.5.1) (2026-08-02)


### Bug Fixes

* remove 020 result forwarding and make the DBcc refill alignment dependent ([ebe891f](https://github.com/benletchford/m68k-rs/commit/ebe891f0e808c693f18ed97b75edb9105f7a32f2))

## [0.5.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.4.0...m68k-v0.5.0) (2026-08-02)


### Features

* calibrate 020 taken-branch refill and model result forwarding from real hardware ([0b9117a](https://github.com/benletchford/m68k-rs/commit/0b9117a83217a61fc3096bbf4860d59bd0fdf8d5))

## [0.4.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.3.2...m68k-v0.4.0) (2026-08-01)


### Features

* add bus-requested cycle boundaries ([#48](https://github.com/benletchford/m68k-rs/issues/48)) ([70ead97](https://github.com/benletchford/m68k-rs/commit/70ead97e05c22fcfe5101c7567e9fe1ebb88b412))

## [0.3.2](https://github.com/benletchford/m68k-rs/compare/m68k-v0.3.1...m68k-v0.3.2) (2026-07-31)


### Bug Fixes

* correct public API documentation ([e0d0b57](https://github.com/benletchford/m68k-rs/commit/e0d0b57fe5f0007a87d0e4027f29039bd5105cc5))

## [0.3.1](https://github.com/benletchford/m68k-rs/compare/m68k-v0.3.0...m68k-v0.3.1) (2026-07-31)


### Bug Fixes

* make native JIT dependencies opt in ([39e767a](https://github.com/benletchford/m68k-rs/commit/39e767a00b6365880a71e44f0805c97aae7f9a02))

## [0.3.0](https://github.com/benletchford/m68k-rs/compare/m68k-v0.2.5...m68k-v0.3.0) (2026-07-31)


### Features

* unify cycle-exact and high-performance emulation ([8d11edf](https://github.com/benletchford/m68k-rs/commit/8d11edf75feb62dd3211bfda7a3cb9a781f6b3e6))

## [0.2.5](https://github.com/benletchford/m68k-rs/compare/m68k-v0.2.4...m68k-v0.2.5) (2026-07-25)


### Performance Improvements

* batch profitable self-loops in native JIT ([36922d8](https://github.com/benletchford/m68k-rs/commit/36922d8a0f892353be9572c3f7c12173c68a854a))
* JIT brief-indexed MOVE and postincrement ADD loops ([db116c2](https://github.com/benletchford/m68k-rs/commit/db116c2fbba94c06a6bdf0ab98742d63e914bef0))
* JIT data-only MOVEM.W postincrement loops ([00f635f](https://github.com/benletchford/m68k-rs/commit/00f635f2c93cafec0567956423bc7bc0a90548f1))
* JIT immediate ASR and LSL shifts ([0897f04](https://github.com/benletchford/m68k-rs/commit/0897f045c38a77ddb30bcbd1309bc773fa499c84))
* JIT memory-source ADD and SUB traces ([99c0f19](https://github.com/benletchford/m68k-rs/commit/99c0f19383506b994db67d779214bfcac16b226e))
* JIT profitable indirect JSR boundaries ([abad647](https://github.com/benletchford/m68k-rs/commit/abad647e23814c07f22824ec556797dd97b621a2))

## [0.2.4](https://github.com/benletchford/m68k-rs/compare/m68k-v0.2.3...m68k-v0.2.4) (2026-07-24)


### Bug Fixes

* count successful calls in trace adaptation ([a2dcfbd](https://github.com/benletchford/m68k-rs/commit/a2dcfbd5c107a0bf09ffbf6c24d289d19e624f88))


### Performance Improvements

* adapt JIT traces to dominant branch paths ([4b95efb](https://github.com/benletchford/m68k-rs/commit/4b95efb1317ba2cccc763426ac64690a67d1489a))

## [0.2.3](https://github.com/benletchford/m68k-rs/compare/m68k-v0.2.2...m68k-v0.2.3) (2026-07-24)


### Performance Improvements

* JIT hot memory-source CMP loops ([3571e27](https://github.com/benletchford/m68k-rs/commit/3571e2754399bc5ef229f97687fac585a911df16))
* JIT hot memory-source CMP loops ([a497005](https://github.com/benletchford/m68k-rs/commit/a49700513c4c2e789c103eb8642e6368e2c83a93))
* JIT two-instruction self-loops ([6433e09](https://github.com/benletchford/m68k-rs/commit/6433e094a82fc8bf7db23c214e60ab152200105d))
* JIT two-instruction self-loops ([cdc342a](https://github.com/benletchford/m68k-rs/commit/cdc342a3f82eab52308a31407678c508af6477db))

## [0.2.2](https://github.com/benletchford/m68k-rs/compare/m68k-v0.2.1...m68k-v0.2.2) (2026-07-23)


### Performance Improvements

* extend traces for Classic Mac memory loops ([a714fc0](https://github.com/benletchford/m68k-rs/commit/a714fc02f71d5747cf84b974e334476be643deb4))
* extend traces for Classic Mac memory loops ([b30fe3b](https://github.com/benletchford/m68k-rs/commit/b30fe3b4b95ff1281cc4ebd67634303b8f01b92a))
* fast-path classic Mac displacement memory ops ([6e8e018](https://github.com/benletchford/m68k-rs/commit/6e8e018e7cfd485a04566d1927bb289cf73a01ae))
* fast-path classic Mac displacement memory ops ([287cd02](https://github.com/benletchford/m68k-rs/commit/287cd024b05ab0a50ae96e7c42fec11b48b4e8df))
* inline generic memory helpers ([146c350](https://github.com/benletchford/m68k-rs/commit/146c350b62c7a7d33d9332625a6bb837f55585d3))
* inline generic memory helpers ([128a6a2](https://github.com/benletchford/m68k-rs/commit/128a6a218a9880fc6f3ed03ad860ea5804063505))
* inline memory operation internals ([e782cff](https://github.com/benletchford/m68k-rs/commit/e782cffeb365b3ca9bb5a7a806429a36735bbac6))
* inline memory operation internals ([a90d60a](https://github.com/benletchford/m68k-rs/commit/a90d60a82998b7a17e434356234301b26418f325))
* inline the decoded simple-op executor ([9a808c5](https://github.com/benletchford/m68k-rs/commit/9a808c5cdac218ea1008cd0017dc226ddc28fccc))
* inline the decoded simple-op executor ([af33e5b](https://github.com/benletchford/m68k-rs/commit/af33e5b18e268f69f3449e4a2415014b0564bf3c))
* record multi-block JIT regions ([6dfea1e](https://github.com/benletchford/m68k-rs/commit/6dfea1e86385d1e1e0cc594f688809bb9139b7a6))
* record multi-block JIT regions ([7fa51ad](https://github.com/benletchford/m68k-rs/commit/7fa51adfd44f69d5118a751d2656b379b1b268a1))

## [0.2.1](https://github.com/benletchford/m68k-rs/compare/m68k-v0.2.0...m68k-v0.2.1) (2026-07-16)


### Bug Fixes

* release-please ([609e8d3](https://github.com/benletchford/m68k-rs/commit/609e8d385b7dc5a69e7a244ff225029da511b491))
* unused mut ([7f28225](https://github.com/benletchford/m68k-rs/commit/7f282250ed0379d2b67a20229fa6867d92c16508))
