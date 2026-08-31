/*
 * Copyright 2022-2025 Stefan Reinauer & Chris Hooper
 * Copyright 2026 Copperline contributors
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright notice,
 *    this list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the documentation
 *    and/or other materials provided with the distribution.
 */

#include "port.h"
#include "printf.h"

#include <clib/expansion_protos.h>
#include <clib/exec_protos.h>
#include <devices/scsidisk.h>
#include <exec/errors.h>
#include <exec/interrupts.h>
#include <exec/lists.h>
#include <exec/memory.h>
#include <hardware/intbits.h>
#include <inline/expansion.h>
#include <libraries/configvars.h>
#include <libraries/expansionbase.h>
#include <proto/expansion.h>
#include <proto/exec.h>
#include <string.h>

#include "a2091_scsi.h"
#include "attach.h"
#include "device.h"
#include "scsi_all.h"
#include "scsi_spc.h"
#include "scsipiconf.h"
#include "sd.h"

#define A2091_MANUFACTURER 514
#define A2091_PRODUCT      3
#define A2091_PRODUCT_OLD  2

extern struct ExecBase *SysBase;
extern int romboot;
struct ExpansionBase *ExpansionBase;

static LONG __saveds
a2091_irq_handler(register a4091_save_t *save asm("a1"))
{
    if (!a2091_controller_interrupt(save->as_device_private))
        return 0;
    save->as_irq_count++;
    if (save->as_svc_task != NULL)
        Signal(save->as_svc_task, BIT(save->as_irq_signal));
    return 1;
}

static int
install_interrupt_server(void)
{
    asave->as_isr = AllocMem(sizeof(*asave->as_isr),
                             MEMF_PUBLIC | MEMF_CLEAR);
    if (asave->as_isr == NULL)
        return ERROR_NO_MEMORY;
    asave->as_isr->is_Node.ln_Type = NT_INTERRUPT;
    asave->as_isr->is_Node.ln_Pri = 0;
    asave->as_isr->is_Node.ln_Name = "scsi.device A2091";
    asave->as_isr->is_Data = asave;
    asave->as_isr->is_Code = (void (*)())a2091_irq_handler;
    AddIntServer(INTB_PORTS, asave->as_isr);
    return 0;
}

static void
remove_interrupt_server(void)
{
    struct Interrupt *isr = asave->as_isr;

    if (isr == NULL)
        return;
    asave->as_isr = NULL;
    RemIntServer(INTB_PORTS, isr);
    FreeMem(isr, sizeof(*isr));
}

#undef NewMinList
void
NewMinList(struct MinList *list)
{
    list->mlh_Tail = NULL;
    list->mlh_Head = (struct MinNode *)&list->mlh_Tail;
    list->mlh_TailPred = (struct MinNode *)list;
}

void *
device_private(device_t dev)
{
    (void)dev;
    return asave->as_device_private;
}

static struct ConfigDev *
find_board(UBYTE *boardnum)
{
    struct ConfigDev *cd = NULL;
    int count = 0;

    ExpansionBase = (struct ExpansionBase *)OpenLibrary("expansion.library", 0);
    if (ExpansionBase == NULL)
        return NULL;

    if (romboot) {
        struct CurrentBinding binding;
        if (GetCurrentBinding(&binding, sizeof(binding)))
            cd = binding.cb_ConfigDev;
    } else {
        do {
            cd = FindConfigDev(cd, A2091_MANUFACTURER, A2091_PRODUCT);
            if (cd != NULL && (cd->cd_Flags & CDB_CONFIGME) != 0) {
                cd->cd_Flags &= ~CDB_CONFIGME;
                *boardnum = count;
                break;
            }
            count++;
        } while (cd != NULL);

        if (cd == NULL) {
            count = 0;
            do {
                cd = FindConfigDev(cd, A2091_MANUFACTURER, A2091_PRODUCT_OLD);
                if (cd != NULL && (cd->cd_Flags & CDB_CONFIGME) != 0) {
                    cd->cd_Flags &= ~CDB_CONFIGME;
                    *boardnum = count;
                    break;
                }
                count++;
            } while (cd != NULL);
        }
    }

    CloseLibrary((struct Library *)ExpansionBase);
    ExpansionBase = NULL;
    return cd;
}

uint8_t
get_dip_switches(void)
{
    /* The A2091 has no SCSI-ID or policy DIP bank. */
    return 0xff;
}

uint8_t
get_host_id(void)
{
    return 7;
}

uint8_t
get_lun_count(void)
{
    return 8;
}

uint8_t
get_target_count(void)
{
    return 8;
}

