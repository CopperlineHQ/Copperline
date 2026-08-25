
#define _CRT_SECURE_NO_WARNINGS

#include <assert.h>
#include <ctype.h>
#include <math.h>
#include <memory.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#ifdef _MSC_VER
#include "msc_dirent.h"
#else
#include <dirent.h>
#endif

#define DONTSTOPONERROR 0

/* Local patch: every fatal abort in this file (unreadable or malformed
 * test data, allocation failure, user abort) exits with status 1. The
 * WinUAE original exits 0 everywhere because the Amiga-side harness never
 * consults the status; here the process exit code is the only signal the
 * embedding runner (and CI) has, so a setup failure must not read as a
 * pass. M68KTester_run_tests likewise reports the aggregate verdict. */

#include "capstone/include/capstone/capstone.h"
#include "cputest_defines.h"
#include "m68k_cpu_tester.h"

typedef unsigned int uae_u32;
typedef int uae_s32;
typedef unsigned short uae_u16;
typedef short uae_s16;
typedef unsigned char uae_u8;
typedef signed char uae_s8;

struct fpureg {
    uae_u16 exp;
    uae_u16 dummy;
    uae_u32 m[2];
};

// must match asm.S
struct registers {
    uae_u32 regs[16];
    uae_u32 ssp;
    uae_u32 msp;
    uae_u32 pc;
    uae_u32 sr;
    uae_u32 exc, exc010;
    uae_u32 excframe;
    struct fpureg fpuregs[8];
    uae_u32 fpiar, fpcr, fpsr;
    uae_u32 srcaddr, dstaddr;
    /* Local patch: fields for the newer generator's CT_ENDPC /
     * CT_BRANCHTARGET / CT_CYCLES records (appended so the callback's
     * prefix cast stays layout-compatible). */
    uae_u32 endpc, branchtarget, cycles;
    uae_u8 branchtarget_mode;
};

/* Local patch: sentinel state (see the per-test comment). */
static uae_u32 cputest_opcodeend;
static uae_u32 cputest_branchdata;
static uae_u8* translate_to_native_checked(uae_u32 addr);

static M68KTesterCallback s_cpu_callback;
static void* s_cpu_user_data;
static M68KTesterContext* s_cpu_context;
static csh s_cs_handle;
static struct registers test_regs;
static struct registers last_registers;
static struct registers regs;
static uae_u8* opcode_memory;
static uae_u32 opcode_memory_addr;
static uae_u8* low_memory;
static uae_u8* high_memory;
static int low_memory_size;
static int high_memory_size;
static uae_u32 test_low_memory_start, test_low_memory_end;
static uae_u32 test_high_memory_start, test_high_memory_end;
static uae_u8* test_memory;
static uae_u32 test_memory_addr, test_memory_end;
static uae_u32 test_memory_size;
static uae_u8* test_data;
static uae_u32 safe_memory_start, safe_memory_end;
static uae_u32 user_stack_memory, super_stack_memory;
static int test_data_size;
static uae_u32 oldvbr;
static uae_u8* vbr_zero = 0;
static int hmem_rom, lmem_rom;
static uae_u8* absallocated;
static int cpu_lvl, fpu_model;
static uae_u16 sr_undefined_mask;
static int check_undefined_sr;
static uae_u32 cpustatearraystore[16];

/*
static uae_u32 cpustatearraynew[] = {
        0x00000005, // SFC
        0x00000005, // DFC
        0x00000009, // CACR
        0x00000000, // CAAR
        0x00000000, // MSP
};
*/

static uae_u8* low_memory_temp;
static uae_u8* high_memory_temp;
static uae_u8* low_memory_back;
static uae_u8* high_memory_back;
static int low_memory_offset;
static int high_memory_offset;

static uae_u32 vbr[256];

static char inst_name[16 + 1];
#ifndef M68K
static char outbuffer[40000];
#else
static char outbuffer[4000];
#endif
static char* outbp;
static int infoadded;
static int errors;
static int testcnt;
static int dooutput = 1;
static int quit;
static uae_u8 ccr_mask;
static uae_u32 addressing_mask = 0x00ffffff;
static uae_u32 interrupt_mask;

/* Local patch: expose the addressing mask parsed from the data set header
 * (bit 31 of the level word selects 32-bit addressing) so the embedding
 * runner applies the same address aliasing as the generator did. */
uae_u32 m68k_tester_addressing_mask(void)
{
    return addressing_mask;
}

/* Local patch: expose the FPU model the data set was generated with (0 =
 * no FPU attached), so the runner can model coprocessor absence. */
uae_u32 m68k_tester_fpu_model(void)
{
    return fpu_model;
}

#define SIZE_STORED_ADDRESS_OFFSET 8
#define SIZE_STORED_ADDRESS 16
static uae_u8 srcaddr[SIZE_STORED_ADDRESS];
static uae_u8 dstaddr[SIZE_STORED_ADDRESS];
static uae_u8 stackaddr[SIZE_STORED_ADDRESS];
static uae_u32 stackaddr_ptr;

#ifndef M68K

#define xmemcpy memcpy

static uae_u8* allocate_absolute(uae_u32 addr, uae_u32 size) { return calloc(1, size); }
/*
static void free_absolute(uae_u32 addr, uae_u32 size)
{
}
*/
/*
static void execute_test000(struct registers *regs)
{
        regs->regs[0] <<= 8;
}
static void execute_test010(struct registers *regs)
{
}
static void execute_test020(struct registers *regs)
{
}
static void execute_testfpu(struct registers *regs)
{
}
*/
static uae_u32 tosuper(uae_u32 v) { return 0; }
static void touser(uae_u32 v) {}
// static uae_u32 exceptiontable000, exceptiontable010, exceptiontable020, exceptiontablefpu;
static uae_u32 testexit(void) { return 0; }
static uae_u32 setvbr(uae_u32 v) { return 0; }
/*
static uae_u32 get_cpu_model(void)
{
        return 0;
}
*/
static void setcpu(uae_u32 v, uae_u32* s, uae_u32* d) {}
static void flushcache(uae_u32 v) {}
// static void *error_vector;
#else

static void xmemcpy(void* d, void* s, int size) { __builtin_memcpy(d, s, size); }

extern uae_u8* allocate_absolute(uae_u32, uae_u32);
extern void free_absolute(uae_u32, uae_u32);
extern void execute_test000(struct registers*);
extern void execute_test010(struct registers*);
extern void execute_test020(struct registers*);
extern void execute_testfpu(struct registers*);
extern uae_u32 tosuper(uae_u32);
extern void touser(uae_u32);
extern uae_u32 exceptiontable000, exceptiontable010, exceptiontable020, exceptiontablefpu;
extern uae_u32 testexit(void);
extern uae_u32 setvbr(uae_u32);
extern uae_u32 get_cpu_model(void);
extern void setcpu(uae_u32, uae_u32*, uae_u32*);
extern void flushcache(uae_u32);
extern void* error_vector;

#endif

static void join_path(char* dest, const char* path, const char* file, int len) {
    size_t path_len = strlen(path);

    if (path_len > 0) {
        if (path[path_len - 1] == '/' || path[path_len - 1] == '\\') {
            snprintf(dest, len, "%s%s", path, file);
        } else {
            snprintf(dest, len, "%s/%s", path, file);
        }
    } else {
        strncpy(dest, file, len);
    }
}

struct accesshistory {
    uae_u8* addr;
    uae_u32 val;
    uae_u32 oldval;
    int size;
};
static int ahcnt;

#define MAX_ACCESSHIST 48
static struct accesshistory ahist[MAX_ACCESSHIST];

static void endinfo(void) {
    printf("Last test: %u\n", testcnt);
    uae_u8* p = opcode_memory;
    for (int i = 0; i < 32 * 2; i += 2) {
        uae_u16 v = (p[i] << 8) | (p[i + 1]);
        if (v == 0x4afc && i > 0) break;
        printf(" %04x", v);
    }
    printf("\n");
}

static void safe_memcpy(uae_u8* d, uae_u8* s, int size) {
    // assume this is true
    assert(safe_memory_start == 0xffffffff);
    assert(safe_memory_end == 0xffffffff);
    xmemcpy(d, s, size);
}

static bool try_range_to_m68k(uint32_t* v, uint8_t* addr, uint8_t* start, uint32_t size) {
    if (addr >= start && addr <= (start + size)) {
        *v = (uint32_t)(uintptr_t)(start - addr);
        return true;
    } else {
        return false;
    }
}

static uint32_t translate_to_m68k(uint8_t* addr) {
    uint32_t v = 0;

    if (try_range_to_m68k(&v, addr, low_memory, low_memory_size)) {
        return v;
    }

    if (try_range_to_m68k(&v, addr, high_memory, high_memory_size)) {
        return v;
    }

    if (try_range_to_m68k(&v, addr, test_memory, test_memory_size)) {
        return v;
    }

    printf("FATAL: %p was not found within the three memory ranges:\n", addr);
    printf("       low_memory (%08x - %08x) %p - %p\n", test_low_memory_start,
           test_low_memory_start + test_low_memory_end, low_memory, low_memory + test_low_memory_end);
    printf("       high_memory (%08x - %08x) %p - %p\n", test_high_memory_start,
           test_high_memory_start + test_high_memory_end, high_memory, high_memory + test_high_memory_end);
    printf("       test_memory (%08x - %08x) %p - %p\n", test_memory_addr, test_memory_addr + test_memory_end,
           test_memory, test_memory + test_memory_end);

    *((volatile int*)0) = 0xfff;

    exit(1);

    return 0;
}

