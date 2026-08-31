
// Generic autoboot/automount RDB parser and mounter.
// - KS 1.3 support, including autoboot mode.
// - 68000 compatible.
// - Boot ROM and executable modes.
// - Autoboot capable (Boot ROM mode only).
// - Full automount support
// - Full RDB filesystem support.
//
// Copyright 2021-2022 Toni Wilen
// Copyright 2022-2026 Stefan Reinauer
// Copyright 2023-2026 Matt Harlum
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
#ifdef DEBUG_MOUNTER
#define USE_SERIAL_OUTPUT
#endif
#if defined(A4091) || defined(DRIVER_A2091)
#include "port.h"
#endif

#include <exec/types.h>
#include <exec/memory.h>
#include <exec/alerts.h>
#include <exec/ports.h>
#include <exec/execbase.h>
#include <exec/io.h>
#include <exec/errors.h>
#include <devices/trackdisk.h>
#include <devices/hardblocks.h>
#include <devices/scsidisk.h>
#include <resources/filesysres.h>
#include <libraries/expansion.h>
#include <libraries/expansionbase.h>
#include <libraries/configvars.h>
#include <clib/alib_protos.h>
#include <dos/dos.h>
#include <dos/dosextens.h>
#include <dos/doshunks.h>

#include <string.h>
#include <stdio.h>
#include <stdbool.h>

#include <proto/exec.h>
#include <proto/expansion.h>
#include <proto/dos.h>

#include "ndkcompat.h"

#include "mounter.h"

#ifdef A4091
#include "scsimsg.h"
#include "device.h"
#include "a4091.h"
#include "attach.h"
#include "romfile.h"
#endif
#ifdef DISKLABELS
#include "legacy.h"
#endif

#ifndef SID_TYPE
#define SID_TYPE 0x1F
#endif

#ifndef HD_WIDESCSI
#define HD_WIDESCSI 8
#endif

#define TRACE 1
#undef TRACE_LSEG
#define Trace printf

#ifdef TRACE_LSEG
#define dbg_lseg printf
#else
#define dbg_lseg(x...) do { } while (0)
#endif

#if TRACE
#define dbg Trace
#else
#define dbg
#endif

#if !defined(A4091) && !defined(DRIVER_A2091)
#define printf(...)
#endif
#define MAX_BLOCKSIZE 2048
#define LSEG_DATASIZE (512 / 4 - 5)

#if NO_CONFIGDEV
extern UBYTE entrypoint, entrypoint_end;
extern UBYTE bootblock, bootblock_end;
#endif

struct MountData
{
	struct ExecBase *SysBase;
	struct ExpansionBase *ExpansionBase;
	struct DosLibrary *DOSBase;
	struct IOExtTD *request;
	struct ConfigDev *configDev;
	const UBYTE *creator;
	const UBYTE *devicename;

	ULONG lsegblock;
	ULONG lseglongs;
	ULONG lsegoffset;
	struct LoadSegBlock *lsegbuf;
	UWORD lsegwordbuf;
	UWORD lseghasword;

	ULONG unitnum;
	LONG ret;
	UBYTE buf[MAX_BLOCKSIZE * 3];
	UBYTE zero[2];
	BOOL wasLastDev;
	BOOL wasLastLun;
	BOOL slowSpinup;
	int blocksize;
};

#define SCSI_CD_MAX_TRACKS 100
#define SCSI_CMD_READ_TOC 0x43

struct __packed SCSI_TOC_TRACK_DESCRIPTOR {
    UBYTE reserved1;
    UBYTE adrControl;
    UBYTE trackNumber;
    UBYTE reserved2;
    UBYTE reserved3;
    UBYTE minute;
    UBYTE second;
    UBYTE frame;
};

struct __packed __attribute__((aligned(2))) SCSI_CD_TOC {
    UWORD length;
    UBYTE firstTrack;
    UBYTE lastTrack;
    struct SCSI_TOC_TRACK_DESCRIPTOR td[SCSI_CD_MAX_TRACKS];
};

#ifdef A4091
#define GetGeometry dev_scsi_get_drivegeometry
#else
// Get Block size of unit
BYTE GetGeometry(struct IOExtTD *req, struct DriveGeometry *geometry)
{
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Warray-bounds="
	struct ExecBase *SysBase = *(struct ExecBase **)4UL;
#pragma GCC diagnostic pop

	req->iotd_Req.io_Command = TD_GETGEOMETRY;
	req->iotd_Req.io_Data    = geometry;
	req->iotd_Req.io_Length  = sizeof(struct DriveGeometry);

	return DoIO((struct IORequest *)req);
}
#endif

static void W_NewList(struct List *new_list)
{
    new_list->lh_Head = (struct Node *)&new_list->lh_Tail;
    new_list->lh_Tail = 0;
    new_list->lh_TailPred = (struct Node *)new_list;
}

// KS 1.3 compatibility functions
APTR W_CreateIORequest(struct MsgPort *ioReplyPort, ULONG size, struct ExecBase *SysBase)
{
	struct IORequest *ret = NULL;
	if(ioReplyPort == NULL)
		return NULL;
	ret = (struct IORequest*)AllocMem(size, MEMF_PUBLIC | MEMF_CLEAR);
	if(ret != NULL)
	{
		ret->io_Message.mn_ReplyPort = ioReplyPort;
		ret->io_Message.mn_Length = size;
	}
	return ret;
}
void W_DeleteIORequest(APTR iorequest, struct ExecBase *SysBase)
{
	if(iorequest != NULL) {
		FreeMem(iorequest, ((struct Message*)iorequest)->mn_Length);
	}
}
struct MsgPort *W_CreateMsgPort(struct ExecBase *SysBase)
{
	struct MsgPort *ret;
	ret = (struct MsgPort*)AllocMem(sizeof(struct MsgPort), MEMF_PUBLIC | MEMF_CLEAR);
	if(ret != NULL)
	{
		BYTE sb = AllocSignal(-1);
		if (sb != -1)
		{
			ret->mp_Flags = PA_SIGNAL;
			ret->mp_Node.ln_Type = NT_MSGPORT;
			W_NewList(&ret->mp_MsgList);
			ret->mp_SigBit = sb;
			ret->mp_SigTask = FindTask(NULL);
			return ret;
		}
		FreeMem(ret, sizeof(struct MsgPort));
	}
	return NULL;
}
void W_DeleteMsgPort(struct MsgPort *port, struct ExecBase *SysBase)
{
	if(port != NULL)
	{
		FreeSignal(port->mp_SigBit);
		FreeMem(port, sizeof(struct MsgPort));
	}
}

// Flush cache (Filesystem relocation)
static void cacheclear(struct MountData *md)
{
	struct ExecBase *SysBase = md->SysBase;
	if (SysBase->LibNode.lib_Version >= 37) {
		CacheClearU();
	}
}

// Simply memory copy.
// Only used for few short copies, it does not need to be optimal.
// Required because compiler built-in memcpy() can have
// extra dependencies which will make boot rom build
// impossible.
static void copymem(void *dstp, void *srcp, UWORD size)
{
	UBYTE *dst = (UBYTE*)dstp;
	UBYTE *src = (UBYTE*)srcp;
	while (size != 0) {
		*dst++ = *src++;
		size--;
	}
}