int
init_chan(device_t self, UBYTE *boardnum)
{
    struct siop_softc *sc = device_private(self);
    struct scsipi_adapter *adapt = &sc->sc_adapter;
    struct scsipi_channel *chan = &sc->sc_channel;
    struct ConfigDev *cd = find_board(boardnum);
    int signal;

    if (cd == NULL || cd->cd_BoardAddr == NULL)
        return ERROR_NO_BOARD;

    asave->as_cd = cd;
    asave->as_addr = (uint32_t)cd->cd_BoardAddr;
    memset(sc, 0, sizeof(*sc));

    sc->sc_dev = self;
    sc->board = (volatile unsigned char *)cd->cd_BoardAddr;

    memset(adapt, 0, sizeof(*adapt));
    adapt->adapt_dev = self;
    adapt->adapt_nchannels = 1;
    adapt->adapt_openings = 1;
    adapt->adapt_request = a2091_scsipi_request;
    adapt->adapt_asave = asave;

    memset(chan, 0, sizeof(*chan));
    chan->chan_adapter = adapt;
    chan->chan_ntargets = 8;
    chan->chan_nluns = 8;
    chan->chan_id = 7;
    TAILQ_INIT(&chan->chan_queue);
    TAILQ_INIT(&chan->chan_complete);
    scsipi_channel_init(chan);

    signal = AllocSignal(-1);
    if (signal < 0)
        return ERROR_NO_FREE_STORE;
    asave->as_irq_signal = (uint8_t)signal;
    asave->as_callout_head = &callout_head;

    if (a2091_controller_init(sc, sc->board) != 0) {
        FreeSignal(asave->as_irq_signal);
        return ERROR_BAD_BOARD;
    }
    if (install_interrupt_server() != 0) {
        a2091_controller_shutdown(sc);
        FreeSignal(asave->as_irq_signal);
        return ERROR_NO_MEMORY;
    }
    sc->board[0x43] = 0x20; /* DMAC PDMD; DMA commands enable INT2. */
    return 0;
}

void scsipi_free_all_xs(struct scsipi_channel *chan);

void
deinit_chan(device_t self)
{
    struct siop_softc *sc = device_private(self);
    remove_interrupt_server();
    a2091_controller_shutdown(sc);
    scsipi_free_all_xs(&sc->sc_channel);
    FreeSignal(asave->as_irq_signal);
}

struct scsipi_periph *
scsipi_alloc_periph(int flags)
{
    struct scsipi_periph *periph;
    uint i;
    (void)flags;

    periph = AllocMem(sizeof(*periph), MEMF_PUBLIC | MEMF_CLEAR);
    if (periph == NULL)
        return NULL;
    for (i = 0; i < PERIPH_NTAGWORDS; i++)
        periph->periph_freetags[i] = 0xffffffff;
    return periph;
}

void
scsipi_free_periph(struct scsipi_periph *periph)
{
    FreeMem(periph, sizeof(*periph));
}

int scsi_probe_device(struct scsipi_channel *chan, int target, int lun,
                      struct scsipi_periph *periph, int *failed);

int
attach(device_t self, uint scsi_unit, struct scsipi_periph **periph_p,
       uint flags)
{
    struct siop_softc *sc = device_private(self);
    struct scsipi_channel *chan = &sc->sc_channel;
    struct scsipi_periph *periph;
    int target, lun, failed = 0;
    int rc;
    (void)flags;

    decode_unit_number(scsi_unit, &target, &lun);
    if (target >= 8 || lun >= 8)
        return ERROR_OPEN_FAIL;
    if (target == chan->chan_id)
        return ERROR_SELF_UNIT;

    periph = scsipi_alloc_periph(0);
    *periph_p = periph;
    if (periph == NULL)
        return ERROR_NO_MEMORY;

    periph->periph_openings = 1;
    periph->periph_target = target;
    periph->periph_lun = lun;
    periph->periph_changenum = 1;
    periph->periph_channel = chan;
    NewMinList(&periph->periph_changeintlist);

    rc = scsi_probe_device(chan, target, lun, periph, &failed);
    (void)rc;
    if (failed) {
        scsipi_free_periph(periph);
        return failed;
    }
    scsipi_insert_periph(chan, periph);
    return 0;
}

ULONG
calculate_unit_number(int target, int lun)
{
    return target + lun * 10;
}

void
decode_unit_number(ULONG unit_num, int *target, int *lun)
{
    *target = unit_num % 10;
    *lun = unit_num / 10;
}

void
detach(struct scsipi_periph *periph)
{
    if (periph != NULL) {
        struct scsipi_channel *chan = periph->periph_channel;
        scsipi_remove_periph(chan, periph);
        scsipi_free_periph(periph);
    }
}

int
periph_still_attached(void)
{
    uint i;
    struct siop_softc *sc = asave->as_device_private;
    struct scsipi_channel *chan = &sc->sc_channel;

    for (i = 0; i < SCSIPI_CHAN_PERIPH_BUCKETS; i++)
        if (LIST_FIRST(&chan->chan_periphtab[i]) != NULL)
            return 1;
    return 0;
}