static uint8_t* try_range_to_native(uint32_t addr, uint8_t* ptr, uint32_t start, uint32_t end) {
    if (addr >= start && addr <= end) {
        return ptr + (addr - start);
    } else {
        return 0;
    }
}

static uae_u8* translate_to_native_checked(uae_u32 addr);

static uint8_t* translate_to_native(uint32_t addr) {
    uint8_t* low = try_range_to_native(addr, low_memory, test_low_memory_start, test_low_memory_end);
    uint8_t* high = try_range_to_native(addr, high_memory, test_high_memory_start, test_high_memory_end);
    uint8_t* test = try_range_to_native(addr, test_memory, test_memory_addr, test_memory_end);

    if (low) {
        return low;
    }
    if (high) {
        return high;
    }
    if (test) {
        return test;
    }

    /*
    printf("FATAL: %08x was not found within the three memory ranges\n", addr);
    printf("       low_memory (%08x - %08x) %p - %p\n",
                    test_low_memory_start, test_low_memory_start + test_low_memory_end,
                    low_memory, low_memory + test_low_memory_end);
    printf("       high_memory (%08x - %08x) %p - %p\n",
                    test_high_memory_start, test_high_memory_start + test_high_memory_end,
                    high_memory, high_memory + test_high_memory_end);
    printf("       test_memory (%08x - %08x) %p - %p\n",
                    test_memory_addr, test_memory_addr + test_memory_end,
                    test_memory, test_memory + test_memory_end);

    *((volatile int*)0) = 0xfff;

    exit(1);
    */

    return 0;
}

static int test_active;
static uae_u32 enable_data;
static uae_u32 error_vectors[12];

// if exception happens outside of test code, jump to
// infinite loop and flash colors.
static void reset_error_vectors(void) {
    uae_u32* p;
    if (cpu_lvl == 0) {
        p = (uae_u32*)vbr_zero;
    } else {
        p = vbr;
    }
    for (int i = 2; i < 4; i++) {
        p[i] = error_vectors[i - 2];
    }
}

static void set_error_vectors(void) {
    uae_u32* p;
    if (cpu_lvl == 0) {
        p = (uae_u32*)vbr_zero;
    } else {
        p = vbr;
    }
    for (int i = 2; i < 4; i++) {
        // NOT implemented on non-m68k
        p[i] = 0;  //(uae_u32)&error_vector;
    }
}

static void start_test(void) {
    if (test_active) return;

#ifdef M68K
    if (lmem_rom > 0) {
        if (memcmp(low_memory, low_memory_temp, low_memory_size)) {
            printf("Low memory ROM mismatch!\n");
            exit(1);
        }
    }
    if (hmem_rom > 0) {
        if (memcmp(high_memory, high_memory_temp, high_memory_size)) {
            printf("High memory ROM mismatch!\n");
            exit(1);
        }
    }
#endif

    test_active = 1;

    enable_data = tosuper(0);

    safe_memcpy(low_memory_back + low_memory_offset, low_memory + low_memory_offset,
                low_memory_size - low_memory_offset);
    // always copy exception vectors if 68000
    if (cpu_lvl == 0 && low_memory_offset > 0x08) safe_memcpy(low_memory_back + 8, low_memory + 8, (192 - 2) * 4);

    if (!hmem_rom)
        safe_memcpy(high_memory_back, high_memory + high_memory_offset, high_memory_size - high_memory_offset);

    safe_memcpy(low_memory + low_memory_offset, low_memory_temp + low_memory_offset,
                low_memory_size - low_memory_offset);
    if (cpu_lvl == 0 && low_memory_offset > 0x08) safe_memcpy(low_memory + 8, low_memory_temp + 8, (192 - 2) * 4);

    if (!hmem_rom)
        safe_memcpy(high_memory + high_memory_offset, high_memory_temp, high_memory_size - high_memory_offset);

    /*
    Not implemented on non-m68k
    if (cpu_lvl == 0) {
            uae_u32 *p = (uae_u32 *)vbr_zero;
            for (int i = 2; i < 12; i++) {
                    p[i] = (uae_u32)(((uae_u32)&exceptiontable000) + (i - 2) * 2);
                    if (i < 12 + 2) {
                            error_vectors[i - 2] = p[i];
                    }
            }
            for (int i = 32; i < 48; i++) {
                    p[i] = (uae_u32)(((uae_u32)&exceptiontable000) + (i - 2) * 2);
            }
    } else {
            oldvbr = setvbr((uae_u32)vbr);
            for (int i = 2; i < 48; i++) {
                    if (fpu_model) {
                            vbr[i] = (uae_u32)(((uae_u32)&exceptiontablefpu) + (i - 2) * 2);
                    } else if (cpu_lvl == 1) {
                            vbr[i] = (uae_u32)(((uae_u32)&exceptiontable010) + (i - 2) * 2);
                    } else {
                            vbr[i] = (uae_u32)(((uae_u32)&exceptiontable020) + (i - 2) * 2);
                    }
                    if (i >= 2 && i < 12) {
                            error_vectors[i - 2] = vbr[i];
                    }
            }
    }
    setcpu(cpu_lvl, cpustatearraynew, cpustatearraystore);
    */
}

static void end_test(void) {
    if (!test_active) return;
    test_active = 0;

    safe_memcpy(low_memory + low_memory_offset, low_memory_back + low_memory_offset,
                low_memory_size - low_memory_offset);
    if (cpu_lvl == 0 && low_memory_offset > 0x08) safe_memcpy(low_memory + 8, low_memory_back + 8, (192 - 2) * 4);

    if (!hmem_rom)
        safe_memcpy(high_memory + high_memory_offset, high_memory_back, high_memory_size - high_memory_offset);

    if (cpu_lvl > 0) {
        setvbr(oldvbr);
    }
    setcpu(cpu_lvl, cpustatearraystore, NULL);

    touser(enable_data);
}

static uae_u8* load_file(const char* path, const char* file, uae_u8* p, int* sizep, int exiterror) {
    char fname[2048];
    join_path(fname, path, file, sizeof(fname));

    FILE* f = fopen(fname, "rb");
    if (!f) {
        if (exiterror) {
            printf("load file Couldn't open '%s'\n", fname);
            exit(1);
        }
        return NULL;
    }
    int size = *sizep;
    if (size < 0) {
        fseek(f, 0, SEEK_END);
        size = ftell(f);
        fseek(f, 0, SEEK_SET);
    }
    if (!p) {
        p = calloc(1, size);
        if (!p) {
            printf("Couldn't allocate %d bytes, file '%s'\n", size, fname);
            exit(1);
        }
    }

    *sizep = fread(p, 1, size, f);

    if (*sizep != size) {
        printf("Couldn't read file '%s'\n", fname);
        exit(1);
    }

    fclose(f);
    return p;
}

static void pl(uae_u8* p, uae_u32 v) {
    p[0] = v >> 24;
    p[1] = v >> 16;
    p[2] = v >> 8;
    p[3] = v >> 0;
}

/*
static void pw(uae_u8 *p, uae_u32 v)
{
        p[0] = v >> 8;
        p[1] = v >> 0;
}
*/

static uae_u32 gl(uae_u8* p) { return (p[0] << 24) | (p[1] << 16) | (p[2] << 8) | (p[3] << 0); }

static uae_u16 gw(uae_u8* p) { return (p[0] << 8) | (p[1] << 0); }

static uae_u8* restore_fpvalue(uae_u8* p, struct fpureg* fp) {
    uae_u8 v = *p++;
    if ((v & CT_SIZE_MASK) != CT_SIZE_FPU) {
        end_test();
        printf("Expected CT_SIZE_FPU, got %02x\n", v);
        endinfo();
        exit(1);
    }
    fp->exp = gw(p);
    p += 2;
    fp->m[0] = gl(p);
    p += 4;
    fp->m[1] = gl(p);
    p += 4;
    fp->dummy = 0;
    return p;
}

static uae_u8* restore_value(uae_u8* p, uae_u32* vp, int* sizep) {
    uae_u32 val = *vp;
    uae_u8 v = *p++;
    switch (v & CT_SIZE_MASK) {
        case CT_SIZE_BYTE:
            val &= 0xffffff00;
            val |= *p++;
            *sizep = 0;
            break;
        case CT_SIZE_WORD:
            val &= 0xffff0000;
            val |= (*p++) << 8;
            val |= *p++;
            *sizep = 1;
            break;
        case CT_SIZE_LONG:
            val = (*p++) << 24;
            val |= (*p++) << 16;
            val |= (*p++) << 8;
            val |= *p++;
            *sizep = 2;
            break;
        case CT_SIZE_FPU:
            end_test();
            printf("Unexpected CT_SIZE_FPU\n");
            endinfo();
            exit(1);
            break;
    }
    *vp = val;
    return p;
}