// Check block checksum
static UWORD checksum(UBYTE *buf, struct MountData *md)
{
	ULONG chk = 0;
	ULONG num_longs;
	(void)md;

	num_longs = (buf[4] << 24) | (buf[5] << 16) | (buf[6] << 8) | (buf[7]);
	if (num_longs > 65535)
		return FALSE;

	for (UWORD i = 0; i < (int)(num_longs * sizeof(LONG)); i += 4) {
		ULONG v = (buf[i + 0] << 24) | (buf[i + 1] << 16) | (buf[i + 2] << 8) | (buf[i + 3 ] << 0);
		chk += v;
	}
	if (chk) {
		dbg("Checksum error %08"PRIx32"\n", chk);
		return FALSE;
	}
	return TRUE;
}


#define MAX_RETRIES 3

// Read single block with retries
static BOOL readblock(UBYTE *buf, ULONG block, ULONG id, struct MountData *md)
{
	struct ExecBase *SysBase = md->SysBase;
	struct IOExtTD *request = md->request;
	UWORD i, max_retries = MAX_RETRIES;
	if (md->slowSpinup)
		max_retries = 15;

	request->iotd_Req.io_Command = CMD_READ;
	request->iotd_Req.io_Offset = block * md->blocksize;
	request->iotd_Req.io_Data = buf;
	request->iotd_Req.io_Length = md->blocksize;
	for (i = 0; i < max_retries; i++) {
		LONG err = DoIO((struct IORequest*)request);
		if (!err) {
			break;
		}
#ifdef A4091
		if (err != ERROR_NOT_READY) {
			dbg("Read block %"PRIu32" error %"PRId32"\n", block, err);
			/* Error retry handled in a4091.device, fail quickly here. */
			i = max_retries;
			break;
		}
		/* Give the drive more time to spin up */
		dbg("Drive not ready.\n");
		delay(1000000);
#endif
	}
	if (i == max_retries) {
		return FALSE;
	}
	ULONG v = (buf[0] << 24) | (buf[1] << 16) | (buf[2] << 8) | (buf[3] << 0);
	dbg_lseg("Read block %"PRIu32" %08"PRIx32"\n", block, v);
	if (id != 0xffffffff) {
		if (v != id) {
			return FALSE;
		}
		if (!checksum(buf, md)) {
			return FALSE;
		}
	}
	return TRUE;
}

// Read multiple longs from LSEG blocks
static BOOL lseg_read_longs(struct MountData *md, ULONG longs, ULONG *data)
{
	dbg_lseg("lseg_read_longs, longs %"PRId32"  ptr %p, remaining %"PRId32"\n", longs, data, md->lseglongs);
	ULONG cnt = 0;
	md->lseghasword = FALSE;
	while (longs > cnt) {
		if (md->lseglongs > 0) {
			data[cnt] = md->lsegbuf->lsb_LoadData[md->lsegoffset];
			md->lsegoffset++;
			md->lseglongs--;
			cnt++;
			if (longs == cnt) {
				return TRUE;
			}
		}
		if (!md->lseglongs) {
			if (md->lsegblock == 0xffffffff) {
				dbg("lseg_read_long premature end!\n");
				return FALSE;
			}
			if (!readblock((UBYTE*)md->lsegbuf, md->lsegblock, IDNAME_LOADSEG, md)) {
				return FALSE;
			}
			md->lseglongs = LSEG_DATASIZE;
			md->lsegoffset = 0;
			dbg_lseg("lseg_read_long lseg block %"PRId32" loaded, next %"PRId32"\n", md->lsegblock, md->lsegbuf->lsb_Next);
			md->lsegblock = md->lsegbuf->lsb_Next;
		}
	}
	return TRUE;
}
// Read single long from LSEG blocks
static BOOL lseg_read_long(struct MountData *md, ULONG *data)
{
	BOOL v;
	if (md->lseghasword) {
		ULONG temp;
		v = lseg_read_longs(md, 1, &temp);
		*data = (md->lsegwordbuf << 16) | (temp >> 16);
		md->lsegwordbuf = (UWORD)temp;
		md->lseghasword = TRUE;
	} else {
		v = lseg_read_longs(md, 1, data);
	}
	dbg_lseg("lseg_read_long %08"PRIx32"\n", *data);
	return v;
}
// Read single word from LSEG blocks
// Internally reads long and buffers second word.
static BOOL lseg_read_word(struct MountData *md, ULONG *data)
{
	if (md->lseghasword) {
		*data = md->lsegwordbuf;
		md->lseghasword = FALSE;
		dbg("lseg_read_word 2/2 %08"PRIx32"\n", *data);
		return TRUE;
	}
	ULONG temp;
	BOOL v = lseg_read_longs(md, 1, &temp);
	if (v) {
		md->lseghasword = TRUE;
		md->lsegwordbuf = (UWORD)temp;
		*data = temp >> 16;
	}
	dbg("lseg_read_word 1/2 %08"PRIx32"\n", *data);
	return v;
}

struct RelocHunk
{
	ULONG hunkSize;
	ULONG *hunkData;
};

