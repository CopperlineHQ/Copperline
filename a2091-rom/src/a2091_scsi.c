/* SPDX-License-Identifier: BSD-2-Clause */
/*
 * Copyright 2026 Copperline contributors
 *
 * WD33C93 PIO transport for the A2091/A590.  The combination-command flow
 * follows the WD33C93A data sheet: load destination/LUN/CDB/transfer count,
 * issue Select-with-ATN-and-Transfer, service the data register while DBR is
 * asserted, then acknowledge the completion and disconnect statuses.
 */

#include "port.h"
#include "a2091_scsi.h"

#include <string.h>

#include "scsi_all.h"

extern struct ExecBase *SysBase;

/* DMAC byte registers (the board is on the low byte of the 16-bit bus). */
#define DMAC_ISTR 0x0041
#define DMAC_CNTR 0x0043
#define DMAC_ACR_HI 0x0084
#define DMAC_ACR_LO 0x0086
#define DMAC_DAWR 0x008f
#define DMAC_SASR 0x0091
#define DMAC_SCMD 0x0093
#define DMAC_ST_DMA 0x00e0
#define DMAC_SP_DMA 0x00e2
#define DMAC_CINT 0x00e4
#define DMAC_FLUSH 0x00e8

#define CNTR_PDMD 0x20
#define CNTR_INTEN 0x10
#define CNTR_DDIR 0x08

#define ISTR_INT_F 0x80
#define ISTR_INTS 0x40
#define ISTR_INT_P 0x10
#define ISTR_FE_FLG 0x01

/* WD33C93 register numbers. */
#define WD_OWN_ID         0x00
#define WD_CONTROL        0x01
#define WD_TIMEOUT        0x02
#define WD_CDB_1          0x03
#define WD_TARGET_LUN     0x0f
#define WD_COMMAND_PHASE  0x10
#define WD_TC_MSB         0x12
#define WD_DESTINATION_ID 0x15
#define WD_SCSI_STATUS    0x17
#define WD_COMMAND        0x18
#define WD_DATA           0x19

#define ASR_INT 0x80
#define ASR_BSY 0x20
#define ASR_CIP 0x10
#define ASR_DBR 0x01

#define WD_CMD_RESET           0x00
#define WD_CMD_ABORT           0x01
#define WD_CMD_SELECT_ATN_XFER 0x08

#define CSR_RESET_AF      0x01
#define CSR_SEL_XFER_DONE 0x16
#define CSR_ABORTED       0x28
#define CSR_TIMEOUT       0x42
#define CSR_UNEXP_STATUS  0x4b
#define CSR_DISC          0x85

/* Polling is intentional for the first hardware-safe transport. */
#define WD_POLL_LIMIT 4000000UL

static inline void
wd_select(struct siop_softc *sc, unsigned char reg)
{
    sc->board[DMAC_SASR] = reg;
}

static inline unsigned char
wd_asr(struct siop_softc *sc)
{
    return sc->board[DMAC_SASR];
}

static inline void
wd_write_selected(struct siop_softc *sc, unsigned char value)
{
    sc->board[DMAC_SCMD] = value;
}

static inline unsigned char
wd_read_selected(struct siop_softc *sc)
{
    return sc->board[DMAC_SCMD];
}

static void
wd_write(struct siop_softc *sc, unsigned char reg, unsigned char value)
{
    wd_select(sc, reg);
    wd_write_selected(sc, value);
}

static unsigned char
wd_read(struct siop_softc *sc, unsigned char reg)
{
    wd_select(sc, reg);
    return wd_read_selected(sc);
}

static int
wd_wait(struct siop_softc *sc, unsigned char mask, unsigned char wanted)
{
    unsigned long left = WD_POLL_LIMIT;
    while (left-- != 0) {
        if ((wd_asr(sc) & mask) == wanted)
            return 0;
    }
    return -1;
}

static int
wd_wait_interrupt(struct siop_softc *sc)
{
    unsigned long left = WD_POLL_LIMIT;

    while (left-- != 0) {
        if (sc->irq_count || (wd_asr(sc) & ASR_INT) != 0)
            return 0;
    }
    return -1;
}

static unsigned char
wd_ack(struct siop_softc *sc)
{
    unsigned char csr;

    Disable();
    if (sc->irq_count) {
        csr = sc->irq_csr[sc->irq_head];
        sc->irq_head = (sc->irq_head + 1) & 3;
        sc->irq_count--;
        Enable();
        return csr;
    }
    csr = wd_read(sc, WD_SCSI_STATUS);
    Enable();
    return csr;
}

static void
wd_set_transfer_count(struct siop_softc *sc, unsigned long count)
{
    wd_select(sc, WD_TC_MSB);
    wd_write_selected(sc, (unsigned char)(count >> 16));
    wd_write_selected(sc, (unsigned char)(count >> 8));
    wd_write_selected(sc, (unsigned char)count);
}

