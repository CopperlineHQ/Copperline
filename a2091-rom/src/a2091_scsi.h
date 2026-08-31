/* SPDX-License-Identifier: BSD-2-Clause */
/*
 * Copyright 2026 Copperline contributors
 */

#ifndef A2091_SCSI_H
#define A2091_SCSI_H

#include "scsipiconf.h"

/*
 * The common A4091-derived driver calls its controller-private object a
 * siop_softc.  Keep that internal ABI name here so the shared command and
 * disk layers do not need board-specific casts; this object describes a
 * WD33C93, not an NCR SIOP.
 */
struct siop_softc {
    device_t sc_dev;
    struct scsipi_adapter sc_adapter;
    struct scsipi_channel sc_channel;
    volatile unsigned char *board;
    volatile unsigned char irq_csr[4];
    volatile unsigned char irq_head;
    volatile unsigned char irq_count;
};

void a2091_scsipi_request(struct scsipi_channel *chan,
                          scsipi_adapter_req_t req, void *arg);
int a2091_controller_init(struct siop_softc *sc,
                          volatile unsigned char *board);
void a2091_controller_shutdown(struct siop_softc *sc);
int a2091_controller_interrupt(struct siop_softc *sc);

#endif