// Filesystem relocator
static APTR fsrelocate(struct MountData *md)
{
	struct ExecBase *SysBase = md->SysBase;
	ULONG data;
	struct RelocHunk *relocHunks;
	LONG firstHunk, lastHunk;
	ULONG totalHunks;
	UWORD hunkCnt;
	WORD ret = 0;
	APTR firstProcessedHunk = NULL;

	if (!lseg_read_long(md, &data)) {
		return NULL;
	}
	if (data != HUNK_HEADER) {
		return NULL;
	}
	// Read the size of a resident library name. This should
	// never be != 0.
	if (!lseg_read_long(md, &data) || data != 0) {
		return NULL;
	}
	// Read the size of the hunk table, which should be > 0.
	// Note that this number may be larger than the
	// difference between the last and the first hunk + 1 for
	// overlay binary files. But then this function does not
	// support overlay binary files.
	if (!lseg_read_long(md, &data) || data <= 0) {
		return NULL;
	}
	// first hunk
	if (!lseg_read_long(md, &firstHunk)) {
		return NULL;
	}
	// last hunk
	if (!lseg_read_long(md, &lastHunk)) {
		return NULL;
	}
	if (firstHunk < 0 || lastHunk < 0 || firstHunk > lastHunk) {
		return NULL;
	}
	totalHunks = lastHunk - firstHunk + 1;
	dbg("first hunk %"PRId32", last hunk %"PRId32"\n", firstHunk, lastHunk);
	relocHunks = AllocMem(totalHunks * sizeof(struct RelocHunk), MEMF_CLEAR);
	if (!relocHunks) {
		return NULL;
	}

	// Pre-allocate hunks
	ULONG *prevChunk = NULL;
	hunkCnt = 0;
	while (hunkCnt < totalHunks) {
		struct RelocHunk *rh = &relocHunks[hunkCnt];
		ULONG hunkHeadSize;
		ULONG memoryFlags = MEMF_PUBLIC;
		if (!lseg_read_long(md, &hunkHeadSize)) {
			goto end;
		}
		if ((hunkHeadSize & (HUNKF_CHIP | HUNKF_FAST)) == (HUNKF_CHIP | HUNKF_FAST)) {
			if (!lseg_read_long(md, &memoryFlags)) {
				goto end;
			}
		} else if (hunkHeadSize & HUNKF_CHIP) {
			memoryFlags |= MEMF_CHIP;
		}
		hunkHeadSize &= ~(HUNKF_CHIP | HUNKF_FAST);
		rh->hunkSize = hunkHeadSize;
		rh->hunkData = AllocMem((hunkHeadSize + 2) * sizeof(ULONG), memoryFlags | MEMF_CLEAR);
		if (!rh->hunkData) {
			goto end;
		}
		dbg("hunk %"PRId32": ptr %p, size %"PRId32", memory flags %08"PRIx32"\n", hunkCnt + firstHunk, rh->hunkData, hunkHeadSize, memoryFlags);
		rh->hunkData[0] = rh->hunkSize + 2;
		rh->hunkData[1] = MKBADDR(prevChunk);
		prevChunk = &rh->hunkData[1];
		rh->hunkData += 2;

		if (!firstProcessedHunk) {
			firstProcessedHunk = (APTR)(rh->hunkData - 1);
		}
		hunkCnt++;
	}
	dbg("hunks allocated\n");

	// Load hunks/relocate
	hunkCnt = 0;
	struct RelocHunk *rh = NULL;
	while (hunkCnt <= totalHunks) {
		ULONG hunkType;
		if (!lseg_read_long(md, &hunkType)) {
			if (hunkCnt >= totalHunks) {
				break;  // normal end
			}
			goto end;
		}
		dbg("HUNK %08"PRIx32"\n", hunkType);
		switch(hunkType)
		{
			case HUNK_CODE:
			case HUNK_DATA:
			case HUNK_BSS:
			{
				ULONG hunkSize;
				if (hunkCnt >= totalHunks) {
					goto end;  // overflow
				}
				rh = &relocHunks[hunkCnt++];
				if (!lseg_read_long(md, &hunkSize)) {
					goto end;
				}
				if (hunkSize > rh->hunkSize) {
					goto end;
				}
				if (hunkType != HUNK_BSS) {
					if (!lseg_read_longs(md, hunkSize, rh->hunkData)) {
						goto end;
					}
				}
			}
			break;
			case HUNK_RELOC32:
			case HUNK_RELOC32SHORT:
			{
				ULONG relocCnt, relocHunk;
				if (rh == NULL) {
					goto end;
				}
				for (;;) {
					if (!lseg_read_long(md, &relocCnt)) {
						goto end;
					}
					if (!relocCnt) {
						break;
					}
					if (!lseg_read_long(md, &relocHunk)) {
						goto end;
					}
					relocHunk -= firstHunk;
					if (relocHunk >= totalHunks) {
						goto end;
					}
					dbg("HUNK_RELOC32: relocs %"PRId32" hunk %"PRId32"\n", relocCnt, relocHunk + firstHunk);
					struct RelocHunk *rhr = &relocHunks[relocHunk];
					while (relocCnt != 0) {
						ULONG relocOffset;
						if (hunkType == HUNK_RELOC32SHORT) {
							if (!lseg_read_word(md, &relocOffset)) {
								goto end;
							}
						} else {
							if (!lseg_read_long(md, &relocOffset)) {
								goto end;
							}
						}
						if (relocOffset > (rh->hunkSize - 1) * sizeof(ULONG)) {
							goto end;
						}
						UBYTE *hData = (UBYTE*)rh->hunkData + relocOffset;
						if (relocOffset & 1) {
							// Odd address, 68000/010 support.
							ULONG v = (hData[0] << 24) | (hData[1] << 16) | (hData[2] << 8) | (hData[3] << 0);
							v += (ULONG)rhr->hunkData;
							hData[0] = v >> 24;
							hData[1] = v >> 16;
							hData[2] = v >>  8;
							hData[3] = v >>  0;
						} else {
							*((ULONG*)hData) += (ULONG)rhr->hunkData;
						}
						relocCnt--;
					}
				}
			}
			break;
			case HUNK_END:
			// do nothing
			if (hunkCnt >= totalHunks) {
				ret = 1;  // normal end
				goto end;
			}
			break;
			default:
			dbg("Unexpected HUNK!\n");
			goto end;
		}
	}
	ret = 1;

end:
	if (!ret) {
		dbg("reloc failed\n");
		hunkCnt = 0;
		while (hunkCnt < totalHunks) {
			struct RelocHunk *rh = &relocHunks[hunkCnt];
			if (rh->hunkData) {
				FreeMem(rh->hunkData - 2, (rh->hunkSize + 2) * sizeof(ULONG));
			}
			hunkCnt++;
		}
		firstProcessedHunk = NULL;
	} else {
		cacheclear(md);
		dbg("reloc ok, first hunk %p\n", firstProcessedHunk);
	}

	FreeMem(relocHunks, totalHunks * sizeof(struct RelocHunk));

	return firstProcessedHunk;
}

// Scan FileSystem.resource, create new if it is not found or existing entry has older version number.
static struct FileSysEntry *FSHDProcess(struct FileSysHeaderBlock *fshb, ULONG dostype, ULONG version, BOOL newOnly, struct MountData *md)
{
	struct ExecBase *SysBase = md->SysBase;
	struct FileSysEntry *result_fse = NULL;
	const UBYTE *creator = md->creator ? md->creator : md->zero;
	const char resourceName[] = "FileSystem.resource";

	Forbid();
	struct FileSysResource *fsr = OpenResource(FSRNAME);
	if (!fsr) {
		// FileSystem.resource didn't exist (KS 1.3), create it.
		fsr = AllocMem(sizeof(struct FileSysResource) + strlen(resourceName) + 1 + strlen((const char *)creator) + 1, MEMF_PUBLIC | MEMF_CLEAR);
		if (fsr) {
			char *FsResName  = (char *)(fsr + 1);
			char *CreatorStr = (char *)FsResName + (strlen(resourceName) + 1);
			W_NewList(&fsr->fsr_FileSysEntries);
			fsr->fsr_Node.ln_Type = NT_RESOURCE;
			strcpy(FsResName, resourceName);
			fsr->fsr_Node.ln_Name = FsResName;
			strcpy(CreatorStr, (const char *)creator);
			fsr->fsr_Creator = CreatorStr;
			AddTail(&SysBase->ResourceList, &fsr->fsr_Node);
		}
		dbg("FileSystem.resource created %p\n", fsr);
	}

