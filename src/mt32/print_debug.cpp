// SPDX-License-Identifier: GPL-3.0-or-later
//
// The engine reports its diagnostics through a callback taking a C
// `va_list`. Rust has no stable type for one, and its shape differs by
// target -- a pointer on Windows and Apple platforms, an array that decays
// to one on x86-64 Linux, and a struct passed by value on AArch64 Linux --
// so a Rust declaration that guessed would be wrong somewhere.
//
// The formatting therefore happens here, where the compiler knows the ABI,
// and Rust is handed a finished string.

#include <cstdarg>
#include <cstdio>

extern "C" {

// Implemented in Rust: takes the formatted line.
void copperline_mt32_log(const char *text);

// Installed in the engine's report handler; see src/mt32/mod.rs.
void copperline_mt32_print_debug(void *instance_data, const char *fmt, va_list args) {
	(void) instance_data;
	char buf[512];
	int written = vsnprintf(buf, sizeof buf, fmt, args);
	if (written > 0) {
		copperline_mt32_log(buf);
	}
}

}
