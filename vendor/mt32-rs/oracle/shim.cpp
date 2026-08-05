/* SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * The oracle: a thin C surface over the reference C++ engine, shaped for
 * the differential tests and nothing else. The tests open both engines on
 * the same ROMs, feed them the same MIDI, and compare what comes out.
 */

#include <cstring>

#include "File.h"
#include "LA32Ramp.h"
#include "LA32WaveGenerator.h"
#include "Tables.h"
#include "ROMInfo.h"
#include "Synth.h"

using namespace MT32Emu;

/* The pitch-envelope jitter source, standing in for libc rand() via a
 * compile-time rename (see build.rs): the C standard's example LCG, chosen
 * because it is trivial to mirror bit for bit in Rust. Seeded afresh by
 * every open, so a synth's output depends on nothing outside it. One
 * engine per thread: thread-local state keeps tests that render on
 * parallel harness threads out of each other's streams. */
static thread_local unsigned long long oracle_rand_state = 1;

extern "C" int mt32_oracle_rand() {
    oracle_rand_state = oracle_rand_state * 1103515245 + 12345;
    return static_cast<int>((oracle_rand_state / 65536) % 32768);
}

namespace {

struct Oracle {
    ArrayFile *controlFile;
    ArrayFile *pcmFile;
    const ROMImage *controlImage;
    const ROMImage *pcmImage;
    Synth *synth;
};

} // namespace

