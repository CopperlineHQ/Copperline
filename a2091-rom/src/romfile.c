//
// Copyright 2022-2025 Stefan Reinauer
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice,
//    this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//

#ifdef DEBUG_DEVICE
#define USE_SERIAL_OUTPUT
#endif
#include "port.h"
#include "printf.h"
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include "device.h"
#include "attach.h"
#include "reloc.h"
#include <resources/filesysres.h>
#include "version.h"
#include "romfile.h"

#define DOSTYPE_CD01 0x43443031
#define DOSTYPE_CDVD 0x43445644

static int add_fs_from_kickstart(void)
{
    struct Resident *r = NULL;

    printf("CDFS in Kickstart... ");
    r=FindResident("cdfs");
    printf("%sfound.\n", r?"":"not ");

    if (r != NULL) {
        if (r && r->rt_Init) {
            printf("Initializing CDFS @%p... ", r);
            InitResident(r, 0);
            printf("done.\n");
            return 1;
        } else
            printf("No rt_Init.\n");
    }
    return 0;
}

#if HAVE_ROM
typedef struct {
	uint32_t romfile[3], romfile_len[3], romfile_dostype[3];
} romfiles_t;

static uint32_t RomFetch32(uint32_t offset)
{
    uint8_t *rombase = (uint8_t *)asave->as_addr;
    uint32_t ret = 0;
    int i;
    for (i=0; i<16; i+=2) {
        ret <<=4;
        ret |= rombase[offset*4 +i] >>4;
    }
    return ret;
}

static void parse_romfiles(romfiles_t *rom)
{
    int i;

    /* If no end-of-rom signature is found below, all slots stay empty */
    memset(rom, 0, sizeof(*rom));

    for (i=1; i<=2; i++) {
        /* Look for end-of-rom signature */
        if (RomFetch32((i*32*1024)-8) == 0xffff5352 &&
                RomFetch32((i*32*1024)-4) == 0x2f434448) {

            rom->romfile_len[0]=RomFetch32((i*32*1024) - 12);
            if (rom->romfile_len[0])
                rom->romfile[0]=RomFetch32((i*32*1024) - 16);

            rom->romfile_len[1]=RomFetch32((i*32*1024) - 20);
            if (rom->romfile_len[1]) {
                rom->romfile[1]=RomFetch32((i*32*1024) - 24);
                rom->romfile_dostype[1]=RomFetch32((i*32*1024) - 28);
            }

            rom->romfile_len[2]=RomFetch32((i*32*1024) - 32);
            if (rom->romfile_len[2]) {
                rom->romfile[2]=RomFetch32((i*32*1024) - 36);
                rom->romfile_dostype[2]=RomFetch32((i*32*1024) - 40);
            }

            break;
        }
    }

    printf("Detected %dkB ROM.\n", i*32);
    if(rom->romfile_len[0]) {
        printf("  Driver @ 0x%05x (%d bytes)\n", rom->romfile[0], rom->romfile_len[0]);

        for (i=1; i<3; i++) {
            if(rom->romfile_len[i]) {
                printf("  FS %d   @ 0x%05x (%d bytes): %08x\n", i, rom->romfile[i],
                            rom->romfile_len[i], rom->romfile_dostype[i]);
                if (rom->romfile_dostype[i] == 0)
                    printf("            no DosType: FS can not be "
                           "demand-loaded (romtool -T missing?)\n");
                if (RomFetch32(rom->romfile[i]) == 0x524e4301) {
                    rom->romfile_len[i] = RomFetch32(rom->romfile[i] + 4);
                    printf("            compressed (%d bytes)\n",
                                    rom->romfile_len[i]);
                }
            } else
                printf("  FS %d not found.\n", i);
        }
    } else
        printf("  Driver not found. Huh?\n");
}