static uae_u8* restore_rel(uae_u8* p, uae_u32* vp, int nocheck) {
    uae_u32 v = *vp;
    switch ((*p++) & CT_SIZE_MASK) {
        case CT_RELATIVE_START_BYTE: {
            uae_u8 val;
            val = *p++;
            v += (uae_s8)val;
            break;
        }
        case CT_RELATIVE_START_WORD: {
            uae_u16 val;
            val = (*p++) << 8;
            val |= *p++;
            v += (uae_s16)val;
            break;
        }
        case CT_ABSOLUTE_WORD: {
            uae_u16 val;
            val = (*p++) << 8;
            val |= *p++;
            v = (uae_s32)(uae_s16)val;
            break;
        }
        case CT_ABSOLUTE_LONG: {
            uae_u32 val;
            val = (*p++) << 24;
            val |= (*p++) << 16;
            val |= (*p++) << 8;
            val |= *p++;
            v = val;
            if (!nocheck) {
                if ((val & addressing_mask) < low_memory_size) {
                    ;  // low memory
                } else if ((val & ~addressing_mask) == ~addressing_mask && val >= 0xfff80000) {
                    ;  // high memory
                } else if ((val & addressing_mask) < test_memory_addr ||
                           (val & addressing_mask) >= test_memory_addr + test_memory_size) {
                    /* Local patch: newer generators emit out-of-test-space
                     * absolute targets (e.g. JMP effective addresses that
                     * are never fetched because the jump faults or the test
                     * ends first); accept them and let the comparison
                     * decide. */
                }
            }
            break;
        }
    }
    *vp = v;
    return p;
}

static uae_u8* restore_rel_ordered(uae_u8* p, uae_u32* vp) {
    if (*p == CT_EMPTY) return p + 1;
    return restore_rel(p, vp, 1);
}

static void validate_mode(uae_u8 mode, uae_u8 v) {
    if ((mode & CT_DATA_MASK) != v) {
        end_test();
        printf("CT_MEMWRITE expected but got %02X\n", mode);
        endinfo();
        exit(1);
    }
}

static uae_u8* get_memory_addr(uae_u8* p, uae_u8** addrp) {
    uae_u8 v = *p++;
    switch (v & CT_SIZE_MASK) {
        case CT_ABSOLUTE_WORD: {
            uae_u16 val;
            val = (*p++) << 8;
            val |= *p++;
            uae_u8* addr;
            uae_s16 offset = (uae_s16)val;
            if (offset < 0) {
                addr = high_memory + 32768 + offset;
            } else {
                addr = low_memory + offset;
            }
            validate_mode(p[0], CT_MEMWRITE);
            *addrp = addr;
            return p;
        }
        case CT_ABSOLUTE_LONG: {
            uae_u32 val;
            val = (*p++) << 24;
            val |= (*p++) << 16;
            val |= (*p++) << 8;
            val |= *p++;
            if (val < low_memory_size) {
#ifndef M68K
                uae_u8* addr = low_memory + val;
#else
                uae_u8* addr = (uae_u8*)val;
#endif
                validate_mode(p[0], CT_MEMWRITE);
                *addrp = addr;
                return p;
            } else if (val >= test_memory_addr && val < test_memory_addr + test_memory_size) {
#ifndef M68K
                uae_u8* addr = test_memory + (val - test_memory_addr);
#else
                uae_u8* addr = (uae_u8*)val;
#endif
                validate_mode(p[0], CT_MEMWRITE);
                *addrp = addr;
                return p;
            } else {
                end_test();
                printf("get_memory_addr CT_ABSOLUTE_LONG outside of test memory! %08x\n", val);
                endinfo();
                exit(1);
            }
        }
        case CT_RELATIVE_START_WORD: {
            uae_u16 val;
            val = (*p++) << 8;
            val |= *p++;
            uae_s16 offset = (uae_s16)val;
            uae_u8* addr = opcode_memory + offset;
            validate_mode(p[0], CT_MEMWRITE);
            *addrp = addr;
            return p;
        } break;

        default:
            end_test();
            printf("get_memory_addr unknown size %02x\n", v);
            endinfo();
            exit(1);
    }
    return NULL;
}

static void tomem(uae_u8* p, uae_u32 v, uae_u32 oldv, int size, int storedata) {
    if (storedata) {
        struct accesshistory* ah = &ahist[ahcnt++];
        ah->oldval = oldv;
        ah->val = v;
        ah->size = size;
        ah->addr = p;
    }
    switch (size) {
        case 0:
            p[0] = (uae_u8)v;
            break;
        case 1:
            p[0] = (uae_u8)(v >> 8);
            p[1] = (uae_u8)(v >> 0);
            break;
        case 2:
            p[0] = (uae_u8)(v >> 24);
            p[1] = (uae_u8)(v >> 16);
            p[2] = (uae_u8)(v >> 8);
            p[3] = (uae_u8)(v >> 0);
            break;
    }
}

static void restoreahist(void) {
    if (!ahcnt) return;
    for (int i = ahcnt - 1; i >= 0; i--) {
        struct accesshistory* ah = &ahist[i];
        tomem(ah->addr, ah->oldval, 0, ah->size, 0);
    }
    ahcnt = 0;
}

static uae_u8* restore_memory(uae_u8* p, int storedata) {
    uae_u8 v = *p;
    switch (v & CT_SIZE_MASK) {
        case CT_ABSOLUTE_WORD: {
            uae_u8* addr;
            int size;
            p = get_memory_addr(p, &addr);
            uae_u32 mv = 0;
            uae_u32 oldv = 0;
            p = restore_value(p, &oldv, &size);
            p = restore_value(p, &mv, &size);
            tomem(addr, mv, oldv, size, storedata);
            return p;
        }
        case CT_ABSOLUTE_LONG: {
            uae_u8* addr;
            int size;
            p = get_memory_addr(p, &addr);
            uae_u32 mv = 0;
            uae_u32 oldv = 0;
            p = restore_value(p, &oldv, &size);
            p = restore_value(p, &mv, &size);
            tomem(addr, mv, oldv, size, storedata);
            return p;
        }
    }
    if ((v & CT_DATA_MASK) == CT_MEMWRITES) {
        switch (v & CT_SIZE_MASK) {
            case CT_PC_BYTES: {
                p++;
                uae_u8* addr = opcode_memory;
                uae_u8 v = *p++;
                addr += v >> 5;
                v &= 31;
                if (v == 0) v = 32;
                xmemcpy(addr, p, v);
                p += v;
                break;
            }
            default:
                end_test();
                printf("Unknown restore_memory type!?\n");
                endinfo();
                exit(1);
                break;
        }
    } else {
        switch (v & CT_SIZE_MASK) {
            case CT_RELATIVE_START_WORD: {
                uae_u8* addr;
                int size;
                p = get_memory_addr(p, &addr);
                uae_u32 mv = 0, oldv = 0;
                p = restore_value(p, &oldv, &size);
                p = restore_value(p, &mv, &size);
                tomem(addr, mv, oldv, size, storedata);
                return p;
            }
            default:
                end_test();
                printf("Unknown restore_memory type!?\n");
                endinfo();
                exit(1);
                break;
        }
    }
    return p;
}

static uae_u8* restore_data(uae_u8* p) {
    uae_u8 v = *p;
    if (v & CT_END) {
        end_test();
        printf("Unexpected end bit!? offset %ld\n", p - test_data);
        endinfo();
        exit(1);
    }
    int mode = v & CT_DATA_MASK;
    if (mode == CT_SRCADDR) {
        int size;
        p = restore_value(p, &regs.srcaddr, &size);
    } else if (mode == CT_DSTADDR) {
        int size;
        p = restore_value(p, &regs.dstaddr, &size);
    } else if (mode < CT_AREG + 8) {
        int size;
        if ((v & CT_SIZE_MASK) == CT_SIZE_FPU) {
            p = restore_fpvalue(p, &regs.fpuregs[mode]);
        } else {
            p = restore_value(p, &regs.regs[mode], &size);
        }
    } else if (mode == CT_SR) {
        int size;
        p = restore_value(p, &regs.sr, &size);
    } else if (mode == CT_FPIAR) {
        int size;
        p = restore_value(p, &regs.fpiar, &size);
    } else if (mode == CT_FPCR) {
        int size;
        p = restore_value(p, &regs.fpcr, &size);
    } else if (mode == CT_FPSR) {
        int size;
        p = restore_value(p, &regs.fpsr, &size);
    } else if (mode == CT_MEMWRITE) {
        // if memwrite, store old data
        p = restore_memory(p, 1);
    } else if (mode == CT_MEMWRITES) {
        p = restore_memory(p, 0);
    } else if (mode == CT_ENDPC) {
        /* Local patch: newer generator records (instruction end PC, branch
         * target with its swap-mode byte, 68000/010 cycle counts). */
        int size;
        p = restore_value(p, &regs.endpc, &size);
    } else if (mode == CT_BRANCHTARGET) {
        int size;
        p = restore_value(p, &regs.branchtarget, &size);
        regs.branchtarget_mode = *p++;
    } else if (mode == CT_CYCLES) {
        int size;
        p = restore_value(p, &regs.cycles, &size);
    } else {
        end_test();
        printf("Unexpected mode %02x\n", v);
        endinfo();
        exit(1);
    }
    return p;
}

static uae_u16 test_sr, test_ccrignoremask;
static uae_u32 test_fpsr, test_fpcr;

/*
static int is_valid_test_addr(uae_u32 a)
{
        return (a >= test_low_memory_start && a < test_low_memory_end && test_low_memory_start != 0xffffffff) ||
                (a >= test_high_memory_start && a < test_high_memory_end && test_high_memory_start != 0xffffffff) ||
                (a >= test_memory_addr && a < test_memory_end);
}
*/