	if (fsr) {
		struct Node *node;
		struct FileSysEntry *found_existing_fse = NULL;

		// Correctly iterate through the list to find if an entry for 'dostype' already exists
		for (node = fsr->fsr_FileSysEntries.lh_Head;
			 node->ln_Succ != NULL; // Standard AmigaOS list traversal: loop while node is not the tail sentinel
			 node = node->ln_Succ) {
			struct FileSysEntry *current_entry = (struct FileSysEntry *)node;
			if (current_entry->fse_DosType == dostype) {
				found_existing_fse = current_entry; // Found a match by DosType
				break; // Process this first match
			}
		}

		if (found_existing_fse) {
			// An entry with the same DosType was found
			if (found_existing_fse->fse_Version >= version) {
				if (newOnly) {
					// Existing entry is suitable, and we only want to add a new one if necessary.
					dbg("FileSystem.resource scan: Existing up-to-date entry 0x%p for 0x%08X found. Version 0x%08X >= requested 0x%08X. No action needed.\n",
						found_existing_fse, dostype, found_existing_fse->fse_Version, version);
					Permit();
					return NULL; // Indicate no new/updated fse needed from this call
				} else {
					// newOnly is false. We found an existing entry.
					result_fse = found_existing_fse;
				}
			}
		}
		// If found_existing_fse is NULL, no entry for this dostype was found.

		// If fshb is provided (i.e., we have a FileSystem definition from RDB/disk)
		// AND newOnly is true (caller wants to add this if it's new or an upgrade)
		// AND we haven't already decided to return an existing (up-to-date, !newOnly) entry:
		if (fshb && newOnly) {
			if (!(found_existing_fse && found_existing_fse->fse_Version >= version)) {
				// Either no existing FSE for this DosType, or existing one is older.
				// So, we create a new one based on fshb.
				result_fse = AllocMem(sizeof(struct FileSysEntry) + strlen((const char *)creator) + 1, MEMF_PUBLIC | MEMF_CLEAR);
				if (result_fse) {
					ULONG patchFlags = fshb->fhb_PatchFlags;
					if (patchFlags & 0x0001)
						result_fse->fse_Type = fshb->fhb_Type;
					if (patchFlags & 0x0002)
						result_fse->fse_Task = fshb->fhb_Task;
					if (patchFlags & 0x0004)
						result_fse->fse_Lock = fshb->fhb_Lock;
					if (patchFlags & 0x0008)
						result_fse->fse_Handler = fshb->fhb_Handler;
					if (patchFlags & 0x0010)
						result_fse->fse_StackSize = fshb->fhb_StackSize;
					if (patchFlags & 0x0020)
						result_fse->fse_Priority = fshb->fhb_Priority;
					if (patchFlags & 0x0040)
						result_fse->fse_Startup = fshb->fhb_Startup;
					if (patchFlags & 0x0080)
						result_fse->fse_SegList = fshb->fhb_SegListBlocks;
					if (patchFlags & 0x0100)
						result_fse->fse_GlobalVec = fshb->fhb_GlobalVec;
					result_fse->fse_DosType = fshb->fhb_DosType;
					result_fse->fse_Version = fshb->fhb_Version;
					result_fse->fse_PatchFlags = fshb->fhb_PatchFlags;
					strcpy((char *)(result_fse + 1), (const char *)creator);
					result_fse->fse_Node.ln_Name = (UBYTE *)(result_fse + 1);
					dbg("FileSystem.resource scan: new FileSysEntry 0x%p created for 0x%08X based on fshb.\n", result_fse, dostype);
				}
			}
		} else if (fshb && !newOnly && found_existing_fse) {
			result_fse = found_existing_fse;
		}
	}
	Permit();
	return result_fse;
}

// Add new FileSysEntry to FileSystem.resource or free it if filesystem load failed.
static void FSHDAdd(struct FileSysEntry *fse, struct MountData *md)
{
	struct ExecBase *SysBase = md->SysBase;
	if (fse->fse_SegList) {
		Forbid();
		struct FileSysResource *fsr = OpenResource(FSRNAME);
		if (fsr) {
			AddHead(&fsr->fsr_FileSysEntries, &fse->fse_Node);
			dbg("FileSysEntry %p added to FileSystem.resource, dostype %08"PRIx32"\n", fse, fse->fse_DosType);
			fse = NULL;
		}
		Permit();
	}
	if (fse) {
		dbg("FileSysEntry %p freed, dostype %08"PRIx32"\n", fse, fse->fse_DosType);
		FreeMem(fse, sizeof(struct FileSysEntry));
	}
}

// Parse FileSystem Header Blocks, load and relocate filesystem if needed.
static struct FileSysEntry *ParseFSHD(UBYTE *buf, ULONG block, ULONG dostype, struct MountData *md)
{
	struct FileSysHeaderBlock *fshb = (struct FileSysHeaderBlock*)buf;
	struct FileSysEntry *fse = NULL;

	for (;;) {
		if (block == 0xffffffff) {
			break;
		}
		if (!readblock(buf, block, IDNAME_FILESYSHEADER, md)) {
			break;
		}
		dbg("FSHD found, block %"PRIu32", dostype %08"PRIx32", looking for dostype %08"PRIx32"\n", block, fshb->fhb_DosType, dostype);
		if (fshb->fhb_DosType == dostype) {
			dbg("FSHD dostype match found\n");
			fse = FSHDProcess(fshb, dostype, fshb->fhb_Version, TRUE, md);
			if (fse) {
				md->lsegblock = fshb->fhb_SegListBlocks;
				md->lsegbuf = (struct LoadSegBlock*)(buf + md->blocksize);
				md->lseglongs = 0;
				APTR seg = fsrelocate(md);
				fse->fse_SegList = MKBADDR(seg);
				// Add to FileSystem.resource if succeeded, delete entry if failure.
				FSHDAdd(fse, md);
			}
			break;
		}
		block = fshb->fhb_Next;
	}
	if (!fse) {
#ifdef A4091
		/* No usable filesystem came from the RDB. Try ROM or
		 * Kickstart before scanning FileSystem.resource. */
		LoadFileSys(dostype, 0);
#endif
		fse = FSHDProcess(NULL, dostype, 0, FALSE, md);
	}
	return fse;
}

#if NO_CONFIGDEV
// Create fake ConfigDev and DiagArea to support autoboot without requiring real autoconfig device.
static void CreateFakeConfigDev(struct MountData *md)
{
	struct ExecBase *SysBase = md->SysBase;
	struct ExpansionBase *ExpansionBase = md->ExpansionBase;
	struct ConfigDev *configDev;

	configDev = AllocConfigDev();
	if (configDev) {
		configDev->cd_BoardAddr = (void*)&entrypoint;
		configDev->cd_BoardSize = (UBYTE*)&entrypoint_end - (UBYTE*)&entrypoint;
		configDev->cd_Rom.er_Type = ERTF_DIAGVALID;
		ULONG bbSize = &bootblock_end - &bootblock;
		ULONG daSize = sizeof(struct DiagArea) + bbSize;
		struct DiagArea *diagArea = AllocMem(daSize, MEMF_CLEAR | MEMF_PUBLIC);
		if (diagArea) {
			diagArea->da_Config = DAC_CONFIGTIME;
			diagArea->da_BootPoint = sizeof(struct DiagArea);
			diagArea->da_Size = (UWORD)daSize;
			copymem(diagArea + 1, &bootblock, bbSize);
			memcpy(&configDev->cd_Rom.er_Reserved0c, &diagArea, sizeof(ULONG));
			cacheclear(md);
		}
		md->configDev = configDev;
	}
}
#endif

struct ParameterPacket
{
	const UBYTE *dosname;
	const UBYTE *execname;
	ULONG unitnum;
	ULONG flags;
	struct DosEnvec de;
};

static UBYTE ToUpper(UBYTE c)
{
	if (c >= 'a' && c <= 'z') {
		return c - ('a'-'A');
	}
	return c;
}

// Case-insensitive BSTR string comparison
static BOOL CompareBSTRNoCase(const UBYTE *src1, const UBYTE *src2)
{
	UBYTE len1 = *src1++;
	UBYTE len2 = *src2++;
	if (len1 != len2) {
		return FALSE;
	}
	for (UWORD i = 0; i < len1; i++) {
		UBYTE c1 = *src1++;
		UBYTE c2 = *src2++;
		c1 = ToUpper(c1);
		c2 = ToUpper(c2);
		if (c1 != c2) {
			return FALSE;
		}
	}
	return TRUE;
}

// Check for duplicate device names
static bool CheckDevName(struct MountData *md, UBYTE *bname)
{
	struct ExecBase *SysBase = md->SysBase;
	bool found = false;

	Forbid();
	struct BootNode *bn;
	for (bn = (struct BootNode*)md->ExpansionBase->MountList.lh_Head;
		 bn->bn_Node.ln_Succ != NULL;
		 bn = (struct BootNode*)bn->bn_Node.ln_Succ)
	{
		struct DeviceNode *dn = bn->bn_DeviceNode;
		const UBYTE *bname2 = BADDR(dn->dn_Name);
		if (CompareBSTRNoCase(bname, bname2)) {
			found = true;
		}
	}

	Permit();
	return found;
}