static int add_romfilesystem(romfiles_t *rom, int slot)
{
    uint32_t fs_seglist = 0;
    struct Resident *r = NULL;
    struct FileSysResource *FileSysResBase;
    struct FileSysEntry *fse = NULL;

    unsigned int i;
    printf("Looking for FS in A4091 ROM slot %d... ", slot);

    if (rom->romfile_len[slot])
        fs_seglist = relocate(rom->romfile[slot], (uint32_t)asave->as_addr);

    printf("%sfound.\n", fs_seglist?"":"not ");

    if (fs_seglist == 0)
        return 0;

    printf("Resident struct... ");
    for (i=fs_seglist; i<fs_seglist + rom->romfile_len[slot]; i+=2) {
        if(*(uint16_t *)i == 0x4afc) {
            r = (struct Resident *)i;
            break;
        }
    }

    printf("%sfound.\n", r?"":"not ");

    if (r != NULL) {
        if (r && r->rt_Init) {
            printf("Initializing FS @%p... ", r);
            InitResident(r, fs_seglist >> 2);
            printf("done.\n");
	    return 1;
        } else
            printf("No rt_Init.\n");
    }

    Forbid();
    FileSysResBase = (struct FileSysResource *)OpenResource(FSRNAME);

    for (fse = (struct FileSysEntry *)FileSysResBase->fsr_FileSysEntries.lh_Head;
	      fse->fse_Node.ln_Succ;
	      fse = (struct FileSysEntry *)fse->fse_Node.ln_Succ) {
	if (fse->fse_DosType == rom->romfile_dostype[slot]) {
		printf("DosType already present. Skipping.\n");
		FileSysResBase = NULL;
	}
    }

    if (FileSysResBase) {

        fse = AllocMem(sizeof(struct FileSysEntry), MEMF_PUBLIC | MEMF_CLEAR);
        if (fse) {
            fse->fse_Node.ln_Name = (UBYTE*)device_id_string;
            fse->fse_DosType = rom->romfile_dostype[slot];
            fse->fse_Version = ((LONG)DEVICE_VERSION) << 16 | DEVICE_REVISION;
            fse->fse_PatchFlags = 0x190; // StackSize, SegList and GlobalVec
            fse->fse_SegList = fs_seglist >> 2;
            fse->fse_GlobalVec = -1;
            //fse->fse_StackSize = 5120;
            fse->fse_StackSize = 16384; // Is there a right answer here?
            fse->fse_Priority = 10;

            AddHead(&FileSysResBase->fsr_FileSysEntries,&fse->fse_Node);
	}
    }
    Permit();
    return (fse!=NULL);
}
#endif

static int fs_is_registered(ULONG id1, ULONG id2)
{
	struct FileSysResource *FileSysResBase;
	struct FileSysEntry *fse;
	int found = 0;

	Forbid();
	FileSysResBase = (struct FileSysResource *)OpenResource(FSRNAME);
	if (FileSysResBase) {
		for (fse = (struct FileSysEntry *)
				FileSysResBase->fsr_FileSysEntries.lh_Head;
		     fse->fse_Node.ln_Succ;
		     fse = (struct FileSysEntry *)fse->fse_Node.ln_Succ) {
			if ((id1 && fse->fse_DosType == id1) ||
			    (id2 && fse->fse_DosType == id2)) {
				found = 1;
				break;
			}
		}
	}
	Permit();
	return found;
}

/*
 * LoadFileSys() is called by the mounter whenever it needs a
 * filesystem for one of the given DosTypes. If a matching
 * FileSysEntry is already registered in FileSystem.resource, it does
 * nothing. Otherwise, filesystems are loaded from the A4091 ROM (or
 * initialized from Kickstart) on demand, i.e. when a partition or
 * medium actually requires them: the mounter only asks for CD01/CDVD
 * when it finds a data CD (and CDROM boot is enabled), and only asks
 * for other DosTypes when it finds a partition of that type. Returns
 * nonzero if a filesystem was initialized; the caller scans
 * FileSystem.resource for the result, since a filesystem with a
 * romtag (e.g. ODFileSystem) registers its own FileSysEntry inside
 * InitResident().
 */
LONG LoadFileSys(ULONG id1, ULONG id2)
{
	static int kickstart_tried;
	LONG loaded = 0;

	/* Already available? Nothing to do. */
	if (fs_is_registered(id1, id2))
		return 0;

	printf("Mounter requests filesystem %08lx/%08lx\n", id1, id2);

	/* A Kickstart-resident CDFS can only serve CD DosTypes. Only try
	 * once: InitResident() must not run again on a later request. */
	if (!kickstart_tried &&
	    (id1 == DOSTYPE_CD01 || id1 == DOSTYPE_CDVD ||
	     id2 == DOSTYPE_CD01 || id2 == DOSTYPE_CDVD)) {
		kickstart_tried = 1;
		loaded |= add_fs_from_kickstart();
	}

#if HAVE_ROM
	static romfiles_t rom;
	static int rom_parsed;
	static int slot_tried[3];
	int slot;

	if (!rom_parsed) {
		parse_romfiles(&rom);
		rom_parsed = 1;
	}

	for (slot = 1; slot < 3; slot++) {
		if (slot_tried[slot] ||
		    rom.romfile_len[slot] == 0 ||
		    rom.romfile_dostype[slot] == 0)
			continue;
		if (rom.romfile_dostype[slot] == id1 ||
		    rom.romfile_dostype[slot] == id2) {
			slot_tried[slot] = 1;
			loaded |= add_romfilesystem(&rom, slot);
		}
	}
#endif
	return loaded;
}