extern "C" {

/* Open a synth on in-memory ROM images. `analog_mode` takes the engine's
 * AnalogOutputMode values (0 digital-only, 1 coarse, 2 accurate,
 * 3 oversampled). Returns null if either image is not a recognised ROM or
 * the synth refuses to open. */
void *mt32_oracle_open(const unsigned char *control, size_t control_len,
                       const unsigned char *pcm, size_t pcm_len,
                       int analog_mode) {
    oracle_rand_state = 1;
    Oracle *o = new Oracle();
    o->controlFile = new ArrayFile(control, control_len);
    o->pcmFile = new ArrayFile(pcm, pcm_len);
    o->controlImage = ROMImage::makeROMImage(o->controlFile);
    o->pcmImage = ROMImage::makeROMImage(o->pcmFile);
    o->synth = new Synth();
    if (o->controlImage->getROMInfo() == NULL || o->pcmImage->getROMInfo() == NULL
        || !o->synth->open(*o->controlImage, *o->pcmImage,
                           static_cast<AnalogOutputMode>(analog_mode))) {
        delete o->synth;
        ROMImage::freeROMImage(o->controlImage);
        ROMImage::freeROMImage(o->pcmImage);
        delete o->controlFile;
        delete o->pcmFile;
        delete o;
        return NULL;
    }
    return o;
}

void mt32_oracle_close(void *handle) {
    Oracle *o = static_cast<Oracle *>(handle);
    o->synth->close();
    delete o->synth;
    ROMImage::freeROMImage(o->controlImage);
    ROMImage::freeROMImage(o->pcmImage);
    delete o->controlFile;
    delete o->pcmFile;
    delete o;
}

/* The rate the synth renders at in the opened analog mode. */
unsigned int mt32_oracle_sample_rate(const void *handle) {
    return static_cast<const Oracle *>(handle)->synth->getStereoOutputSampleRate();
}

/* One short MIDI message, low byte first, played immediately. */
void mt32_oracle_play_msg(void *handle, unsigned int msg) {
    static_cast<Oracle *>(handle)->synth->playMsgNow(msg);
}

/* One SysEx message, F0..F7 inclusive, played immediately. */
void mt32_oracle_play_sysex(void *handle, const unsigned char *sysex, unsigned int len) {
    static_cast<Oracle *>(handle)->synth->playSysexNow(sysex, len);
}

/* Render interleaved stereo, two Bit16s per frame. */
void mt32_oracle_render(void *handle, short *stream, unsigned int frames) {
    static_cast<Oracle *>(handle)->synth->render(stream, frames);
}

/* The LCD line (21 bytes: 20 characters and a terminator) and the MIDI
 * MESSAGE lamp, returned as the lamp state. */
int mt32_oracle_display(const void *handle, char *buffer21) {
    return static_cast<const Oracle *>(handle)->synth->getDisplayState(buffer21, false);
}

/* Read `len` bytes of the synth's addressable memory. `addr` is the
 * printed three-byte form the manual and SysEx use (0x100000 for the
 * system area); the engine wants the three 7-bit bytes packed flat, so it
 * is converted here. */
void mt32_oracle_read_memory(void *handle, unsigned int addr, unsigned int len,
                             unsigned char *out) {
    const unsigned int flat =
        ((addr & 0x7f0000) >> 2) | ((addr & 0x7f00) >> 1) | (addr & 0x7f);
    static_cast<Oracle *>(handle)->synth->readMemory(flat, len, out);
}

/* Identify one image as the engine does: 1 if it is a known FULL ROM,
 * with the short name copied into `name32`; 0 for anything else,
 * including the half- and interleaved-dump forms. */
int mt32_oracle_identify(const unsigned char *data, size_t len, char *name32) {
    ArrayFile file(data, len);
    const ROMImage *image = ROMImage::makeROMImage(&file);
    const ROMInfo *info = image->getROMInfo();
    int full = info != NULL && info->pairType == ROMInfo::Full;
    if (full) {
        strncpy(name32, info->shortName, 31);
        name32[31] = 0;
    }
    ROMImage::freeROMImage(image);
    return full;
}

/* The engine's constant tables, copied out for entry-by-entry
 * comparison: the four 8-bit curves, then the LA32's exponent and
 * log-sine tables, then the eight decay factors, packed in that order. */
void mt32_oracle_tables(unsigned char *curves559, unsigned short *exp512,
                        unsigned short *logsin512, unsigned char *decay8) {
    const Tables &t = Tables::getInstance();
    memcpy(curves559, t.levelToAmpSubtraction, 101);
    memcpy(curves559 + 101, t.envLogarithmicTime, 256);
    memcpy(curves559 + 357, t.masterVolToAmpSubtraction, 101);
    memcpy(curves559 + 458, t.pulseWidth100To255, 101);
    memcpy(exp512, t.exp9, 512 * sizeof(unsigned short));
    memcpy(logsin512, t.logsin9, 512 * sizeof(unsigned short));
    memcpy(decay8, t.resAmpDecayFactors, 8);
}

/* One LA32 ramp, for stepwise comparison. The ramp reads the exponent
 * table through a static pointer set at synth open; initialise it here so
 * a ramp stands alone. */
void *mt32_oracle_ramp_new() {
    LA32Ramp::initTables(Tables::getInstance());
    return new LA32Ramp();
}

void mt32_oracle_ramp_free(void *ramp) {
    delete static_cast<LA32Ramp *>(ramp);
}

void mt32_oracle_ramp_start(void *ramp, unsigned char target, unsigned char increment) {
    static_cast<LA32Ramp *>(ramp)->startRamp(target, increment);
}

/* One step: the value, with the interrupt flag packed into bit 32. */
unsigned long long mt32_oracle_ramp_next(void *ramp) {
    LA32Ramp *r = static_cast<LA32Ramp *>(ramp);
    unsigned long long value = r->nextValue();
    if (r->checkInterrupt()) value |= 1ULL << 32;
    return value;
}

/* One integer partial pair, the unit that turns parameters into linear
 * samples. Master/slave selected by `which` (0 master, 1 slave). */
void *mt32_oracle_pair_new() {
    LA32IntPartialPair::initTables(Tables::getInstance());
    return new LA32IntPartialPair();
}

void mt32_oracle_pair_free(void *pair) {
    delete static_cast<LA32IntPartialPair *>(pair);
}

void mt32_oracle_pair_init(void *pair, int ring_modulated, int mixed) {
    static_cast<LA32IntPartialPair *>(pair)->init(ring_modulated != 0, mixed != 0);
}

void mt32_oracle_pair_init_synth(void *pair, int which, int sawtooth,
                                 unsigned char pulse_width, unsigned char resonance) {
    static_cast<LA32IntPartialPair *>(pair)->initSynth(
        which == 0 ? LA32PartialPair::MASTER : LA32PartialPair::SLAVE,
        sawtooth != 0, pulse_width, resonance);
}

void mt32_oracle_pair_init_pcm(void *pair, int which, const short *pcm_wave,
                               unsigned int len, int looped) {
    static_cast<LA32IntPartialPair *>(pair)->initPCM(
        which == 0 ? LA32PartialPair::MASTER : LA32PartialPair::SLAVE,
        pcm_wave, len, looped != 0);
}

void mt32_oracle_pair_generate(void *pair, int which, unsigned int amp,
                               unsigned short pitch, unsigned int cutoff) {
    static_cast<LA32IntPartialPair *>(pair)->generateNextSample(
        which == 0 ? LA32PartialPair::MASTER : LA32PartialPair::SLAVE, amp, pitch, cutoff);
}

short mt32_oracle_pair_next_out(void *pair) {
    return static_cast<LA32IntPartialPair *>(pair)->nextOutSample();
}

void mt32_oracle_pair_deactivate(void *pair, int which) {
    static_cast<LA32IntPartialPair *>(pair)->deactivate(
        which == 0 ? LA32PartialPair::MASTER : LA32PartialPair::SLAVE);
}

int mt32_oracle_pair_is_active(void *pair, int which) {
    return static_cast<LA32IntPartialPair *>(pair)->isActive(
        which == 0 ? LA32PartialPair::MASTER : LA32PartialPair::SLAVE) ? 1 : 0;
}

} /* extern "C" */

