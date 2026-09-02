//
// Copyright 2022-2023 Stefan Reinauer & Chris Hooper
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this
//    list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//

typedef uint64_t ULLONG;

typedef struct { /* RFC4122 */
	union {
		struct {
			ULONG time_low;
			UWORD time_mid;
			UWORD time_high_and_version;
			UBYTE clock_seq_high_and_reserved;
			UBYTE clock_seq_low;
			UBYTE node[6];
		} UUID;
		UBYTE raw[16];
	} u;
} __attribute__((packed)) GUID;

struct mbr_partition {
	UBYTE  status;
	UBYTE  f_head;
	UBYTE  f_sect;
	UBYTE  f_cyl;
	UBYTE  type;
	UBYTE  l_head;
	UBYTE  l_sect;
	UBYTE  l_cyl;
	ULONG  f_lba;
	ULONG  num_sect;
} __attribute__((packed));

struct mbr {
	UBYTE  bootcode[424];
	GUID   boot_guid;
	ULONG  disk_id;
	UBYTE  magic[2];
	struct mbr_partition part[4];
	UBYTE  sig[2];
} __attribute__((packed));

struct gpt {
	UBYTE  signature[8];
	ULONG  revision;
	ULONG  size;
	ULONG  header_crc32;
	ULONG  reserved_zero;
	ULLONG my_lba;
	ULLONG alternate_lba;
	ULLONG first_usable_lba;
	ULLONG last_usable_lba;
	GUID   disk_uuid;
	ULLONG entries_lba;
	ULONG  number_of_entries;
	ULONG  size_of_entry;
	ULONG  entries_crc32;
} __attribute__((packed));

struct gpt_partition {
	GUID partition_type;
	GUID unique_partition;
	ULLONG first_lba;
	ULLONG last_lba;
	ULLONG attribute_flags;
	USHORT partition_name[36];
} __attribute__((packed));
