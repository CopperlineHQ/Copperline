/*
 * SPDX-FileCopyrightText: 2020-2026 Dimitris Panokostas
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Ported from BlitterStudio/host-tools (github.com/BlitterStudio/host-tools),
 * commit c14cf8c1be881d7157a0a051e3f6f4ed695c57d3, drivers/mhi/src/mhi_abi.h.
 * That file was itself a strict subset of the official MHI developer kit's
 * `Include/libraries/mhi.h` (Aminet driver/audio/mhi_dev.lha, Paul Qureshi &
 * Thomas Wenzel) -- it only defined the constants mhiuae.library actually
 * used (volume/panning, MPEG1/2/2.5, Layer3, VBR, joint-stereo). Extended
 * here with the full constant set from that official header (MHIQ_MPEG4,
 * MHIQ_LAYER1/LAYER2, MHIQ_BASS/TREBLE/MID/PREFACTOR_CONTROL,
 * MHIQ_5_BAND_EQ/MHIQ_10_BAND_EQ, MHIQ_CROSSMIXING, and the full MHIP_*
 * parameter set including the 5/10-band EQ aliases) so mhi_copperline.c can
 * answer MHIQuery per the complete spec table in docs/internals/mhi.md
 * ("The MHI-API/board split"), not just the subset mhiuae.library needed.
 * Values match the official header verbatim -- see test-assets/mhi-devkit/
 * extracted/Include/libraries/mhi.h (WP1 notes).
 */

#ifndef MHI_ABI_H
#define MHI_ABI_H

/* MHI status flags for player (MHIGetStatus). NOT the same numbering as
 * this board's own STATUS register -- see board.h's own note and
 * docs/internals/mhi.md "Status and control" for why that's deliberate. */
#define MHIF_PLAYING      0
#define MHIF_STOPPED      1
#define MHIF_OUT_OF_DATA  2
#define MHIF_PAUSED       3

/* MHI queries and returned values */
#define MHIF_UNSUPPORTED 0
#define MHIF_SUPPORTED   1
#define MHIF_FALSE       0
#define MHIF_TRUE        1

#define MHIQ_DECODER_NAME    1000
#define MHIQ_DECODER_VERSION 1001
#define MHIQ_AUTHOR          1002

#define MHIQ_IS_HARDWARE 1010
#define MHIQ_IS_68K      1011
#define MHIQ_IS_PPC      1012

#define MHIQ_CAPABILITIES 0
#define MHIQ_MPEG1        1
#define MHIQ_MPEG2        2
#define MHIQ_MPEG25       3
#define MHIQ_MPEG4        4 /* there is no MPEG3! */

#define MHIQ_LAYER1 10
#define MHIQ_LAYER2 11
#define MHIQ_LAYER3 12

#define MHIQ_VARIABLE_BITRATE 20
#define MHIQ_JOINT_STEREO     21

#define MHIQ_BASS_CONTROL      30
#define MHIQ_TREBLE_CONTROL    31
#define MHIQ_MID_CONTROL       32
#define MHIQ_PREFACTOR_CONTROL 33
#define MHIQ_5_BAND_EQ         34
#define MHIQ_10_BAND_EQ        35

#define MHIQ_VOLUME_CONTROL  40
#define MHIQ_PANNING_CONTROL 41
#define MHIQ_CROSSMIXING     42

/* MHI decoder parameters (MHISetParam) */
#define MHIP_VOLUME      0 /* 0=muted .. 100=0dB */
#define MHIP_PANNING     1 /* 0=left .. 50=center .. 100=right */
#define MHIP_CROSSMIXING 2 /* 0=stereo .. 100=mono */
#define MHIP_BASS        3 /* 0=max.cut .. 50=unity .. 100=max.boost */
#define MHIP_MID         4
#define MHIP_TREBLE      5
#define MHIP_PREFACTOR   6
#define MHIP_MIDBASS     7
#define MHIP_MIDHIGH     8
#define MHIP_BAND1       9  /* 32 Hz */
#define MHIP_BAND2 MHIP_BASS /* 64 Hz */
#define MHIP_BAND3       10 /* 125 Hz */
#define MHIP_BAND4 MHIP_MIDBASS /* 250 Hz */
#define MHIP_BAND5       11 /* 500 Hz */
#define MHIP_BAND6 MHIP_MID /* 1 kHz */
#define MHIP_BAND7       12 /* 2 kHz */
#define MHIP_BAND8 MHIP_MIDHIGH /* 4 kHz */
#define MHIP_BAND9       13 /* 8 kHz */
#define MHIP_BAND10 MHIP_TREBLE /* 16 kHz */

#endif /* MHI_ABI_H */
