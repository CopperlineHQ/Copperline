/* SPDX-License-Identifier: BSD-2-Clause */
/*
 * Copyright 2026 Copperline contributors
 *
 * Board attachment glue shared with the A4091-derived Amiga device layer.
 */

#ifndef A2091_ATTACH_H
#define A2091_ATTACH_H

#include <exec/types.h>
#include "port.h"

struct ExecBase;
struct MsgPort;
struct timerequest;
struct ConfigDev;
struct Interrupt;
struct scsipi_periph;
struct siop_softc;

typedef struct {
    uint32_t              as_addr;
    struct ExecBase      *as_SysBase;
    int8_t                as_timer_running;
    uint8_t               as_irq_signal;
    uint32_t              as_irq_count;
    uint32_t              as_int_mask;
    uint32_t              as_timer_mask;
    struct Task          *as_svc_task;
    struct Interrupt     *as_isr;
    volatile uint8_t      as_exiting;
    struct siop_softc    *as_device_private;
    struct MsgPort       *as_timerport;
    struct timerequest   *as_timerio;
    struct callout      **as_callout_head;
    struct ConfigDev     *as_cd;
    void                 *as_scripts_copy;
    uint32_t              as_scripts_copy_size;
    uint8_t               need_chip_ram_dma;
    uint8_t               cdrom_boot;
    uint8_t               ignore_last;
    uint8_t               allow_disc;
} a4091_save_t;

extern a4091_save_t *asave;

int attach(device_t self, uint scsi_target, struct scsipi_periph **periph,
           uint flags);
void detach(struct scsipi_periph *periph);
int periph_still_attached(void);
int init_chan(device_t self, UBYTE *boardnum);
void deinit_chan(device_t self);

uint8_t get_dip_switches(void);
uint8_t get_host_id(void);
uint8_t get_lun_count(void);
uint8_t get_target_count(void);
void decode_unit_number(ULONG unit_num, int *target, int *lun);
ULONG calculate_unit_number(int target, int lun);

#endif