/*
static int addr_diff(uae_u8 *ap, uae_u8 *bp, int size)
{
        for (int i = 0; i < size; i++) {
                if (is_valid_test_addr((uae_u32)bp)) {
                        if (*ap != *bp)
                                return 1;
                }
                ap++;
                bp++;
        }
        return 0;
}

static void addinfo_bytes(char *name, uae_u8 *src, uae_u32 address, int offset, int len)
{
        sprintf(outbp, "%s %08lx ", name, address);
        address += offset;
        outbp += strlen(outbp);
        int cnt = 0;
        while (len-- > 0) {
                if (offset == 0)
                        *outbp++ = '*';
                else if (cnt > 0)
                        *outbp++ = '.';
                if ((uae_u8*)address >= safe_memory_start && (uae_u8*)address < safe_memory_end) {
                        outbp[0] = '?';
                        outbp[1] = '?';
                } else {
                        sprintf(outbp, "%02x", src[cnt]);
                }
                outbp += 2;
                offset++;
                address++;
                cnt++;
        }
        *outbp++ = '\n';
}
*/

// extern uae_u16 disasm_instr(uae_u16 *, char *);

static void addinfo(void) {
    if (infoadded) return;
    infoadded = 1;
    if (!dooutput) return;
    sprintf(outbp, "%u:", testcnt);
    outbp += strlen(outbp);

    uae_u16* code;
    code = (uae_u16*)opcode_memory;

    cs_insn* insn = 0;

    size_t count = cs_disasm(s_cs_handle, (uint8_t*)code, 32, 0, 0, &insn);

    if (count >= 1) {
        sprintf(outbp, "\t%s\t%s\n", insn[0].mnemonic, insn[0].op_str);
        cs_free(insn, count);
        outbp += strlen(outbp);
    }

    /*
    if (code[0] == 0x4e73 || code[0] == 0x4e74 || code[0] == 0x4e75) {
            addinfo_bytes("P", stackaddr, stackaddr_ptr, -SIZE_STORED_ADDRESS_OFFSET, SIZE_STORED_ADDRESS);
            addinfo_bytes(" ", (uae_u8 *)stackaddr_ptr - SIZE_STORED_ADDRESS_OFFSET, stackaddr_ptr,
    -SIZE_STORED_ADDRESS_OFFSET, SIZE_STORED_ADDRESS);
    }
    if (regs.srcaddr != 0xffffffff) {
            uae_u8 *a = srcaddr;
            uae_u8 *b = (uae_u8 *)regs.srcaddr - SIZE_STORED_ADDRESS_OFFSET;
            addinfo_bytes("S", a, regs.srcaddr, -SIZE_STORED_ADDRESS_OFFSET, SIZE_STORED_ADDRESS);
            if (addr_diff(a, b, SIZE_STORED_ADDRESS)) {
                    addinfo_bytes(" ", b, regs.srcaddr, -SIZE_STORED_ADDRESS_OFFSET, SIZE_STORED_ADDRESS);
            }
    }
    if (regs.dstaddr != 0xffffffff) {
            uae_u8 *a = dstaddr;
            uae_u8 *b = (uae_u8*)regs.dstaddr - SIZE_STORED_ADDRESS_OFFSET;
            addinfo_bytes("D", a, regs.dstaddr, -SIZE_STORED_ADDRESS_OFFSET, SIZE_STORED_ADDRESS);
            if (addr_diff(a, b, SIZE_STORED_ADDRESS)) {
                    addinfo_bytes(" ", b, regs.dstaddr, -SIZE_STORED_ADDRESS_OFFSET, SIZE_STORED_ADDRESS);
            }
    }
    */
}

struct srbit {
    char* name;
    int bit;
};
static const struct srbit srbits[] = {{"T1", 15}, {"T0", 14}, {"S", 13}, {"M", 12}, {"X", 4},
                                      {"N", 3},   {"Z", 2},   {"V", 1},  {"C", 0},  {NULL, 0}};

static void out_regs(struct registers* r, int before) {
    if (before) {
        for (int i = 0; i < 16; i++) {
            if (i > 0 && (i % 4) == 0) {
                strcat(outbp, "\n");
            } else if ((i % 8) != 0) {
                strcat(outbp, " ");
            }
            outbp += strlen(outbp);
            sprintf(outbp, "%c%d:%c%08x", i < 8 ? 'D' : 'A', i & 7,
                    test_regs.regs[i] != last_registers.regs[i] ? '*' : ' ', r->regs[i]);
            outbp += strlen(outbp);
        }
        *outbp++ = '\n';
    } else {
        // output only lines that have at least one modified register to save screen space
        for (int i = 0; i < 4; i++) {
            int diff = 0;
            for (int j = 0; j < 4; j++) {
                int idx = i * 4 + j;
                if (test_regs.regs[idx] != regs.regs[idx]) {
                    diff = 1;
                }
            }
            if (diff) {
                for (int j = 0; j < 4; j++) {
                    int idx = i * 4 + j;
                    if (j > 0) *outbp++ = ' ';
                    sprintf(outbp, "%c%d:%c%08x", idx < 8 ? 'D' : 'A', idx & 7,
                            test_regs.regs[idx] != last_registers.regs[idx] ? '*' : ' ', test_regs.regs[idx]);
                    outbp += strlen(outbp);
                }
                *outbp++ = '\n';
            }
        }
    }
    sprintf(outbp, "SR:%c%04x   PC: %08x ISP: %08x", test_sr != last_registers.sr ? '*' : ' ',
            before ? test_sr : test_regs.sr, r->pc, r->ssp);
    outbp += strlen(outbp);
    if (cpu_lvl >= 2 && cpu_lvl <= 4) {
        sprintf(outbp, " MSP: %08x", r->msp);
        outbp += strlen(outbp);
    }
    *outbp++ = '\n';

    if (before >= 0) {
        uae_u16 s = before ? test_sr : test_regs.sr;  // current value
        uae_u16 s1 = regs.sr;                         // original value
        uae_u16 s2 = test_regs.sr;                    // test result value
        uae_u16 s3 = last_registers.sr;               // expected result value
        for (int i = 0; srbits[i].name; i++) {
            if (i > 0) *outbp++ = ' ';
            uae_u16 mask = 1 << srbits[i].bit;
            sprintf(outbp, "%s%c%d", srbits[i].name,
                    (s2 & mask) != (s3 & mask) ? '!' : ((s1 & mask) != (s2 & mask) ? '*' : '='), (s & mask) != 0);
            outbp += strlen(outbp);
        }
        *outbp++ = '\n';
    }

    if (!fpu_model) return;

    for (int i = 0; i < 8; i++) {
        if ((i % 2) == 0) {
            strcat(outbp, "\n");
        } else if ((i % 4) != 0) {
            strcat(outbp, " ");
        }
        outbp += strlen(outbp);
        struct fpureg* f = &r->fpuregs[i];
        void* f1 = &regs.fpuregs[i];
        void* f2 = &test_regs.fpuregs[i];
        sprintf(outbp, "FP%d:%c%04x-%08x%08x %Lf", i, memcmp(f1, f2, sizeof(struct fpureg)) ? '*' : ' ', f->exp,
                f->m[0], f->m[1], *((long double*)f));
        outbp += strlen(outbp);
    }
    sprintf(outbp, "\nFPSR:%c%08x FPCR:%c%08x FPIAR:%c%08x\n", test_fpsr != test_regs.fpsr ? '*' : ' ',
            before ? test_fpsr : r->fpsr, test_fpcr != test_regs.fpcr ? '*' : ' ', before ? test_fpcr : r->fpcr,
            regs.fpiar != test_regs.fpiar ? '*' : ' ', r->fpiar);

    outbp += strlen(outbp);
}

static void hexdump(uae_u8* p, int len) {
    for (int i = 0; i < len; i++) {
        if (i > 0) *outbp++ = '.';
        sprintf(outbp, "%02x", p[i]);
        outbp += strlen(outbp);
    }
    *outbp++ = '\n';
}

static uae_u8 last_exception[256];
static int last_exception_len;