// Check for duplicate device names
static void CheckAndFixDevName(struct MountData *md, UBYTE *bname)
{
	struct ExecBase *SysBase = md->SysBase;

	Forbid();
	struct BootNode *bn = (struct BootNode*)md->ExpansionBase->MountList.lh_Head;
	while (bn->bn_Node.ln_Succ) {
		struct DeviceNode *dn = bn->bn_DeviceNode;
		const UBYTE *bname2 = BADDR(dn->dn_Name);
		if (CompareBSTRNoCase(bname, bname2)) {
			UBYTE len = bname[0] > 30 ? 30 : bname[0];
			UBYTE *name = bname + 1;
			dbg("Duplicate device name '%s'\n", name);
			if (len > 2 && name[len - 2] == '.' && name[len - 1] >= '0' && name[len - 1] < '9') {
				// if already ends to .<digit>: increase digit by one
				name[len - 1]++;
			} else {
				// else append .1
				name[len++] = '.';
				name[len++] = '1';
				name[len] = 0;
				bname[0] += 2;
			}
			dbg("-> new device name '%s'\n", name);
			// retry
			bn = (struct BootNode*)md->ExpansionBase->MountList.lh_Head;
			continue;
		}
		bn = (struct BootNode*)bn->bn_Node.ln_Succ;
	}
	Permit();
}

// Add DeviceNode to Expansion MountList.
static void AddNode(struct PartitionBlock *part, struct ParameterPacket *pp, struct DeviceNode *dn, UBYTE *name, struct MountData *md)
{
	struct ExecBase *SysBase = md->SysBase;
	struct ExpansionBase *ExpansionBase = md->ExpansionBase;
	struct DosLibrary *DOSBase = md->DOSBase;

	LONG bootPri = (part->pb_Flags & PBFF_BOOTABLE) ? pp->de.de_BootPri : -128;
	if (ExpansionBase->LibNode.lib_Version >= 37) {
		// KS 2.0+
		if (!md->DOSBase && bootPri > -128) {
			dbg("KS20+ Mounting as bootable: pri %08"PRIx32"\n", bootPri);
			AddBootNode(bootPri, ADNF_STARTPROC, dn, md->configDev);
		} else {
			dbg("KS20+: Mounting as non-bootable\n");
			AddDosNode(bootPri, ADNF_STARTPROC, dn);
		}
	} else {
		// KS 1.3
		if (!md->DOSBase && bootPri > -128) {
			dbg("KS13 Mounting as bootable: pri %08"PRIx32"\n", bootPri);
			// Create and insert bootnode manually.
			struct BootNode *bn = AllocMem(sizeof(struct BootNode), MEMF_CLEAR | MEMF_PUBLIC);
			if (bn) {
				bn->bn_Node.ln_Type = NT_BOOTNODE;
				bn->bn_Node.ln_Pri = (BYTE)bootPri;
				bn->bn_Node.ln_Name = (UBYTE*)md->configDev;
				bn->bn_DeviceNode = dn;
				Forbid();
				Enqueue(&md->ExpansionBase->MountList, &bn->bn_Node);
				Permit();
			}
		} else {
			dbg("KS13: Mounting as non-bootable\n");
			AddDosNode(bootPri, 0, dn);
			if (md->DOSBase) {
				// KS 1.3 ADNF_STARTPROC is not supported
				// need to use DeviceProc() to start the filesystem process.
				UWORD len = strlen(name);
				name[len++] = ':';
				name[len] = 0;
				void * __attribute__((unused)) mp = DeviceProc(name);
				dbg("DeviceProc() returned %p\n", mp);
			}
		}
	}
}

static void ProcessPatchFlags(struct DeviceNode *dn, struct FileSysEntry *fse)
{
	// Process PatchFlags.
	ULONG patchFlags = fse->fse_PatchFlags;
	if (patchFlags & 0x0001)
		dn->dn_Type = fse->fse_Type;
	if (patchFlags & 0x0002)
		dn->dn_Task = (struct MsgPort *)fse->fse_Task;
	if (patchFlags & 0x0004)
		dn->dn_Lock = fse->fse_Lock;
	if (patchFlags & 0x0008)
		dn->dn_Handler = fse->fse_Handler;
	if (patchFlags & 0x0010)
		dn->dn_StackSize = fse->fse_StackSize;
	if (patchFlags & 0x0020)
		dn->dn_Priority = fse->fse_Priority;
	if (patchFlags & 0x0040)
		dn->dn_Startup = fse->fse_Startup;
	if (patchFlags & 0x0080)
		dn->dn_SegList = fse->fse_SegList;
	if (patchFlags & 0x0100)
		dn->dn_GlobalVec = fse->fse_GlobalVec;
}

// Parse PART block, mount drive.
static ULONG ParsePART(UBYTE *buf, ULONG block, ULONG filesysblock, struct MountData *md)
{
	struct ExecBase *SysBase = md->SysBase;
	struct ExpansionBase *ExpansionBase = md->ExpansionBase;
	struct PartitionBlock *part = (struct PartitionBlock*)buf;
	ULONG nextpartblock = 0xffffffff;

	if (!readblock(buf, block, IDNAME_PARTITION, md)) {
		return nextpartblock;
	}
	dbg("PART found, block %"PRIu32"\n", block);
	nextpartblock = part->pb_Next;
	if (!(part->pb_Flags & PBFF_NOMOUNT)) {
		struct ParameterPacket *pp = AllocMem(sizeof(struct ParameterPacket), MEMF_PUBLIC | MEMF_CLEAR);
		if (pp) {
			UBYTE len;
			copymem(&pp->de, &part->pb_Environment, (part->pb_Environment[0] + 1) * sizeof(ULONG));
			struct FileSysEntry *fse = ParseFSHD(buf + md->blocksize, filesysblock, pp->de.de_DosType, md);
			pp->execname = md->devicename;
			pp->unitnum = md->unitnum;
			pp->dosname = part->pb_DriveName + 1;
			len=(*part->pb_DriveName) > 30 ? 30 : (*part->pb_DriveName);
			part->pb_DriveName[len + 1] = 0;
			dbg("PART '%s'\n", pp->dosname);
			CheckAndFixDevName(md, part->pb_DriveName);
			struct DeviceNode *dn = MakeDosNode(pp);
			if (dn) {
				if (fse) {
					ProcessPatchFlags(dn, fse);
				}
				dbg("Mounting partition\n");
#if NO_CONFIGDEV
				if (!md->configDev && !md->DOSBase) {
					CreateFakeConfigDev(md);
				}
#endif
				AddNode(part, pp, dn, part->pb_DriveName + 1, md);
				md->ret++;
			} else {
				dbg("Device node creation failed\n");
			}
			FreeMem(pp, sizeof(struct ParameterPacket));
		}
	}
	return nextpartblock;
}

// Scan PART blocks
static LONG ParseRDSK(UBYTE *buf, struct MountData *md)
{
	struct RigidDiskBlock *rdb = (struct RigidDiskBlock*)buf;
	ULONG partblock = rdb->rdb_PartitionList;
	ULONG filesysblock = rdb->rdb_FileSysHeaderList;
	ULONG flags = rdb->rdb_Flags;
	for (;;) {
		if (partblock == 0xffffffff) {
			break;
		}
		partblock = ParsePART(buf, partblock, filesysblock, md);
	}

	md->wasLastDev = (flags & RDBFF_LAST) != 0;
	md->wasLastLun = (flags & RDBFF_LASTLUN) != 0;

	return md->ret;
}

