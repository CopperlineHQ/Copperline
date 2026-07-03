/* Minimal stub for the capstone disassembler API, standing in for the real
 * library the vendored cputest runner uses only to pretty-print failing
 * instructions. cs_disasm reports zero instructions, so the runner falls
 * back to raw opcode words in its mismatch output. */
#pragma once
#include <stddef.h>
#include <stdint.h>

typedef size_t csh;
typedef int cs_err;
typedef int cs_mode;
#define CS_ERR_OK 0
#define CS_ARCH_M68K 0
#define CS_MODE_BIG_ENDIAN 0
#define CS_MODE_M68K_000 0

typedef struct cs_insn {
    char mnemonic[32];
    char op_str[160];
} cs_insn;

static inline cs_err cs_open(int arch, cs_mode mode, csh* handle) {
    (void)arch;
    (void)mode;
    *handle = 1;
    return CS_ERR_OK;
}

static inline size_t cs_disasm(csh handle, const uint8_t* code, size_t code_size, uint64_t address,
                               size_t count, cs_insn** insn) {
    (void)handle;
    (void)code;
    (void)code_size;
    (void)address;
    (void)count;
    *insn = 0;
    return 0;
}

static inline void cs_free(cs_insn* insn, size_t count) {
    (void)insn;
    (void)count;
}