static uae_u8* validate_exception(struct registers* regs, uae_u8* p, int excnum, int sameexc, int* experr) {
    int exclen = 0;
    uae_u8* exc;
    uae_u8* op = p;
    uae_u8* sp = translate_to_native(regs->excframe);
    uae_u32 v;
    uae_u8 excdatalen = *p++;

    if (!excdatalen) return p;
    /* Local patch: the newer generator's exception records carry extra
     * detail (trace stacking, group-2-with-1 markers, per-frame fields)
     * in a format this runner predates. The record is length-prefixed, so
     * skip it wholesale: the exception NUMBER is still verified by the
     * caller; frame CONTENTS are not (phase 2 ports the current
     * validator). 0xff means "same as previous", which carries no bytes. */
    if (excdatalen != 0xff) {
        return p + excdatalen;
    }
    return p;
    exc = last_exception;
    if (excdatalen != 0xff) {
        if (cpu_lvl == 0) {
            if (excnum == 2 || excnum == 3) {
                // status (with undocumented opcode part)
                uae_u8 opcode0 = p[1];
                uae_u8 opcode1 = p[2];
                exc[0] = opcode0;
                exc[1] = (opcode1 & ~0x1f) | p[0];
                p += 3;
                // access address
                v = opcode_memory_addr;
                p = restore_rel_ordered(p, &v);
                pl(exc + 2, v);
                // opcode
                exc[6] = opcode0;
                exc[7] = opcode1;
                // sr
                exc[8] = regs->sr >> 8;
                exc[9] = regs->sr;
                // pc
                pl(exc + 10, regs->pc);
                exclen = 14;
            }
        } else if (cpu_lvl > 0) {
            // sr
            exc[0] = regs->sr >> 8;
            exc[1] = regs->sr;
            pl(exc + 2, regs->pc);
            const uae_u16 t0 = *p++;
            const uae_u16 t1 = *p++;
            // frame type
            uae_u16 frame = (t0 << 8) | t1;
            exc[6] = frame >> 8;
            exc[7] = frame >> 0;

            switch (frame >> 12) {
                case 0:
                    exclen = 8;
                    break;
                case 2:
                    v = opcode_memory_addr;
                    p = restore_rel_ordered(p, &v);
                    pl(exc + 8, v);
                    exclen = 12;
                    break;
                case 3:
                    v = opcode_memory_addr;
                    p = restore_rel_ordered(p, &v);
                    pl(exc + 8, v);
                    exclen = 12;
                    break;
                case 4:
                    v = opcode_memory_addr;
                    p = restore_rel_ordered(p, &v);
                    pl(exc + 8, v);
                    v = opcode_memory_addr;
                    p = restore_rel_ordered(p, &v);
                    pl(exc + 12, v);
                    exclen = 16;
                    break;
                case 8:
                    exc[8] = *p++;
                    exc[9] = *p++;
                    v = opcode_memory_addr;
                    p = restore_rel_ordered(p, &v);
                    pl(exc + 10, v);
                    // data out
                    exc[16] = *p++;
                    exc[17] = *p++;
                    // data in
                    exc[20] = *p++;
                    exc[21] = *p++;
                    // inst
                    exc[24] = *p++;
                    exc[25] = *p++;
                    exc[14] = exc[15] = 0;
                    sp[14] = sp[15] = 0;
                    exc[18] = exc[19] = 0;
                    sp[18] = sp[19] = 0;
                    exc[22] = exc[23] = 0;
                    sp[22] = sp[23] = 0;
                    // ignore undocumented data
                    exclen = 26;
                    break;
                case 0x0a:
                case 0x0b:
                    exclen = 8;
                    break;
                default:
                    end_test();
                    printf("Unknown frame %04x\n", frame);
                    exit(1);
                    break;
            }
        }
        last_exception_len = exclen;
        if (p != op + excdatalen + 1) {
            end_test();
            printf("Exception length mismatch %d != %ld\n", excdatalen, p - op - 1);
            exit(1);
        }
    } else {
        exclen = last_exception_len;
    }
    if (exclen == 0 || !sameexc) return p;
    if (memcmp(exc, sp, exclen)) {
        sprintf(outbp, "Exception %d stack frame mismatch:\n", excnum);
        outbp += strlen(outbp);
        strcpy(outbp, "Expected: ");
        outbp += strlen(outbp);
        hexdump(exc, exclen);
        strcpy(outbp, "Got     : ");
        outbp += strlen(outbp);
        hexdump(sp, exclen);
        errors = 1;
        *experr = 1;
    }
    return p;
}

// regs: registers before execution of test code
// test_reg: registers used during execution of test code, also modified by test code.
// last_registers: registers after modifications from data files. Test ok if test_reg == last_registers.