// Search for RDB
static LONG ScanRDSK(struct MountData *md)
{
	LONG ret = -1;
	for (UWORD i = 0; i < RDB_LOCATION_LIMIT; i++) {
		if (readblock(md->buf, i, 0xffffffff, md)) {
			struct RigidDiskBlock *rdb = (struct RigidDiskBlock*)md->buf;
			if (rdb->rdb_ID == IDNAME_RIGIDDISK) {
				dbg("RDB found, block %"PRIu32"\n", i);
				ret = ParseRDSK(md->buf, md);
				break;
			}
		}
	}
	return ret;
}

static struct FileSysEntry *find_filesystem(ULONG id1, ULONG id2, struct ExecBase *SysBase)
{
	struct FileSysResource *FileSysResBase = NULL;
	struct FileSysEntry *fse, *fs=NULL;
#ifdef A4091
	/* Try ROM or Kickstart before scanning FileSystem.resource.
	 * Run outside Forbid() because loading may allocate memory. */
	LoadFileSys(id1, id2);
#endif
	Forbid();
	if ((FileSysResBase = (struct FileSysResource *)OpenResource(FSRNAME))) {
		for (fse = (struct FileSysEntry *)FileSysResBase->fsr_FileSysEntries.lh_Head;
			  fse->fse_Node.ln_Succ;
			  fse = (struct FileSysEntry *)fse->fse_Node.ln_Succ) {
			if ((id1 && fse->fse_DosType==id1) || (id2 && fse->fse_DosType==id2)) {
				fs=fse;
				break;
			}
		}
	}
	Permit();
	return fs;
}

// Check if there is a disc inserted
static bool UnitIsReady(struct IOStdReq *req)
{
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Warray-bounds="
	struct ExecBase *SysBase = *(struct ExecBase **)4UL;
#pragma GCC diagnostic pop

	BYTE err;

	// First spin up the disc
	// Not critical if there's an error so no need to check
	req->io_Command = CMD_START;
	req->io_Error   = 0;
	DoIO((struct IORequest *)req);

	req->io_Command = TD_CHANGESTATE;
	req->io_Actual  = 0;
	req->io_Error   = 0;
	err = DoIO((struct IORequest *)req);

	// Some devices/units don't support this - assume that it is ready
	if (err == IOERR_NOCMD) return true;

	if (err == 0 && req->io_Actual == 0) return true;

	return false;
}


// Check if this is a data disc by reading the TOC and checking that track 1 is a data track.
static bool isDataCD(struct IOStdReq *ior)
{
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Warray-bounds="
	struct ExecBase *SysBase = *(struct ExecBase **)4UL;
#pragma GCC diagnostic pop
	bool ret = false;

	BYTE err;

	struct SCSICmd     *scsiCmd = NULL;
	struct SCSI_CD_TOC *tocBuf  = NULL;

	ULONG bufSize = sizeof(struct SCSI_CD_TOC);

	char cdb[10];
	memset(&cdb,0,10);

	if ((scsiCmd = AllocMem(sizeof(struct SCSICmd),MEMF_PUBLIC | MEMF_CLEAR))) {
		if ((tocBuf = AllocMem(bufSize,MEMF_PUBLIC | MEMF_CLEAR))) {
			scsiCmd->scsi_Data      = (UWORD *)tocBuf;
			scsiCmd->scsi_Length    = bufSize;
			scsiCmd->scsi_Flags     = SCSIF_READ;
			scsiCmd->scsi_CmdLength = 10;
			scsiCmd->scsi_Command   = cdb;

			cdb[0] = SCSI_CMD_READ_TOC;
			cdb[2] = 0;                  // Format: 0
			cdb[6] = 1;                  // Track 1
			cdb[7] = bufSize >> 8;
			cdb[8] = bufSize & 0xFF;

			ior->io_Data    = scsiCmd;
			ior->io_Length  = sizeof(struct SCSICmd);
			ior->io_Command = HD_SCSICMD;

			for (int retry = 0; retry < 3; retry++) {
				if ((err = DoIO((struct IORequest *)ior)) == 0 && scsiCmd->scsi_Status == 0)
					break;
			}

			if (err == 0) {
				if (tocBuf->firstTrack == 1 && tocBuf->td[0].trackNumber == 1) {
					if (tocBuf->td[0].adrControl & 0x04) {	// Data Track?
						ret = true;
					}
				}
			}

			FreeMem(tocBuf,bufSize);
		}
		FreeMem(scsiCmd,sizeof(struct SCSICmd));
	}
	return ret;
}

// CheckPVD
// Check for "CDTV" or "AMIGA BOOT" as the System ID in the PVD
// Returns: -1 on error, 0 if not CDTV/AMIGA BOOT, 1 if bootable
static LONG CheckPVD(struct IOStdReq *ior, struct ExecBase *SysBase)
{
	const char sys_id_1[] = "CDTV";
	const char sys_id_2[] = "AMIGA BOOT";
	const char iso_id[]   = "CD001";

	BYTE err = 0;
	LONG ret = -1;
	char *buf = NULL;

	if (!(buf = AllocMem(2048,MEMF_ANY|MEMF_CLEAR))) goto done;

	char *id_string = buf + 1;
	char *system_id = buf + 8;

	ior->io_Command = CMD_READ;
	ior->io_Data    = buf;
	ior->io_Length  = 2048;
	ior->io_Offset  = 32768; // Sector 16

	for (int retry = 0; retry < 3; retry++) {
		if ((err = DoIO((struct IORequest*)ior)) == 0) break;
	}

	if (err == 0) {
		// Check ISO ID String & for PVD Version & Type code
		if ((strncmp(iso_id,id_string,5) == 0) && buf[0] == 1 && buf[6] == 1) {
			ret = (strncmp(sys_id_1,system_id,strlen(sys_id_1)) == 0 || strncmp(sys_id_2,system_id,strlen(sys_id_2)) == 0);
		}
	}

done:
	if (buf)  FreeMem(buf,2048);
	return ret;
}

// Search for Bootable CDROM
static LONG ScanCDROM(struct MountData *md)
{
	struct ExecBase *SysBase = md->SysBase;
	struct ExpansionBase *ExpansionBase = md->ExpansionBase;
	struct FileSysEntry *fse=NULL;
	char dosName[] = "\3CD0"; // BCPL string
	LONG bootPri;
	LONG isBootable;

	if (!UnitIsReady((struct IOStdReq *)md->request))
		return -1;

	if (!isDataCD((struct IOStdReq *)md->request))
		return -1;

	// "CDTV" or "AMIGA BOOT"?
	isBootable = CheckPVD((struct IOStdReq *)md->request,SysBase);

	if (isBootable == -1) {
		// ISO PVD Not found, RDB CD?
		return ScanRDSK(md);
	} else {
		if (isBootable) {
			bootPri = 2; // Yes, give priority
		} else {
			bootPri = -1; // May not be a boot disk, lower priority than HDD
		}
	}

	fse=find_filesystem(0x43443031, 0x43445644, md->SysBase);
	if (!fse) {
		printf("Could not load filesystem\n");
		return -1;
	}

	struct ParameterPacket pp;

	memset(&pp,0,sizeof(struct ParameterPacket));

	pp.dosname              = dosName + 1;
	pp.execname             = md->devicename;
	pp.unitnum              = md->unitnum;
	pp.de.de_TableSize      = 16; // Up to DE_DOSTYPE
	pp.de.de_SizeBlock      = 2048 >> 2;
	pp.de.de_Surfaces       = 1;
	pp.de.de_SectorPerBlock = 1;
	pp.de.de_BlocksPerTrack = 1;
	pp.de.de_NumBuffers     = 5;
	pp.de.de_BufMemType     = MEMF_ANY|MEMF_CLEAR;
	pp.de.de_MaxTransfer    = 0x100000;
	pp.de.de_Mask           = 0x7FFFFFFE;
	pp.de.de_DosType        = fse->fse_DosType; // CD01 / CDVD
	pp.de.de_BootPri        = bootPri;

	for (int i=0; i<9; i++) {
		if (CheckDevName(md,dosName)) {
			dosName[3] += 1;
		} else {
			break;
		}
	}

	struct DeviceNode *node = MakeDosNode(&pp);
	if (!node) {
		printf("Could not create DosNode\n");
		return -1;
	}

	ProcessPatchFlags(node, fse);

	AddBootNode(bootPri, ADNF_STARTPROC, node, md->configDev);

	return 1;
}

