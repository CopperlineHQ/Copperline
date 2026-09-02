//
// Copyright 2022-2025 Stefan Reinauer & Chris Hooper
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
#include <devices/trackdisk.h>
#include <dos/dostags.h>
#include <exec/errors.h>
#include <exec/execbase.h>
#include <exec/resident.h>
#include <exec/semaphores.h>
#include <exec/io.h>
#include <libraries/dos.h>
#include <proto/dos.h>
#include <proto/exec.h>
#include <proto/expansion.h>
#include <utility/tagitem.h>
#include <clib/alib_protos.h>
#include <devices/scsidisk.h>
#include <resources/filesysres.h>
#include <string.h>

#include "device.h"
#include "cmdhandler.h"
#include "version.h"
#include "mounter.h"
#include "bootmenu.h"
#include "romfile.h"
#include "attach.h"

#ifdef DEBUG
#include <clib/debug_protos.h>
#endif


#define DEVICE_PRIORITY 10  // Fine to leave priority as zero

#define DEVICE_NAME XSTR(DEVNAME) ".device"

struct ExecBase *SysBase;
struct MsgPort *myPort;

static BPTR saved_seg_list;

/*
 * -----------------------------------------------------------
 * A library or device with a romtag should start with moveq #-1,d0
 * (to safely return an error if a user tries to execute the file),
 * followed by a Resident structure.
 * ------------------------------------------------------------
 */
int __attribute__((no_reorder)) _start(void);
int __attribute__((no_reorder)) _start(void)
{
    return (-1);
}

/*
 * -----------------------------------------------------------
 * A romtag structure. After the driver is brought in from disk, the
 * disk image will be scanned for this structure to discover magic
 * constants about it (such as where to start running from...).
 *
 * endcode is a marker that shows the end of your code. Make sure it
 * does not span hunks, and is not before the rom tag! It is ok to
 * put it right after the rom tag -- that way you it's always safe.
 * Make sure the program has only a single code hunk if you put it
 * at the end of your code.
 * ------------------------------------------------------------
 */
asm("romtag:                                \n"
    "       dc.w    "XSTR(RTC_MATCHWORD)"   \n"
    "       dc.l    romtag                  \n"
    "       dc.l    endcode                 \n"
    "       dc.b    0                       \n"
    "       dc.b    "XSTR(DEVICE_VERSION)"  \n"
    "       dc.b    "XSTR(NT_DEVICE)"       \n"
    "       dc.b    "XSTR(DEVICE_PRIORITY)" \n"
    "       dc.l    _device_name            \n"
    "       dc.l    _device_id_string       \n"
    "       dc.l    _init                   \n"
    "endcode:                               \n");

static const char device_name[]      = DEVICE_NAME;
char real_device_name[17];
static struct SignalSemaphore entry_sem;
int romboot = FALSE;

/*
 * get_device_name
 * ---------------
 * Creates 2nd.a4091.device, 3rd.a4091.device etc
 */
static char *get_device_name(void)
{
    char *name = real_device_name;
    for (int i=0; i<8; i++) {
        if (i) {
            name[0]='0'+i+1;
            if (i==1)         name[1]='n';
            else if (i==2)    name[1]='r';
            else              name[1]='t';
            if (i==1 || i==2) name[2]='d';
            else              name[2]='h';
            name[3]='.';
            name[4]=0;
        } else
            name[0]=0;
        strcat(name, device_name);

#ifdef TEST_2ND_DEVICE
        if (i==1)
#else
        if (FindName(&SysBase->DeviceList, name)==NULL)
#endif
            break;
    }
    return name;
}

/*
 * ------- init_device ---------------------------------------
 * FOR RTF_AUTOINIT:
 *   This routine gets called after the device has been allocated.
 *   The device pointer is in d0. The AmigaDOS segment list is in a0.
 *   If it returns the device pointer, then the device will be linked
 *   into the device list.  If it returns NULL, then the device
 *   will be unloaded.
 *
 * IMPORTANT:
 *   If you don't use the "RTF_AUTOINIT" feature, there is an additional
 *   caveat. If you allocate memory in your Open function, remember that
 *   allocating memory can cause an Expunge... including an expunge of your
 *   device. This must not be fatal. The easy solution is don't add your
 *   device to the list until after it is ready for action.
 *
 * CAUTION:
 *   This function runs in a forbidden state !!!
 *   This call is single-threaded by Exec
 * ------------------------------------------------------------
 */