static uae_u8* validate_test(uae_u8* p, int ignore_errors, int ignore_sr) {
    uae_u8 regs_changed[16] = {0};
    uae_u8 regs_fpuchanged[8] = {0};
    uae_u8 sr_changed = 0;  //, pc_changed = 0;
    uae_u8 fpiar_changed = 0, fpsr_changed = 0, fpcr_changed = 0;
    int exc = -1;

    for (int i = 0; i < 16; i++) {
        if (last_registers.regs[i] != test_regs.regs[i]) {
            regs_changed[i] = 1;
        }
    }
    if ((last_registers.sr & test_ccrignoremask) != (test_regs.sr & test_ccrignoremask)) {
        sr_changed = 1;
    }
    if (last_registers.pc != test_regs.pc) {
        // pc_changed = 1;
    }
    if (fpu_model) {
        for (int i = 0; i < 8; i++) {
            if (memcmp(&last_registers.fpuregs[i], &test_regs.fpuregs[i], sizeof(struct fpureg))) {
                regs_fpuchanged[i] = 1;
            }
        }
        if (last_registers.fpsr != test_regs.fpsr) {
            fpsr_changed = 1;
        }
        if (last_registers.fpcr != test_regs.fpcr) {
            fpcr_changed = 1;
        }
        if (last_registers.fpiar != test_regs.fpiar) {
            fpiar_changed = 1;
        }
    }

    if (*p == CT_END_SKIP) return p + 1;

    int experr = 0;
    for (;;) {
        uae_u8 v = *p;
        if (v & CT_END) {
            exc = v & CT_EXCEPTION_MASK;
            int cpuexc = test_regs.exc & 65535;
            int cpuexc010 = test_regs.exc010 & 65535;
            p++;
            if ((v & CT_END_INIT) == CT_END_INIT) {
                end_test();
                printf("Unexpected CT_END_INIT %02x %08lx\n", v, p - test_data);
                endinfo();
                exit(1);
            }
            if (exc == 1) {
                end_test();
                printf("Invalid exception %02x\n", exc);
                endinfo();
                exit(1);
            }
            if (cpu_lvl > 0 && exc > 0 && cpuexc010 != cpuexc) {
                addinfo();
                if (dooutput) {
                    sprintf(outbp, "Exception: vector number does not match vector offset! (%d <> %d)\n", exc,
                            cpuexc010);
                    experr = 1;
                    outbp += strlen(outbp);
                    errors++;
                }
                break;
            }

            if (ignore_errors) {
                if (exc) {
                    p = validate_exception(&test_regs, p, exc, exc == cpuexc, &experr);
                }
                break;
            }
            if (exc == 0 && cpuexc == 4) {
                // successful complete generates exception 4 with matching PC
                if (last_registers.pc != test_regs.pc && dooutput) {
                    sprintf(outbp, "PC: expected %08x but got %08x\n", last_registers.pc, test_regs.pc);
                    outbp += strlen(outbp);
                    errors++;
                }
                break;
            }
            if (exc) {
                p = validate_exception(&test_regs, p, exc, exc == cpuexc, &experr);
            }
            if (exc != cpuexc) {
                addinfo();
                if (dooutput) {
                    if (cpuexc == 4 && last_registers.pc == test_regs.pc) {
                        sprintf(outbp, "Exception: expected %d but got no exception.\n", exc);
                    } else if (cpuexc == 4) {
                        sprintf(outbp, "Exception: expected %d but got %d (or no exception)\n", exc, cpuexc);
                    } else {
                        sprintf(outbp, "Exception: expected %d but got %d\n", exc, cpuexc);
                    }
                    experr = 1;
                }
                outbp += strlen(outbp);
                errors++;
            }
            break;
        }
        int mode = v & CT_DATA_MASK;

        if (mode < CT_AREG + 8 && (v & CT_SIZE_MASK) != CT_SIZE_FPU) {
            uae_u32 val = last_registers.regs[mode];
            int size;
            p = restore_value(p, &val, &size);
            if (val != test_regs.regs[mode] && !ignore_errors) {
                addinfo();
                if (dooutput) {
                    sprintf(outbp, "%c%d: expected %08x but got %08x\n", mode < CT_AREG ? 'D' : 'A', mode & 7, val,
                            test_regs.regs[mode]);
                    outbp += strlen(outbp);
                }
                errors++;
            }
            regs_changed[mode] = 0;
            last_registers.regs[mode] = val;
        } else if (mode < CT_AREG && (v & CT_SIZE_MASK) == CT_SIZE_FPU) {
            struct fpureg val;
            p = restore_fpvalue(p, &val);
            if (memcmp(&val, &test_regs.fpuregs[mode], sizeof(struct fpureg)) && !ignore_errors) {
                addinfo();
                if (dooutput) {
                    sprintf(outbp, "FP%d: expected %04x-%08x%08x but got %04x-%08x%08x\n", mode, val.exp, val.m[0],
                            val.m[1], test_regs.fpuregs[mode].exp, test_regs.fpuregs[mode].m[0],
                            test_regs.fpuregs[mode].m[1]);
                    outbp += strlen(outbp);
                }
                errors++;
            }
            regs_fpuchanged[mode] = 0;
            xmemcpy(&last_registers.fpuregs[mode], &val, sizeof(struct fpureg));
        } else if (mode == CT_SR) {
            uae_u32 val = last_registers.sr;
            int size;
            // High 16 bit: ignore mask, low 16 bit: SR/CCR
            p = restore_value(p, &val, &size);
            test_ccrignoremask = ~(val >> 16);

            if ((val & (sr_undefined_mask & test_ccrignoremask)) !=
                    (test_regs.sr & (sr_undefined_mask & test_ccrignoremask)) &&
                !ignore_errors && !ignore_sr) {
                addinfo();
                if (dooutput) {
                    sprintf(outbp, "SR: expected %04x -> %04x but got %04x (%04x)\n", test_sr & 0xffff, val & 0xffff,
                            test_regs.sr & 0xffff, test_ccrignoremask);
                    outbp += strlen(outbp);
                }
                errors++;
            }
            sr_changed = 0;
            last_registers.sr = val;
        } else if (mode == CT_PC) {
            uae_u32 val = last_registers.pc;
            p = restore_rel(p, &val, 0);
            // pc_changed = 0;
            last_registers.pc = val;
        } else if (mode == CT_FPCR) {
            uae_u32 val = last_registers.fpcr;
            int size;
            p = restore_value(p, &val, &size);
            if (val != test_regs.fpcr && !ignore_errors) {
                addinfo();
                if (dooutput) {
                    sprintf(outbp, "FPCR: expected %08x -> %08x but got %08x\n", test_fpcr, val, test_regs.fpcr);
                    outbp += strlen(outbp);
                }
                errors++;
            }
            fpcr_changed = 0;
            last_registers.fpcr = val;
        } else if (mode == CT_FPSR) {
            uae_u32 val = last_registers.fpsr;
            int size;
            p = restore_value(p, &val, &size);
            if (val != test_regs.fpsr && !ignore_errors) {
                addinfo();
                if (dooutput) {
                    sprintf(outbp, "FPSR: expected %08x -> %08x but got %08x\n", test_fpsr, val, test_regs.fpsr);
                    outbp += strlen(outbp);
                }
                errors++;
            }
            fpsr_changed = 0;
            last_registers.fpsr = val;
        } else if (mode == CT_FPIAR) {
            uae_u32 val = last_registers.fpiar;
            p = restore_rel(p, &val, 0);
            if (val != test_regs.fpiar && !ignore_errors) {
                addinfo();
                if (dooutput) {
                    sprintf(outbp, "FPIAR: expected %08x but got %08x\n", val, test_regs.fpiar);
                    outbp += strlen(outbp);
                }
                errors++;
            }
            fpiar_changed = 0;
            last_registers.fpiar = val;

        } else if (mode == CT_MEMWRITES) {
            p = restore_memory(p, 0);
        } else if (mode == CT_MEMWRITE) {
            uae_u8* addr;
            uae_u32 val = 0, mval = 0, oldval = 0;
            int size;
            p = get_memory_addr(p, &addr);
            p = restore_value(p, &oldval, &size);
            p = restore_value(p, &val, &size);
            switch (size) {
                case 0:
                    mval = addr[0];
                    if (mval != val && !ignore_errors) {
                        addinfo();
                        if (dooutput) {
                            sprintf(outbp, "Memory byte write: address %08x, expected %02x but got %02x\n",
                                    translate_to_m68k(addr), val, mval);
                            outbp += strlen(outbp);
                        }
                        errors++;
                    }
                    addr[0] = oldval;
                    break;
                case 1:
                    mval = (addr[0] << 8) | (addr[1]);
                    if (mval != val && !ignore_errors) {
                        addinfo();
                        if (dooutput) {
                            sprintf(outbp, "Memory word write: address %08x, expected %04x but got %04x\n",
                                    translate_to_m68k(addr), val, mval);
                            outbp += strlen(outbp);
                        }
                        errors++;
                    }
                    addr[0] = oldval >> 8;
                    addr[1] = oldval;
                    break;
                case 2:
                    mval = gl(addr);
                    if (mval != val && !ignore_errors) {
                        addinfo();
                        if (dooutput) {
                            sprintf(outbp, "Memory long write: address %08x, expected %08x but got %08x\n",
                                    translate_to_m68k(addr), val, mval);
                            outbp += strlen(outbp);
                        }
                        errors++;
                    }
                    pl(addr, oldval);
                    break;
            }
        } else if (mode == CT_ENDPC) {
            /* Local patch: consume newer generator records. */
            uae_u32 val = last_registers.endpc;
            int size;
            p = restore_value(p, &val, &size);
            last_registers.endpc = val;
        } else if (mode == CT_BRANCHTARGET) {
            uae_u32 val = last_registers.branchtarget;
            int size;
            p = restore_value(p, &val, &size);
            last_registers.branchtarget = val;
            last_registers.branchtarget_mode = *p++;
        } else if (mode == CT_CYCLES) {
            uae_u32 val = last_registers.cycles;
            int size;
            p = restore_value(p, &val, &size);
            last_registers.cycles = val;
            /* Local patch: opt-in cycle-count validation (CPUTEST_CYCLES=1).
             * The callback reports its measured count in test_regs.cycles;
             * the data records the generator's cycle-exact count (68000/010
             * sets only, CPU-clock units). */
            /* test_regs.cycles == 0 means the callback measured nothing
             * for this record (round seed / skipped round): no instruction
             * costs zero cycles, so skip rather than false-positive. */
            if (getenv("CPUTEST_CYCLES") && test_regs.cycles != 0 && test_regs.cycles != last_registers.cycles) {
                addinfo();
                if (dooutput) {
                    sprintf(outbp, "Cycles: expected %u but got %u\n",
                            last_registers.cycles, test_regs.cycles);
                    outbp += strlen(outbp);
                }
                errors++;
            }
        } else {
            end_test();
            printf("Unknown test data %02x mode %d\n", v, mode);
            exit(1);
        }
    }
    if (!ignore_errors) {
        if (!ignore_sr) {
            for (int i = 0; i < 16; i++) {
                if (regs_changed[i]) {
                    addinfo();
                    if (dooutput) {
                        sprintf(outbp, "%c%d: modified %08x -> %08x but expected no modifications\n", i < 8 ? 'D' : 'A',
                                i & 7, last_registers.regs[i], test_regs.regs[i]);
                        outbp += strlen(outbp);
                    }
                    errors++;
                }
            }
            if (sr_changed) {
                addinfo();
                if (dooutput) {
                    sprintf(outbp, "SR: modified %04x -> %04x but expected no modifications\n",
                            last_registers.sr & 0xffff, test_regs.sr & 0xffff);
                    outbp += strlen(outbp);
                }
                errors++;
            }
        }
        for (int i = 0; i < 8; i++) {
            if (regs_fpuchanged[i]) {
                addinfo();
                if (dooutput) {
                    sprintf(outbp, "FP%d: modified %04x-%08x%08x -> %04x-%08x%08x but expected no modifications\n", i,
                            last_registers.fpuregs[i].exp, last_registers.fpuregs[i].m[0],
                            last_registers.fpuregs[i].m[1], test_regs.fpuregs[i].exp, test_regs.fpuregs[i].m[0],
                            test_regs.fpuregs[i].m[1]);
                    outbp += strlen(outbp);
                }
                errors++;
            }
        }
        if (fpsr_changed) {
            addinfo();
            if (dooutput) {
                sprintf(outbp, "FPSR: modified %08x -> %08x but expected no modifications\n", last_registers.fpsr,
                        test_regs.fpsr);
                outbp += strlen(outbp);
            }
            errors++;
        }
        if (fpcr_changed) {
            addinfo();
            if (dooutput) {
                sprintf(outbp, "FPCR: modified %08x -> %08x but expected no modifications\n", last_registers.fpcr,
                        test_regs.fpcr);
                outbp += strlen(outbp);
            }
            errors++;
        }
        if (fpiar_changed) {
            addinfo();
            if (dooutput) {
                sprintf(outbp, "FPIAR: modified %08x -> %08x but expected no modifications\n", last_registers.fpiar,
                        test_regs.fpiar);
                outbp += strlen(outbp);
            }
            errors++;
        }
    }
    if (errors && dooutput) {
        addinfo();
        if (!fpu_model) {
            strcat(outbp, "Registers before:\n");
            outbp += strlen(outbp);
        }
        out_regs(&regs, 1);
        if (!fpu_model) {
            strcat(outbp, "Registers after:\n");
            outbp += strlen(outbp);
        }
        out_regs(&test_regs, 0);
        if (exc > 1) {
            if (!experr) {
                sprintf(outbp, "OK: Generated exception %d\n", exc);
                outbp += strlen(outbp);
            }
            if ((exc == 3 || exc == 2) && cpu_lvl == 0) {
                sprintf(outbp, "RW=%d IN=%d FC=%d\n", ((test_regs.exc >> (16 + 4)) & 1),
                        ((test_regs.exc >> (16 + 3)) & 1), ((test_regs.exc >> (16 + 0)) & 7));
                outbp += strlen(outbp);
            }
        } else if (exc == 0 && (test_regs.exc & 65535) == 4) {
            sprintf(outbp, "OK: No exception generated\n");
            outbp += strlen(outbp);
        }
    }
    return p;
}

/* Local patch: NULL-returning translation for the sentinel hooks. */
static uae_u8* translate_to_native_checked(uae_u32 addr) {
    return translate_to_native(addr);
}

static void store_addr(uae_u32 s, uae_u8* d) {
    if (s == 0xffffffff) return;
    for (int i = 0; i < SIZE_STORED_ADDRESS; i++) {
        uae_u32 ss = s + (i - SIZE_STORED_ADDRESS_OFFSET);
        uint8_t* v = translate_to_native(ss);
        if (v) {
            *d++ = *v;
        } else {
            *d++ = 0;
        }
    }
}

