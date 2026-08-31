//
// Copyright 2022-2023 Chris Hooper & Stefan Reinauer
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

#ifndef _SD_H
#define _SD_H

#define MAX_BOUNCE_SIZE (256 * 1024)

uint32_t get_scripts_dma_addr(const void *scripts, uint32_t size);
#if defined(SCRIPTS_IN_MAINBOARD_RAM)
uint32_t get_scripts_mainboard_addr(const void *scripts, uint32_t size,
                                    uint32_t align);
#endif
void free_scripts_copy(void);

int sd_readwrite(void *periph, uint64_t blkno, uint b_flags,
                 void *buf, uint buflen, void *ior);
int sd_seek(void *periph_p, uint64_t blkno, void *ior);
int sd_scsidirect(void *periph, void *cmd_p, void *ior);
int sd_getgeometry(void *periph, void *buf, void *ior);
int sd_get_protstatus(void *periph_p, ULONG *status);
int sd_startstop(void *periph_p, void *ior, int start, int load_eject,
                 int immed);
int sd_testunitready(void *periph_p, void *ior);
void sd_testunitready_walk(struct scsipi_channel *chan);

uint32_t sd_blocksize(void *periph_p);

void sd_media_unloaded(struct scsipi_periph *periph);
void sd_media_loaded(struct scsipi_periph *periph);

#endif /* _SD_H */