static struct Library __used __saveds *
init_device(BPTR seg_list asm("a0"), struct Library *dev asm("d0"))
{
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Warray-bounds="
    /* !!! required !!! save a pointer to exec */
    SysBase = *(struct ExecBase **)4UL;
#pragma GCC diagnostic pop

    /* save pointer to our loaded code (the SegList) */
    saved_seg_list = seg_list;

    get_device_name();
    dev->lib_Node.ln_Type = NT_DEVICE;
    dev->lib_Node.ln_Name = real_device_name;
    dev->lib_Flags = LIBF_SUMUSED | LIBF_CHANGED;
    dev->lib_Version = DEVICE_VERSION;
    dev->lib_Revision = DEVICE_REVISION;
    dev->lib_IdString = (APTR)device_id_string;

    /* Start thread to manage board and process commands */
    InitSemaphore(&entry_sem);
    ObtainSemaphore(&entry_sem);
    printf(XSTR(DEVNAME)": %s %s\n", device_name, device_id_string);
    dev->lib_OpenCnt++;

    int board_num = 0;
    if (start_cmd_handler(&board_num)) {
        printf("Start handler failed\n");
        dev->lib_OpenCnt--;
        ReleaseSemaphore(&entry_sem);
        return (NULL);
    }

    dev->lib_OpenCnt--;
    ReleaseSemaphore(&entry_sem);

    return (dev);
}

/*
 * device dependent expunge function
 * ------------------------------------------------------------
 * !!! CAUTION: This function runs in a forbidden state !!!
 * This call is guaranteed to be single-threaded; only one task
 * will execute your Expunge at a time.
 */
static BPTR __used __saveds
drv_expunge(struct Library *dev asm("a6"))
{
    ObtainSemaphore(&entry_sem);

    if ((dev->lib_OpenCnt != 0) || periph_still_attached()) {
        printf("expunge() device still open\n");
        dev->lib_Flags |= LIBF_DELEXP;  // Indicate I'll expunge myself later
        ReleaseSemaphore(&entry_sem);
        return (0);
    }

    printf("expunge() %s\n", device_id_string);

    /* Ask the command handler to shut down */
    stop_cmd_handler();

    Forbid();
    BPTR seg_list = saved_seg_list;
    Remove(&dev->lib_Node);
    FreeMem((char *)dev - dev->lib_NegSize,
            dev->lib_NegSize + dev->lib_PosSize);
    ReleaseSemaphore(&entry_sem);
    return (seg_list);
}

/*
 * drv_open
 * --------
 * Opens the device for access. The first call of this function
 * will perform an attach (initialize) of the SCSI adapter.
 *
 * ------------------------------------------------------------
 *
 * CAUTION: This function runs in Forbid() state. It is guaranteed
 *          to be single-threaded; only one task will execute Open()
 *          at a time.
 */
static void __used __saveds
drv_open(struct Library *dev asm("a6"), struct IORequest *ioreq asm("a1"),
         uint scsi_unit asm("d0"), ULONG flags asm("d1"))
{
    int rc;
    ioreq->io_Message.mn_Node.ln_Type = NT_REPLYMSG;

    ObtainSemaphore(&entry_sem);
    dev->lib_OpenCnt++;

    if ((rc = open_unit(scsi_unit, (void **) &ioreq->io_Unit, flags)) != 0) {
        int target, lun;
        decode_unit_number(scsi_unit, &target, &lun);
        printf("Open fail %d.%d\n", target, lun);
        dev->lib_OpenCnt--;
        ioreq->io_Error = rc;
        ReleaseSemaphore(&entry_sem);
        return;
    }
#if 0
    printf("Open Unit=%p\n", ioreq->io_Unit);
#endif

    ioreq->io_Error = 0; // Success

    ReleaseSemaphore(&entry_sem);
}

/*
 * drv_close
 * ---------
 * Closes the device. The last call of this function will perform an detach
 * (reset) of the SCSI adapter.
 *
 * ------------------------------------------------------------
 *
 * CAUTION: This function runs in Forbid() state. It is guaranteed
 *          to be single-threaded; only one task will execute Close()
 *          at a time.
 */
static BPTR __used __saveds
drv_close(struct Library *dev asm("a6"), struct IORequest *ioreq asm("a1"))
{
    ObtainSemaphore(&entry_sem);
#if 0
    printf("close %u\n", dev->lib_OpenCnt);
#endif

    close_unit((void *) ioreq->io_Unit);

    dev->lib_OpenCnt--;
    ReleaseSemaphore(&entry_sem);
    if (dev->lib_Flags & LIBF_DELEXP)
        return (drv_expunge(dev));
    return (0);
}

/* device dependent beginio function */
static void __used __saveds
drv_begin_io(struct Library *dev asm("a6"), struct IORequest *ior asm("a1"))
{
    (void)dev;

    /* These commands are forced to always execute in immediate mode */
    switch (ior->io_Command) {
        case TD_REMCHANGEINT:
            printf("TD_REMCHANGEINT\n");
            td_remchangeint(ior);
            return;
        case CMD_START:
            /*
             * This driver doesn't currently disable queue processing
             * on a CMD_STOP, so it does not need to immediately
             * execute a CMD_START.
             */
            break;
    }

    /* These commands may optionally execute in immediate mode */
    if (ior->io_Flags & IOF_QUICK) {
        switch (ior->io_Command) {
            case TD_ADDCHANGEINT:
                printf("TD_ADDCHANGEINT\n");
                td_addchangeint(ior);
                return;
            case TD_REMOVE:
                printf("TD_REMOVE\n");
                td_remove(ior);
                return;
        }
    }

    /* All other commands must be pushed to the driver task */
    ior->io_Flags &= ~IOF_QUICK;
    PutMsg(myPort, &ior->io_Message);
}