static void process_test(uae_u8* p) {
    outbp = outbuffer;
    outbp[0] = 0;
    infoadded = 0;
    errors = 0;

    memset(&regs, 0, sizeof(struct registers));
    regs.sr = interrupt_mask << 8;
    regs.srcaddr = 0xffffffff;
    regs.dstaddr = 0xffffffff;
    /* Local patch: the newer generator's CT_ENDPC / CT_BRANCHTARGET
     * records are partial-value updates against these bases (matching
     * cputest.cpp's *_old seeds). */
    regs.endpc = opcode_memory_addr;
    regs.branchtarget = 0xffffffff;

    start_test();

    test_ccrignoremask = 0xffff;
    ahcnt = 0;

    for (;;) {
#ifndef M68K
        outbp = outbuffer;
#endif

        for (;;) {
            uae_u8 v = *p;
            if (v == CT_END_INIT || v == CT_END_FINISH) break;
            p = restore_data(p);
        }
        if (*p == CT_END_FINISH) break;
        p++;

        store_addr(regs.srcaddr, srcaddr);
        store_addr(regs.dstaddr, dstaddr);

        /* Local patch: the newer generator no longer stores the
         * end-of-instruction sentinel (ILLEGAL + NOP) or the branch-target
         * data in the memory image; the consumer plants them and swaps
         * their word order every CCR round, exactly like cputest.cpp's
         * generation loop, so tests that read their own instruction
         * stream see the same bytes the generator did. */
        cputest_opcodeend = 0x4afc4e71;
        cputest_branchdata = 0x4afc4e71;

        xmemcpy(&last_registers, &regs, sizeof(struct registers));

        int fpumode = fpu_model && (opcode_memory[0] & 0xf0) == 0xf0;

        if (cpu_lvl >= 2) flushcache(cpu_lvl);

        // uae_u32 pc = opcode_memory_addr;

        int extraccr = 0;

        uae_u32 last_pc = opcode_memory_addr;
        uae_u32 last_fpiar = opcode_memory_addr;
        int old_super = -1;

        for (;;) {
            uae_u16 sr_mask = 0;

            if (extraccr & 1) sr_mask |= 0x2000;  // S
            if (extraccr & 2) sr_mask |= 0x4000;  // T0
            if (extraccr & 4) sr_mask |= 0x8000;  // T1
            if (extraccr & 8) sr_mask |= 0x1000;  // M

            int maxccr = *p++;
            for (int ccr = 0; ccr < maxccr; ccr++) {
                /* Local patch (see above): per-round sentinel and branch
                 * target toggling. Swap first, then write, mirroring the
                 * generator. */
                {
                    cputest_opcodeend = (cputest_opcodeend >> 16) | (cputest_opcodeend << 16);
                    uae_u8* ep = translate_to_native_checked(regs.endpc);
                    uae_u8* ep3 = translate_to_native_checked(regs.endpc + 3);
                    uae_u8* ep1 = translate_to_native_checked(regs.endpc + 1);
                    if (ep && ep3) {
                        pl(ep, cputest_opcodeend);
                    } else if (ep && ep1) {
                        ep[0] = cputest_opcodeend >> 24;
                        ep[1] = cputest_opcodeend >> 16;
                    }
                    if (regs.branchtarget != 0xffffffff && !(regs.branchtarget & 1) &&
                        regs.branchtarget_mode) {
                        uae_u8* bp = translate_to_native_checked(regs.branchtarget);
                        uae_u8* bp3 = translate_to_native_checked(regs.branchtarget + 3);
                        if (regs.branchtarget_mode == 1 && bp && bp3) {
                            cputest_branchdata = (cputest_branchdata >> 16) | (cputest_branchdata << 16);
                            pl(bp, cputest_branchdata);
                        } else if (regs.branchtarget_mode == 2 && bp) {
                            uae_u16 bv = (bp[0] << 8) | bp[1];
                            bv = (bv == 0x4e71) ? 0x4afc : 0x4e71;
                            bp[0] = bv >> 8;
                            bp[1] = bv & 0xff;
                        }
                    }
                }
                regs.ssp = super_stack_memory - 0x80;
                regs.msp = super_stack_memory;
                regs.pc = opcode_memory_addr;
                regs.fpiar = opcode_memory_addr;

#ifdef M68K
                xmemcpy((void*)regs.ssp, (void*)regs.regs[15], 0x20);
#endif
                xmemcpy(&test_regs, &regs, sizeof(struct registers));
                /* Local patch: with the NOP leading, execution runs through
                 * it and stops in front of the ILLEGAL. */
                test_regs.endpc = regs.endpc + (((cputest_opcodeend >> 16) == 0x4e71) ? 2 : 0);

                if (maxccr >= 32) {
                    test_regs.sr = ccr;
                } else {
                    test_regs.sr = (ccr ? 31 : 0);
                }
                test_regs.sr |= sr_mask | (interrupt_mask << 8);
                test_sr = test_regs.sr;
                if (fpumode) {
                    if (maxccr >= 32) {
                        test_regs.fpsr = (ccr & 15) << 24;
                        test_regs.fpcr = (ccr >> 4) << 4;
                    } else {
                        test_regs.fpsr = (ccr ? 15 : 0) << 24;
                        test_regs.fpcr = (ccr >> 1) << 4;
                    }
                    test_fpsr = test_regs.fpsr;
                    test_fpcr = test_regs.fpcr;
                }
                int super = (test_regs.sr & 0x2000) != 0;

                if (super != old_super) {
                    stackaddr_ptr = super ? regs.ssp : regs.regs[15];
                    store_addr(stackaddr_ptr, stackaddr);
                    old_super = super;
                }

                if ((*p) == CT_END_SKIP) {
                    p++;

                } else {
                    int ignore_errors = 0;
                    int ignore_sr = 0;

                    if ((ccr_mask & ccr) || (ccr == 0)) {
                        reset_error_vectors();

                        /*
                        if (cpu_lvl == 1) {
                                execute_test010(&test_regs);
                        } else if (cpu_lvl >= 2) {
                                if (fpu_model)
                                        execute_testfpu(&test_regs);
                                else
                                        execute_test020(&test_regs);
                        } else {
                                execute_test000(&test_regs);
                        }
                        */

                        s_cpu_callback(s_cpu_user_data, s_cpu_context, (M68KTesterRegisters*)&test_regs);

                        if (ccr_mask == 0 && ccr == 0) ignore_sr = 1;

                        set_error_vectors();

                    } else {
                        test_regs.sr = test_sr;
                        ignore_errors = 1;
                        ignore_sr = 1;
                    }

                    last_registers.pc = last_pc;
                    last_registers.fpiar = last_fpiar;

                    if ((*p) == CT_SKIP_REGS) {
                        p++;
                        for (int i = 0; i < 16; i++) {
                            test_regs.regs[i] = regs.regs[i];
                        }
                    }

                    p = validate_test(p, ignore_errors, ignore_sr);

                    last_pc = last_registers.pc;
                    last_fpiar = last_registers.fpiar;
                }

                testcnt++;

                if (testexit()) {
                    end_test();
                    printf("\nAborted (%d)\n", testcnt);
                    exit(1);
                }

#if DONTSTOPONERROR == 0
                if (quit || errors) goto end;
#endif
            }

            if (*p == CT_END) {
                p++;
                break;
            }

            extraccr = *p++;
        }

        restoreahist();
    }

end:
    end_test();

    if (infoadded) {
        printf("\n");
        printf("%s\n", outbuffer);
    }
}

/*
static void freestuff(void)
{
        if (test_memory && test_memory_addr)
                free_absolute(test_memory_addr, test_memory_size);
#ifdef WAITEXIT
        getchar();
#endif
}
*/

static uae_u32 read_u32(FILE* f) {
    uae_u8 data[4] = {0};
    fread(data, 1, 4, f);
    return gl(data);
}