static void
wd_abort(struct siop_softc *sc)
{
    wd_write(sc, WD_COMMAND, WD_CMD_ABORT);
    if (wd_wait_interrupt(sc) == 0)
        (void)wd_ack(sc);
}

static int
wd_reset(struct siop_softc *sc)
{
    wd_write(sc, WD_OWN_ID, 0x08 | 7); /* EAF, 8-10 MHz band, host ID 7. */
    wd_write(sc, WD_COMMAND, WD_CMD_RESET);
    if (wd_wait_interrupt(sc) != 0)
        return -1;
    if (wd_ack(sc) != CSR_RESET_AF)
        return -1;
    wd_write(sc, WD_CONTROL, 0x00);    /* asynchronous PIO */
    wd_write(sc, WD_TIMEOUT, 0x02);    /* brisk absent-target scan */
    return 0;
}

int
a2091_controller_init(struct siop_softc *sc,
                      volatile unsigned char *board)
{
    sc->board = board;
    sc->irq_head = 0;
    sc->irq_count = 0;
    board[DMAC_CNTR] = CNTR_PDMD;
    board[DMAC_DAWR] = 3;
    return wd_reset(sc);
}

void
a2091_controller_shutdown(struct siop_softc *sc)
{
    sc->board[DMAC_CNTR] = CNTR_PDMD;
    wd_abort(sc);
}

int
a2091_controller_interrupt(struct siop_softc *sc)
{
    unsigned char istr = sc->board[DMAC_ISTR];

    if ((istr & (ISTR_INT_F | ISTR_INT_P | ISTR_INTS)) !=
        (ISTR_INT_F | ISTR_INT_P | ISTR_INTS))
        return 0;
    /* Keep the chip interrupt asserted if software has not drained the
     * small completion FIFO yet; this avoids losing the closely-spaced
     * command-complete and disconnect causes. */
    if (sc->irq_count == sizeof(sc->irq_csr))
        return 1;
    sc->irq_csr[(sc->irq_head + sc->irq_count) & 3] =
        wd_read(sc, WD_SCSI_STATUS);
    sc->irq_count++;
    /* One hardware interrupt completes one DMA transaction.  Gate the
     * shared PORTS line before the closely-following disconnect cause;
     * the command tail drains that cause by polling. */
    sc->board[DMAC_CNTR] = CNTR_PDMD;
    return 1;
}

static void
dmac_write_word(struct siop_softc *sc, unsigned short off,
                unsigned short value)
{
    *(volatile unsigned short *)(sc->board + off) = value;
}

static void
dmac_strobe(struct siop_softc *sc, unsigned short off)
{
    (void)*(volatile unsigned short *)(sc->board + off);
}

static int
a2091_dma_safe(const void *data, unsigned long length)
{
    unsigned long start = (unsigned long)data;
    unsigned long end = start + length;

    /* Short inquiry/sense/mode transfers stay in PIO; DMA is for sectors. */
    return length >= 512 && (start & 1) == 0 && (length & 1) == 0 &&
           end >= start && end <= 0x01000000UL;
}

static void
a2091_dma_start(struct siop_softc *sc, struct scsipi_xfer *xs, int data_out)
{
    unsigned long address = (unsigned long)xs->data;

    if (SysBase->LibNode.lib_Version >= 37)
        CacheClearU();
    sc->board[DMAC_CNTR] = CNTR_PDMD | CNTR_INTEN |
                           (data_out ? CNTR_DDIR : 0);
    dmac_write_word(sc, DMAC_ACR_HI, (unsigned short)(address >> 16));
    dmac_write_word(sc, DMAC_ACR_LO, (unsigned short)address);
    dmac_strobe(sc, DMAC_ST_DMA);
}

static void
a2091_dma_stop(struct siop_softc *sc, int data_in)
{
    unsigned long left = WD_POLL_LIMIT;

    if (data_in) {
        dmac_strobe(sc, DMAC_FLUSH);
        while (left-- != 0 && (sc->board[DMAC_ISTR] & ISTR_FE_FLG) == 0)
            ;
    }
    dmac_strobe(sc, DMAC_CINT);
    dmac_strobe(sc, DMAC_SP_DMA);
    /* Leave INT2 gated while the command-complete disconnect is drained
     * by the polling tail.  The next DMA command enables it again. */
    sc->board[DMAC_CNTR] = CNTR_PDMD;
    if (data_in && SysBase->LibNode.lib_Version >= 37)
        CacheClearU();
}

static void
a2091_complete(struct scsipi_xfer *xs, scsipi_xfer_result_t error,
               unsigned char status, int resid)
{
    xs->error = error;
    xs->status = status;
    xs->resid = resid;
    scsipi_done(xs);
}