/* device dependent abortio function */
static ULONG __used __saveds
drv_abort_io(struct Library *dev asm("a6"), struct IORequest *ior asm("a1"))
{
    (void)dev;
    (void)ior;

    printf("abort_io(%d)\n", ior->io_Command);

    return (IOERR_NOCMD);
}

static const ULONG device_vectors[] =
{
    (ULONG) drv_open,
    (ULONG) drv_close,
    (ULONG) drv_expunge,
    0,   // extFunc not used here
    (ULONG) drv_begin_io,
    (ULONG) drv_abort_io,
    -1   // function table end marker
};

static void list_filesystems(void)
{
#ifdef DEBUG_DEVICE
	struct FileSysResource *FileSysResBase = NULL;
	struct FileSysEntry *fse;

	if (!(FileSysResBase = (struct FileSysResource *)OpenResource(FSRNAME))) {
		printf("Cannot open %s\n", FSRNAME);
		return;
	}

	printf("DosType Version   Creator\n");
	printf("---------------------------------------------------------------------------\n");
	for (fse = (struct FileSysEntry *)FileSysResBase->fsr_FileSysEntries.lh_Head;
		  fse->fse_Node.ln_Succ;
		  fse = (struct FileSysEntry *)fse->fse_Node.ln_Succ) {
		int x;
		for (x=24; x>=8; x-=8)
			putchar((fse->fse_DosType >> x) & 0xFF);

		putchar((fse->fse_DosType & 0xFF) < 0x30
						? (fse->fse_DosType & 0xFF) + 0x30
						: (fse->fse_DosType & 0xFF));
		printf("    %3d.%-3d", (fse->fse_Version >> 16), (fse->fse_Version & 0xFFFF));
		if(fse->fse_Node.ln_Name[0]) {
			char term;
			int name_len = strlen(fse->fse_Node.ln_Name);

			printf("  %.55s", fse->fse_Node.ln_Name);
			if (name_len >= 55) {
				printf("...");
				name_len = 55;
			}
			term = fse->fse_Node.ln_Name[name_len-1];
			if (term != 10 && term != 13)
				printf("\n");
		} else {
			printf("  N/A\n");
		}
	}
#endif
}

static int mount_drives(struct ConfigDev *cd, struct Library *dev)
{
	extern char real_device_name[];
	struct MountStruct ms;
	int i, j = 1, ret = 0;
	UBYTE dip_switches = get_dip_switches();
	UBYTE hostid = get_host_id();
#ifdef NCR53C770
	ULONG unitNum[16];
	int max_targets = get_target_count();
#else
	ULONG unitNum[8];
	int max_targets = 8;
#endif
	(void)dev;

	/* Produce unitNum at runtime */
	unitNum[0] = max_targets - 1;
	for (i=0; i<max_targets; i++)
		if (hostid != i)
			unitNum[j++] = i;

	printf("Mounter:\n");
	ms.deviceName = real_device_name;
	ms.unitNum = unitNum;
	ms.creatorName = NULL;
	ms.configDev = cd;
	ms.SysBase = SysBase;
	ms.luns = (get_lun_count() > 1);
	ms.slowSpinup = !(dip_switches & BIT(4));  // 0: Short Spinup 1: Long Spinup
	ms.cdBoot = asave->cdrom_boot;
	ms.ignoreLast = asave->ignore_last;
	ms.hostId = hostid;

	ret = MountDrive(&ms);

	printf("ret = %x\nunitNum = { ", ret);
	for (i=0; i<max_targets; i++)
		printf("%x%s", unitNum[i], i < (max_targets - 1) ? ", " : " }\n");

	list_filesystems();
	return ret;
}

static struct Library __used __saveds *
init(BPTR seg_list asm("a0"), struct Library *dev asm("d0"))
{
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Warray-bounds="
    SysBase = *(struct ExecBase **)4UL;
#pragma GCC diagnostic pop

    if (seg_list == 0)
        romboot = TRUE;

    struct Library *mydev = MakeLibrary((ULONG *)&device_vectors, NULL,
            (APTR)init_device, sizeof(struct Library), seg_list);

    if (mydev != NULL) {
        AddDevice((struct Device *)mydev);

        if (romboot) {
            mount_drives(asave->as_cd, dev);
            boot_menu();
        }
    }

    return mydev;
}


#if HAVE_ROM == 0
asm(
"    .globl _bootblock, _bootblock_end          \n"
"    .globl _entrypoint, _entrypoint_end        \n"
"    _entrypoint     = 0xdd0000                 \n"
"    _entrypoint_end = 0xdd4000                 \n"
"_bootblock: lea     _dosname(pc),a1            \n"
"            jsr     -96(a6) /* FindResident */ \n"
"            tst.l   d0                         \n"
"            beq.s   fail                       \n"
"            move.l  d0,a0                      \n"
"            move.l  22(a0),a0 /* RT_INIT */    \n"
"            jsr     (a0)                       \n"
"fail:       rts                                \n"
"_dosname: .asciz \"dos.library\"               \n"
"_bootblock_end:                                \n"
);
#endif
