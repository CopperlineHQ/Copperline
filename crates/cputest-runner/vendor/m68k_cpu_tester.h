#pragma once

#include <stdint.h>
#include "cputest_defines.h"

#define M68K_INST_NAME_SIZE 16
#define M68K_CPU_TESTER

// 80-bit register type
typedef struct M68KTesterFpuReg {
    uint16_t exp;
    uint16_t dummy;
    uint32_t m[2];
} M68KTesterFpuReg;

// Holds all the registers
typedef struct M68KTesterRegisters {
    uint32_t regs[16];
    uint32_t ssp;
    uint32_t msp;
    uint32_t pc;
    uint32_t sr;
    uint32_t exc;
    uint32_t excframe;
    M68KTesterFpuReg fpuregs[8];
    uint32_t fpiar, fpcr, fpsr;
    uint32_t tracecnt;
    uint32_t tracedata[6];
    uint32_t cycles, cycles2, cyclest;
    uint32_t srcaddr, dstaddr, branchtarget;
    uint32_t endpc;
    uint8_t branchtarget_mode;
} M68KTesterRegisters;

// The tester will load data into different memory ranges so it's up to the implementor to make sure
// the implementation can read from all of these ranges
typedef struct M68KTesterMemoryRange {
    // Buffer in native memory (notice this doesn't have to be 32-bit in size so don't cast it)
    uint8_t* buffer;
    // Memory start location in 32-bit address space
    uint32_t start;
    // Memory end location in 32-bit address space
    uint32_t end;
    // Size of test memory
    uint32_t size;
} M68KTesterMemoryRange;

// Context for testing one instruction type
typedef struct M68KTesterContext {
    const char* opcode;
    // Low memory section
    uint32_t stop_on_error;
    // Low memory section
    M68KTesterMemoryRange low_memory;
    // High memory section
    M68KTesterMemoryRange high_memory;
    // Memory for the actual testing code
    M68KTesterMemoryRange test_memory;
    // name of the instruction
    char name[M68K_INST_NAME_SIZE + 1];
    // path for loading data files from
    char cpu_path[2048];
} M68KTesterContext;

// This is called for each test. user_data is pointer provided in M68KTester_run_tests which is passed down to the test
// context holds memory ranges needed for the callback to handle to read the memory correctly and registes holds the
// current state
typedef void (*M68KTesterCallback)(void* user_data, const M68KTesterContext* context, M68KTesterRegisters* regs);

// Settings for the runner
typedef struct M68KTesterRunSettings {
    // name of the opcode to test (needs to match the names in the data directory)
    // also "all" is supported to test all of them
    const char* opcode;
    // CPU level to use (0 = 0000, 1 = 010, 2 = 020, 3 = 030, 4 = 040, 5 & 6 = 060)
    uint8_t cpu_level;
    // Verify undefined status registers
    uint8_t check_undefined_sr;
    // continue on error
    uint8_t continue_on_error;
} M68KTesterRunSettings;

// This is returned from the M68KTester_init(). If the call failed context will be zero and the string set to
// an string describing the error. If ok context will have a valid pointer and error will be NULL.
typedef struct M68KTesterInitResult {
    // Contains data needed for running the tests
    M68KTesterContext* context;
    // Error string set to null terminated string if error, otherwise NULL
    // Notice this isn't currently used but will in the future.
    const char* error;
} M68KTesterInitResult;

// Init the tester
M68KTesterInitResult M68KTester_init(const char* path, const M68KTesterRunSettings* settings);

// Run the tests. Returns 1 of tests are ok otherwise 0
// This function will run the tests for a given instruction(s) and the callback will be called for each
// instruction test
//
// Notice this function isn't thread-safe.
//
int M68KTester_run_tests(M68KTesterContext* context, void* user_data, M68KTesterCallback callback);