#ifdef DISKLABELS

static void
lba2chs(ULONG start, ULONG end, ULONG max_lba, ULONG *cs_p, ULONG *ce_p, ULONG *h_p, ULONG *s_p)
{
    ULONG cs = start >> 1;
    ULONG ce = end >> 1;
    ULONG cm = max_lba >> 1;
    ULONG h = 2;
    ULONG s = 1;
    while ((cm >= 10000) && (s < 32)) {
        cm >>= 2;
        cs >>= 2;
        ce >>= 2;
        h <<= 1;
        s <<= 1;
    }
    *cs_p = cs;
    *ce_p = ce;
    *h_p = h;
    *s_p = s;
}

static LONG register_legacy(struct MountData *md, UBYTE bootable, UBYTE type, ULONG pstart, ULONG plen, ULONG max_lba)
{
	struct FileSysEntry *fse=NULL;
	char dosName[] = "MS0";
	static unsigned int cnt = 0;
	LONG bootPri;
	ULONG pend = pstart + plen - 1;
	ULONG cs,ce,h,s;

	if (cnt > 9) {
		printf("Error: Too many partitions, skipping MS%d\n", cnt);
		cnt++;
		return -1;
	}

	if (type != 0x00 && type != 0x01 && type != 0x04 &&
	    type != 0x06 && type != 0x0B && type != 0x0C && type != 0x0E) {
		printf("Warning: Partition type 0x%02x may not be FAT\n", type);
	}

	bootPri = bootable ? 0 : -1;

	lba2chs(pstart,pend, max_lba, &cs, &ce, & h, &s);

	printf("register_legacy: %d - %d  (%d/%d/%d - %d/%d/%d)\n",
			pstart, pend, cs,h,s,ce,h,s);

	fse=find_filesystem(0x46415401, 0, md->SysBase);
	if (!fse) {
		printf("Could not load filesystem\n");
		return -1;
	}

	struct ParameterPacket pp;

	memset(&pp,0,sizeof(struct ParameterPacket));

	pp.dosname              = dosName;
	pp.execname             = md->devicename;
	pp.unitnum              = md->unitnum;
	pp.de.de_TableSize      = sizeof(struct DosEnvec);
	pp.de.de_SizeBlock      = 512 >> 2;
	pp.de.de_Surfaces       = h;
	pp.de.de_SectorPerBlock = 1;
	pp.de.de_BlocksPerTrack = s;
	pp.de.de_LowCyl         = cs;
	pp.de.de_HighCyl        = ce;
	pp.de.de_NumBuffers     = 5;
	pp.de.de_BufMemType     = MEMF_ANY|MEMF_CLEAR;
	pp.de.de_MaxTransfer    = 0x100000;
	pp.de.de_Mask           = 0x7FFFFFFE;
	pp.de.de_DosType        = 0x46415401; // FAT95 for now
	pp.de.de_BootPri        = bootPri;

	dosName[2]='0' + cnt;
	struct DeviceNode *node = MakeDosNode(&pp);
	if (!node) {
		printf("Could not create DosNode\n");
		return -1;
	}

	ProcessPatchFlags(node, fse);

	AddBootNode(bootPri, ADNF_STARTPROC, node, md->configDev);
	cnt++;

	return 1;
}

#define MAX_EXTENDED_PARTITIONS 16

unsigned long parse_extended(struct MountData *md, int extended, unsigned long start, unsigned long max_lba)
{
	struct mbr *mbr = (struct mbr *)md->buf;
	unsigned long new_start;

	if (extended > 4 + MAX_EXTENDED_PARTITIONS) {
		printf("Warning: Extended partition limit (%d) reached\n",
		       MAX_EXTENDED_PARTITIONS);
		return 0;
	}

	printf("   %2d   ", extended++);
	printf("%c   %02x %8lx %8lx\n", mbr->part[0].status & 0x80 ? '*':' ',
			mbr->part[0].type,
			start + __bswap32(mbr->part[0].f_lba),
			(unsigned long)__bswap32(mbr->part[0].num_sect));

	register_legacy(md, mbr->part[0].status & 0x80, mbr->part[0].type,
			start + __bswap32(mbr->part[0].f_lba),
			__bswap32(mbr->part[0].num_sect), max_lba);

	new_start = __bswap32(mbr->part[1].f_lba);

	if (mbr->part[1].type == 5) {
		readblock(md->buf, start + new_start, 0xffffffff, md);
		parse_extended(md, extended, start + new_start, max_lba);
	}

	return 0;
}

static LONG ParseMBR(UBYTE *buf, struct MountData *md)
{
	int extended = 5;
	ULONG max_lba = 0;

	struct mbr *mbr = (struct mbr *)buf;
	struct mbr_partition part[4];
	int i;
	// copy because we might overwrite our buffer
	// with an extended partition
	for (i=0;i<4;i++) {
		part[i] = mbr->part[i];
		if ((__bswap32(mbr->part[i].f_lba) + __bswap32(mbr->part[i].num_sect)) > max_lba)
			max_lba = __bswap32(mbr->part[i].f_lba) + __bswap32(mbr->part[i].num_sect);
	}

	printf(" Part Boot Type   Start   Length\n");
	for (i=0;i<4;i++) {
		if (!part[i].f_lba) {
			continue;
		}
		printf("   %2d   ", i+1);
		printf("%c   %02x %8x %8x\n", part[i].status & 0x80 ? '*':' ',
			part[i].type,
			__bswap32(part[i].f_lba),
			__bswap32(part[i].num_sect));

		if (part[i].type == 5) {
			readblock(md->buf, __bswap32(part[i].f_lba), 0xffffffff, md);
			parse_extended(md, extended, __bswap32(part[i].f_lba), max_lba);
		} else {
			register_legacy(md, part[i].status & 0x80, part[i].type,
					__bswap32(part[i].f_lba),
					__bswap32(part[i].num_sect), max_lba);
		}
	}

	return md->ret;
}

static void print_guid(GUID *x)
{
	(void)x; // In case we turned debugging off.

	// Somebody has got to be proud of this mixed endian prank.

	printf("%08x-%04x-%04x-%02x%02x-%02x%02x%02x%02x%02x%02x",
			__bswap32(x->u.UUID.time_low), __bswap16(x->u.UUID.time_mid),
			__bswap16(x->u.UUID.time_high_and_version),
			x->u.UUID.clock_seq_high_and_reserved, x->u.UUID.clock_seq_low,
			x->u.UUID.node[0], x->u.UUID.node[1], x->u.UUID.node[2],
			x->u.UUID.node[3], x->u.UUID.node[4], x->u.UUID.node[5]);
}