static void
a2091_run_xfer(struct siop_softc *sc, struct scsipi_xfer *xs)
{
    struct scsipi_periph *periph = xs->xs_periph;
    unsigned long left = WD_POLL_LIMIT;
    int transferred = 0;
    unsigned char asr;
    unsigned char csr;
    unsigned char status = 0;
    int data_in = (xs->xs_control & XS_CTL_DATA_IN) != 0;
    int data_out = (xs->xs_control & XS_CTL_DATA_OUT) != 0;
    int use_dma = xs->datalen != 0 && (data_in || data_out) &&
                  a2091_dma_safe(xs->data, (unsigned long)xs->datalen);
    int i;

    if (wd_wait(sc, ASR_CIP, 0) != 0) {
        a2091_complete(xs, XS_TIMEOUT, 0, xs->datalen);
        return;
    }

    wd_write(sc, WD_CONTROL, use_dma ? 0x80 : 0x00);
    wd_write(sc, WD_DESTINATION_ID, (unsigned char)periph->periph_target);
    wd_write(sc, WD_TARGET_LUN, (unsigned char)periph->periph_lun);
    wd_select(sc, WD_CDB_1);
    for (i = 0; i < xs->cmdlen; i++)
        wd_write_selected(sc, ((unsigned char *)xs->cmd)[i]);
    wd_set_transfer_count(sc, (unsigned long)xs->datalen);
    if (use_dma)
        a2091_dma_start(sc, xs, data_out);
    wd_write(sc, WD_COMMAND, WD_CMD_SELECT_ATN_XFER);

    if (!use_dma && xs->datalen != 0 && (data_in || data_out)) {
        wd_select(sc, WD_DATA);
        while (transferred < xs->datalen && left-- != 0) {
            asr = wd_asr(sc);
            if ((asr & ASR_DBR) != 0) {
                if (data_in)
                    xs->data[transferred] = wd_read_selected(sc);
                else
                    wd_write_selected(sc, xs->data[transferred]);
                transferred++;
            } else if ((asr & ASR_INT) != 0) {
                break;
            }
        }
        if (left == 0) {
            wd_abort(sc);
            a2091_complete(xs, XS_TIMEOUT, 0, xs->datalen - transferred);
            return;
        }
    }

    if ((wd_asr(sc) & ASR_INT) == 0 && wd_wait_interrupt(sc) != 0) {
        wd_abort(sc);
        a2091_complete(xs, XS_TIMEOUT, 0, xs->datalen - transferred);
        return;
    }
    csr = wd_ack(sc);

    /* A short variable-length reply changes to status with TC non-zero. */
    if (csr == CSR_UNEXP_STATUS) {
        wd_write(sc, WD_COMMAND_PHASE, 0x46);
        wd_write(sc, WD_COMMAND, WD_CMD_SELECT_ATN_XFER);
        if (wd_wait_interrupt(sc) != 0) {
            wd_abort(sc);
            a2091_complete(xs, XS_TIMEOUT, 0, xs->datalen - transferred);
            return;
        }
        csr = wd_ack(sc);
    }

    if (csr == CSR_TIMEOUT) {
        if (use_dma)
            a2091_dma_stop(sc, data_in);
        a2091_complete(xs, XS_SELTIMEOUT, 0, xs->datalen - transferred);
        return;
    }
    if (csr == CSR_ABORTED || csr != CSR_SEL_XFER_DONE) {
        if (use_dma)
            a2091_dma_stop(sc, data_in);
        a2091_complete(xs, XS_DRIVER_STUFFUP, 0, xs->datalen - transferred);
        return;
    }

    if (use_dma) {
        a2091_dma_stop(sc, data_in);
        transferred = xs->datalen;
    }

    status = wd_read(sc, WD_TARGET_LUN);

    /* The chip posts bus-free after completion. Drain it if it is ready. */
    if (wd_wait_interrupt(sc) == 0) {
        unsigned char disc = wd_ack(sc);
        (void)disc;
    }

    a2091_complete(xs,
                   status == SCSI_OK ? XS_NOERROR : XS_BUSY,
                   status, xs->datalen - transferred);
}

void
a2091_scsipi_request(struct scsipi_channel *chan, scsipi_adapter_req_t req,
                     void *arg)
{
    struct siop_softc *sc = device_private(chan->chan_adapter->adapt_dev);

    switch (req) {
    case ADAPTER_REQ_RUN_XFER:
        a2091_run_xfer(sc, (struct scsipi_xfer *)arg);
        break;
    case ADAPTER_REQ_GROW_RESOURCES:
    case ADAPTER_REQ_SET_XFER_MODE:
        break;
    }
}