/* The Boss reverb model standing alone, for differencing the port before
 * it joins the synth: the integer renderer's model, opened at creation,
 * driven by the same parameter writes and sample blocks. */

#include "BReverbModel.h"

extern "C" {

void *mt32_oracle_reverb_new(int mode, int mt32_compatible) {
    BReverbModel *model = BReverbModel::createBReverbModel(
        static_cast<ReverbMode>(mode), mt32_compatible != 0,
        RendererType_BIT16S);
    model->open();
    return model;
}

void mt32_oracle_reverb_free(void *model) {
    delete static_cast<BReverbModel *>(model);
}

void mt32_oracle_reverb_set(void *model, unsigned char time,
                            unsigned char level) {
    static_cast<BReverbModel *>(model)->setParameters(time, level);
}

void mt32_oracle_reverb_process(void *model, const short *in_left,
                                const short *in_right, short *out_left,
                                short *out_right, unsigned int frames) {
    static_cast<BReverbModel *>(model)->process(in_left, in_right, out_left,
                                                out_right, frames);
}

int mt32_oracle_reverb_active(const void *model) {
    return static_cast<const BReverbModel *>(model)->isActive() ? 1 : 0;
}

} // extern "C"

/* The MIDI stream parser standing alone: events collected into a tagged
 * log the differential reads back -- 1 + message for shorts, 2 + length
 * + bytes for SysEx, 3 + byte for Realtime, 4 for each dropped-data
 * note. Order preserved, which is half the behaviour. */

#include <vector>

#include "MidiStreamParser.h"

namespace {

struct CollectingParser : MidiReceiver, MidiReporter {
    MidiStreamParserImpl *impl;
    std::vector<unsigned char> log;

    void handleShortMessage(const Bit32u message) {
        log.push_back(1);
        for (int i = 0; i < 4; i++) log.push_back((message >> (8 * i)) & 0xFF);
    }

    void handleSysex(const Bit8u stream[], const Bit32u length) {
        log.push_back(2);
        for (int i = 0; i < 4; i++) log.push_back((length >> (8 * i)) & 0xFF);
        log.insert(log.end(), stream, stream + length);
    }

    void handleSystemRealtimeMessage(const Bit8u realtime) {
        log.push_back(3);
        log.push_back(realtime);
    }

    void printDebug(const char *) {
        log.push_back(4);
    }
};

} // namespace

extern "C" {

void *mt32_oracle_parser_new() {
    CollectingParser *p = new CollectingParser();
    p->impl = new MidiStreamParserImpl(*p, *p, SYSEX_BUFFER_SIZE);
    return p;
}

void mt32_oracle_parser_free(void *parser) {
    CollectingParser *p = static_cast<CollectingParser *>(parser);
    delete p->impl;
    delete p;
}

void mt32_oracle_parser_parse(void *parser, const unsigned char *stream,
                              unsigned int len) {
    static_cast<CollectingParser *>(parser)->impl->parseStream(stream, len);
}

/* Copy out and clear the event log; returns its length. If it exceeds
 * `cap`, nothing is copied or cleared and the needed length returns. */
unsigned int mt32_oracle_parser_take_log(void *parser, unsigned char *out,
                                         unsigned int cap) {
    CollectingParser *p = static_cast<CollectingParser *>(parser);
    unsigned int len = static_cast<unsigned int>(p->log.size());
    if (len <= cap) {
        memcpy(out, p->log.data(), len);
        p->log.clear();
    }
    return len;
}

} // extern "C"