// Microsoft Basic Data Partition GUID (used for FAT and NTFS)
// EBD0A0A2-B9E5-4433-87C0-68B6B72699C7
static const GUID GUID_BASIC_DATA = {{
	{ 0xEBD0A0A2, 0xB9E5, 0x4433, 0x87, 0xC0,
	  { 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7 } }
}};

static int guid_equal(const GUID *a, const GUID *b)
{
	return memcmp(a->u.raw, b->u.raw, 16) == 0;
}

static LONG ParseGPT(UBYTE *buf, struct MountData *md)
{
	struct gpt *gpt=(struct gpt *)buf;
	uint64_t max_lba = __bswap64(gpt->last_usable_lba);
	uint64_t pstart = __bswap64(gpt->entries_lba);
	int numparts = __bswap32(gpt->number_of_entries);
	int psize = __bswap32(gpt->size_of_entry);
	int i, pos = 0;

	printf(" part start at: %lld\n", pstart);
	printf(" Number of partitions: %d\n", numparts);
	printf(" size of entry: %d\n", psize);

	for (i = 0; i < numparts; i++) {
		if (i % 4 == 0) {
			pos = 0;
			readblock(md->buf, pstart++, 0xffffffff, md);
		}
		struct gpt_partition *gpt_par = (struct gpt_partition *)(md->buf + (pos * psize));
		pos++;

		/* skip empty partitions */
		if (gpt_par->first_lba == 0 && gpt_par->last_lba == 0)
			continue;

		uint64_t first_lba = __bswap64(gpt_par->first_lba);
		uint64_t last_lba = __bswap64(gpt_par->last_lba);

		printf("%d. %8llx - %8llx ", i, first_lba, last_lba);
		print_guid(&gpt_par->partition_type);
		printf("\n");

		if (guid_equal(&gpt_par->partition_type, &GUID_BASIC_DATA)) {
			register_legacy(md, 0, 0, (ULONG)first_lba,
					(ULONG)(last_lba - first_lba + 1),
					(ULONG)max_lba);
		} else {
			printf("   Skipping non-FAT partition type\n");
		}
	}

	return md->ret;
}

static LONG ScanMBR(struct MountData *md)
{
	LONG ret = -1;
	if (readblock(md->buf, 1, 0xffffffff, md)) {
		struct gpt *gpt = (struct gpt *)md->buf;
		if (!memcmp(gpt->signature, "EFI PART", 8)) {
			dbg("GPT found\n");
			ret = ParseGPT(md->buf, md);
		}
	}
	if (ret == -1 && readblock(md->buf, 0, 0xffffffff, md)) {
		struct mbr *mbr = (struct mbr *)md->buf;
		if (mbr->sig[0]==0x55 && mbr->sig[1]==0xaa) {
			dbg("MBR found\n");
			ret = ParseMBR(md->buf, md);
		}
	}

	return ret;
}
#endif

// Return values:
// If single unit number:
// -1 = No RDB found, device failed to open, disk error or RDB block checksum error.
// 0 = RDB found but no partitions found, disk error or mount failure.
// >0: Number of partitions mounted.
// If unit number array:
// Unit number is replaced with error code:
// Error codes are same as above except:
// -2 = Skipped, previous unit had RDBFF_LAST set.
LONG MountDrive(struct MountStruct *ms)
{
	LONG ret = -1;
	struct MsgPort *port = NULL;
	struct IOExtTD *request = NULL;
	struct ExpansionBase *ExpansionBase;
	struct DriveGeometry geom;
	struct ExecBase *SysBase = ms->SysBase;
	dbg("Starting..\n");
	ExpansionBase = (struct ExpansionBase*)OpenLibrary("expansion.library", 34);
	if (ExpansionBase) {
		struct MountData *md = AllocMem(sizeof(struct MountData), MEMF_CLEAR | MEMF_PUBLIC);
		if (md) {
			md->DOSBase = (struct DosLibrary*)OpenLibrary("dos.library", 34);
			md->SysBase = SysBase;
			md->ExpansionBase = ExpansionBase;
			dbg("SysBase=%p ExpansionBase=%p DosBase=%p\n", md->SysBase, md->ExpansionBase, md->DOSBase);
			md->configDev = ms->configDev;
			md->creator = ms->creatorName;
			md->slowSpinup = ms->slowSpinup;
			port = W_CreateMsgPort(SysBase);
			if(port) {
				request = (struct IOExtTD*)W_CreateIORequest(port, sizeof(struct IOExtTD), SysBase);
				if(request) {
					ULONG target;
					ULONG lun = 0;
					for (target = 0; target < 8; target++, lun = 0) {
						// Skip the host controller ID
						if (target == ms->hostId)
							continue;
						ULONG unitNum;
next_lun:
						if (target > 7 || lun > 7) {
							// Phase V wide SCSI scheme for IDs/LUNs > 7
							unitNum = lun * 10 * 1000 + target * 10 + HD_WIDESCSI;
						} else {
							// Traditional scheme for IDs/LUNs <= 7
							unitNum = target + lun * 10;
						}
						dbg("OpenDevice('%s', %"PRId32", %p, 0)\n", ms->deviceName, unitNum, request);
						UBYTE err = OpenDevice(ms->deviceName, unitNum, (struct IORequest*)request, 0);
						if (err == 0) {
							err = GetGeometry(request ,&geom);
							if (err == 0) {
								ret = -1;
								md->request    = request;
								md->devicename = ms->deviceName;
								md->blocksize  = geom.dg_SectorSize;
								md->unitnum    = unitNum;

								switch (geom.dg_DeviceType & SID_TYPE) {
								case DG_CDROM:
								case DG_WORM:
								case DG_OPTICAL_DISK:
									if (!ms->cdBoot) {
										printf("CDROM boot disabled.\n");
										break;
									}
									ret = ScanCDROM(md);
									break;

								case DG_DIRECT_ACCESS: // DISK
									ret = ScanRDSK(md);
#ifdef DISKLABELS
									if (ret==-1)
										ret = ScanMBR(md);
#endif
									break;
								default:
									printf("Don't know how to boot from device type %d.\n",
										geom.dg_DeviceType & SID_TYPE);
									break;
								}
							}

							// Disable motor after probing
							request->iotd_Req.io_Command = TD_MOTOR;
							request->iotd_Req.io_Length  = 0;
							DoIO((struct IORequest*)request);

							CloseDevice((struct IORequest*)request);

							if (ms->luns && (lun++ < 8) &&
							    (!md->wasLastLun)) {
								goto next_lun;
							}

							if (md->wasLastDev && !ms->ignoreLast) {
								dbg("RDBFF_LAST exit\n");
								break;
							}
						} else {
							dbg("OpenDevice(%s,%"PRId32") failed: %"PRId32"\n", ms->deviceName, unitNum, (BYTE)err);
						}
					}
					W_DeleteIORequest(request, SysBase);
				}
				W_DeleteMsgPort(port, SysBase);
			}
			if (md->DOSBase) {
				CloseLibrary(&md->DOSBase->dl_lib);
			}
			FreeMem(md, sizeof(struct MountData));
		}
		CloseLibrary(&ExpansionBase->LibNode);
	}
	dbg("Exit code %"PRId32"\n", ret);
	return ret;
}