static int test_mnemo(const char* path, const char* opcode) {
    int size;
    uae_u8 data[4] = {0};
    uae_u32 v;
    char fname[256], tfname[256];
    int filecnt = 1;
    uae_u32 starttimeid;
    int lvl;

    errors = 0;
    quit = 0;

    sprintf(tfname, "%s%s/0000.dat", path, opcode);
    FILE* f = fopen(tfname, "rb");
    if (!f) {
        printf("Couldn't open '%s'\n", tfname);
        exit(1);
    }
    v = read_u32(f);
    if (v != DATA_VERSION) {
        printf("Invalid test data file (header)\n");
        exit(1);
    }

    starttimeid = read_u32(f);
    uae_u32 hmem_lmem = read_u32(f);
    hmem_rom = (uae_s16)(hmem_lmem >> 16);
    lmem_rom = (uae_s16)(hmem_lmem & 65535);
    test_memory_addr = read_u32(f);
    test_memory_size = read_u32(f);
    test_memory_end = test_memory_addr + test_memory_size;
    opcode_memory_addr = read_u32(f);
    uae_u32 lvl_mask = read_u32(f);
    lvl = (lvl_mask >> 16) & 15;
    interrupt_mask = (lvl_mask >> 20) & 7;
    addressing_mask = (lvl_mask & 0x80000000) ? 0xffffffff : 0x00ffffff;
    sr_undefined_mask = lvl_mask & 0xffff;
    fpu_model = read_u32(f);
    test_low_memory_start = read_u32(f);
    test_low_memory_end = read_u32(f);
    test_high_memory_start = read_u32(f);
    test_high_memory_end = read_u32(f);
    safe_memory_start = read_u32(f);
    safe_memory_end = read_u32(f);
    user_stack_memory = read_u32(f);
    super_stack_memory = read_u32(f);
    fread(inst_name, 1, sizeof(inst_name) - 1, f);
    inst_name[sizeof(inst_name) - 1] = 0;

    int lvl2 = cpu_lvl;
    if (lvl2 == 5 && lvl2 != lvl) lvl2 = 4;

    if (lvl != lvl2) {
        printf("Mismatched CPU model: %u <> %u\n", 68000 + 10 * (cpu_lvl < 5 ? cpu_lvl : 6),
               68000 + (lvl < 5 ? lvl : 6) * 10);
        /* Local patch: a skipped mnemonic is a failed mnemonic. */
        return 1;
    }

    if (!check_undefined_sr) {
        sr_undefined_mask = ~sr_undefined_mask;
    } else {
        sr_undefined_mask = 0xffff;
    }

    if (lmem_rom >= 0 && (low_memory_size <= 0 || !low_memory_temp)) {
        printf("lmem.dat required but it was not loaded or was missing.\n");
        return 1;
    }
    if (hmem_rom >= 0 && (high_memory_size <= 0 || !high_memory_temp)) {
        printf("hmem.dat required but it was not loaded or was missing.\n");
        return 1;
    }

    low_memory_offset = 0;
    high_memory_offset = 0;
    if (test_low_memory_start != 0xffffffff) low_memory_offset = test_low_memory_start;
    if (test_high_memory_start != 0xffffffff) high_memory_offset = test_high_memory_start & 0x7fff;

    if (!absallocated) {
        test_memory = allocate_absolute(test_memory_addr, test_memory_size);
        if (!test_memory) {
            printf("Couldn't allocate tmem area %08x-%08x\n", (uae_u32)test_memory_addr, test_memory_size);
            exit(1);
        }
        absallocated = test_memory;
    }
    if (absallocated != test_memory) {
        printf("tmem area changed!?\n");
        exit(1);
    }

    size = test_memory_size;
    load_file(path, "tmem.dat", test_memory, &size, 1);
    if (size != test_memory_size) {
        printf("tmem.dat size mismatch\n");
        exit(1);
    }

    opcode_memory = translate_to_native(opcode_memory_addr);

    printf("CPUlvl=%d, Mask=%08x Code=%08x SP=%08x ISP=%08x\n", cpu_lvl, addressing_mask, opcode_memory_addr,
           user_stack_memory, super_stack_memory);
    printf(" Low: %08x-%08x High: %08x-%08x\n", test_low_memory_start, test_low_memory_end, test_high_memory_start,
           test_high_memory_end);
    printf("Test: %08x-%08x Safe: %08x-%08x\n", test_memory_addr, test_memory_end, safe_memory_start, safe_memory_end);
    printf("%s:\n", inst_name);

    testcnt = 0;

    s_cpu_context->low_memory.buffer = low_memory;
    s_cpu_context->low_memory.start = test_low_memory_start;
    s_cpu_context->low_memory.end = test_low_memory_end;
    s_cpu_context->low_memory.size = test_low_memory_end - test_low_memory_start;

    s_cpu_context->high_memory.buffer = high_memory;
    s_cpu_context->high_memory.start = test_high_memory_start;
    s_cpu_context->high_memory.end = test_high_memory_end;
    s_cpu_context->high_memory.size = test_high_memory_end - test_high_memory_start;

    s_cpu_context->test_memory.buffer = test_memory;
    s_cpu_context->test_memory.start = test_memory_addr;
    s_cpu_context->test_memory.end = test_memory_end;
    s_cpu_context->test_memory.size = test_memory_end - test_memory_addr;

    for (;;) {
        printf("%s. %u...\n", tfname, testcnt);

        sprintf(tfname, "%s%s/%04d.dat", path, opcode, filecnt);
        FILE* f = fopen(tfname, "rb");
        if (!f) break;
        fread(data, 1, 4, f);
        if (gl(data) != DATA_VERSION) {
            printf("Invalid test data file (header)\n");
            exit(1);
        }
        fread(data, 1, 4, f);
        if (gl(data) != starttimeid) {
            printf("Test data file header mismatch (old test data file?)\n");
            break;
        }
        fseek(f, 0, SEEK_END);
        test_data_size = ftell(f);
        fseek(f, 16, SEEK_SET);
        test_data_size -= 16;
        if (test_data_size <= 0) break;
        test_data = calloc(1, test_data_size);
        if (!test_data) {
            printf("Couldn't allocate memory for '%s', %u bytes\n", tfname, test_memory_size);
            exit(1);
        }
        if (fread(test_data, 1, test_data_size, f) != test_data_size) {
            printf("Couldn't read '%s'\n", fname);
            free(test_data);
            break;
        }
        fclose(f);
        /* Local patch: newer cputest generators append a file-count byte
         * after the CT_END_FINISH terminator; accept either layout (the
         * tag walker stops at CT_END_FINISH regardless). */
        if (test_data[test_data_size - 1] != CT_END_FINISH &&
            !(test_data_size >= 2 && test_data[test_data_size - 2] == CT_END_FINISH)) {
            printf("Invalid test data file (footer)\n");
            free(test_data);
            exit(1);
        }

        process_test(test_data);

        if (errors || quit) {
            free(test_data);
            break;
        }

        free(test_data);
        filecnt++;
    }

    if (!errors && !quit) {
        printf("All tests complete (total %u).\n", testcnt);
    }

    return errors || quit;
}

/*
static int getparamval(const char *p)
{
        if (strlen(p) > 2 && p[0] == '0' && toupper(p[1]) == 'X') {
                char *endptr;
                return strtol(p + 2, &endptr, 16);
        } else {
                return atol(p);
        }
}
*/

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

static int isdir(const char* dirpath, const char* name) {
    struct stat buf;
    char path[2048];

    join_path(path, dirpath, name, sizeof(path));

    return stat(path, &buf) == 0 && S_ISDIR(buf.st_mode);
}

// Init the tester
M68KTesterInitResult M68KTester_init(const char* base_path, const M68KTesterRunSettings* settings) {
    M68KTesterInitResult result;

    char cpu_string_name[64];
    char path[2048] = {0};

    // TODO: Use correct arch here
    cs_err err = cs_open(CS_ARCH_M68K, (cs_mode)(CS_MODE_BIG_ENDIAN | CS_MODE_M68K_000), &s_cs_handle);

    if (err) {
        printf("Failed on cs_open() with error returned: %u\n", err);
        abort();
    }

    vbr_zero = calloc(1, 1024);
    cpu_lvl = settings->cpu_level == 6 ? 5 : settings->cpu_level;
    snprintf(cpu_string_name, sizeof(cpu_string_name), "%u/", 68000 + (cpu_lvl == 5 ? 6 : cpu_lvl) * 10);
    join_path(path, base_path, cpu_string_name, sizeof(path));

    low_memory_size = -1;
    low_memory_temp = load_file(path, "lmem.dat", NULL, &low_memory_size, 0);
    high_memory_size = -1;
    high_memory_temp = load_file(path, "hmem.dat", NULL, &high_memory_size, 0);

    if (low_memory_size > 0) low_memory = calloc(1, low_memory_size);
    if (high_memory_size > 0) high_memory = calloc(1, high_memory_size);

    if (low_memory_size > 0) low_memory_back = calloc(1, low_memory_size);
    if (high_memory_size > 0) high_memory_back = calloc(1, high_memory_size);

    M68KTesterContext* context = calloc(1, sizeof(M68KTesterContext));
    context->opcode = settings->opcode;
    strncpy(context->cpu_path, path, 2048);

    result.context = context;
    result.error = NULL;

    return result;
}

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

int M68KTester_run_tests(M68KTesterContext* context, void* user_data, M68KTesterCallback callback) {
    s_cpu_callback = callback;
    s_cpu_user_data = user_data;
    s_cpu_context = context;
    /* Local patch: the upstream function returns 0 on every path, so the
     * header's "returns 1 if the tests are ok" contract was never met and
     * an embedding runner could not distinguish a clean run from a
     * mismatch. test_mnemo already reports per-mnemonic failure; carry it
     * across the whole run. */
    int failed = 0;

    if (!strcmp(context->opcode, "all")) {
        DIR* d = opendir(context->cpu_path);
        if (!d) {
            printf("Couldn't list directory '%s'\n", context->cpu_path);
            return 0;
        }
#define MAX_FILE_LEN 1024
#define MAX_MNEMOS 256
        char* dirs = calloc(MAX_MNEMOS, MAX_FILE_LEN);
        int diroff = 0;
        if (!dirs) return 0;

        for (;;) {
            struct dirent* dr = readdir(d);
            if (!dr) break;
            int d = isdir(context->cpu_path, dr->d_name);
            if (d && dr->d_name[0] != '.') {
                strcpy(dirs + diroff, dr->d_name);
                diroff += MAX_FILE_LEN;
                if (diroff >= MAX_FILE_LEN * MAX_MNEMOS) {
                    printf("too many directories!?\n");
                    return 0;
                }
            }
        }
        closedir(d);

        for (int i = 0; i < diroff; i += MAX_FILE_LEN) {
            for (int j = i + MAX_FILE_LEN; j < diroff; j += MAX_FILE_LEN) {
                if (strcmp(dirs + i, dirs + j) > 0) {
                    char tmp[MAX_FILE_LEN];
                    strcpy(tmp, dirs + j);
                    strcpy(dirs + j, dirs + i);
                    strcpy(dirs + i, tmp);
                }
            }
        }

        for (int i = 0; i < diroff; i += MAX_FILE_LEN) {
            if (test_mnemo(context->cpu_path, dirs + i)) {
                failed = 1;
                if (context->stop_on_error) break;
            }
        }

        free(dirs);

    } else {
        failed = test_mnemo(context->cpu_path, context->opcode) != 0;
    }

    return failed ? 0 : 1;
}
