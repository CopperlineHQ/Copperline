/* SPDX-License-Identifier: CC0-1.0 */
/* Copperline project template: one bitplane, Copper colours, a blitter bob,
 * and a vertical-blank interrupt. Press the left mouse button to exit. */

#include <exec/interrupts.h>
#include <exec/memory.h>
#include <exec/types.h>
#include <graphics/gfxbase.h>
#include <hardware/intbits.h>
#include <proto/exec.h>
#include <proto/graphics.h>

#include "uaelib.h"

extern struct ExecBase *SysBase;

#define CUSTOM_WORD(offset) (*(volatile UWORD *)(0xdff000u + (offset)))
#define CIAA_PRA (*(volatile UBYTE *)0xbfe001u)

enum {
    SCREEN_BYTES = 40 * 256,
    COPPER_WORDS = 18,
};

struct GfxBase *GfxBase;
static struct View *saved_view;
static UWORD saved_dma;
static UWORD saved_intena;
static UWORD *screen;
static UWORD *bob;
static UWORD *copper;
static struct Interrupt vblank;
static volatile ULONG frames;

static void set_pointer(UWORD high_offset, const void *pointer)
{
    ULONG address = (ULONG)pointer;
    CUSTOM_WORD(high_offset) = (UWORD)(address >> 16);
    CUSTOM_WORD(high_offset + 2) = (UWORD)address;
}

static void wait_blitter(void)
{
    while (CUSTOM_WORD(0x002) & 0x4000)
        ;
}

static void draw_bob(UWORD x, UWORD y)
{
    UBYTE *destination = (UBYTE *)screen + (ULONG)y * 40 + (x >> 3);
    wait_blitter();
    CUSTOM_WORD(0x040) = 0x09f0; /* A -> D, copy minterm. */
    CUSTOM_WORD(0x042) = 0;
    CUSTOM_WORD(0x044) = 0xffff;
    CUSTOM_WORD(0x046) = 0xffff;
    set_pointer(0x050, bob);
    set_pointer(0x054, destination);
    CUSTOM_WORD(0x064) = 0;
    CUSTOM_WORD(0x066) = 38;
    CUSTOM_WORD(0x058) = (16u << 6) | 1u;
}

static ULONG vblank_handler(void)
{
    ++frames;
    if ((frames & 3u) == 0)
        draw_bob((UWORD)((frames >> 2) % 19u) * 16u, 112);
    CUSTOM_WORD(0x09c) = 0x0020;
    CUSTOM_WORD(0x09c) = 0x0020;
    return 0;
}

static void take_system(void)
{
    Forbid();
    Disable();
    OwnBlitter();
    WaitBlit();
    saved_view = GfxBase->ActiView;
    LoadView(0);
    WaitTOF();
    WaitTOF();
    saved_dma = CUSTOM_WORD(0x002);
    saved_intena = CUSTOM_WORD(0x01c);
    CUSTOM_WORD(0x096) = 0x7fff;
    CUSTOM_WORD(0x09a) = 0x7fff;
    CUSTOM_WORD(0x09c) = 0x7fff;
    Enable();
}

static void free_system(void)
{
    RemIntServer(INTB_VERTB, &vblank);
    wait_blitter();
    CUSTOM_WORD(0x096) = 0x7fff;
    CUSTOM_WORD(0x09a) = 0x7fff;
    CUSTOM_WORD(0x096) = 0x8000 | (saved_dma & 0x7fff);
    CUSTOM_WORD(0x09a) = 0x8000 | (saved_intena & 0x7fff);
    LoadView(saved_view);
    WaitTOF();
    WaitTOF();
    DisownBlitter();
    Permit();
}

static void build_display(void)
{
    ULONG screen_address = (ULONG)screen;
    UWORD words[COPPER_WORDS] = {
        0x008e, 0x2c81, 0x0090, 0x2cc1,
        0x0092, 0x0038, 0x0094, 0x00d0,
        0x0100, 0x1200,
        0x00e0, (UWORD)(screen_address >> 16),
        0x00e2, (UWORD)screen_address,
        0x0180, 0x0123,
        0xffff, 0xfffe,
    };
    ULONG i;
    for (i = 0; i < COPPER_WORDS; ++i)
        copper[i] = words[i];
    for (i = 0; i < 16; ++i)
        bob[i] = (UWORD)(0x8001u | (0x4002u >> (i & 7u)));
    set_pointer(0x080, copper);
    CUSTOM_WORD(0x088) = 0;
}

int main(void)
{
    SysBase = *(struct ExecBase **)4;
    GfxBase = (struct GfxBase *)OpenLibrary("graphics.library", 33);
    if (!GfxBase)
        return 20;
    screen = AllocMem(SCREEN_BYTES, MEMF_CHIP | MEMF_CLEAR);
    bob = AllocMem(16 * sizeof(UWORD), MEMF_CHIP | MEMF_CLEAR);
    copper = AllocMem(COPPER_WORDS * sizeof(UWORD), MEMF_CHIP | MEMF_CLEAR);
    if (!screen || !bob || !copper)
        goto cleanup;

    take_system();
    build_display();
    debug_register_bitmap(screen, "screen", 320, 256, 1, 0);
    debug_register_copperlist(copper, "copper", COPPER_WORDS * 2, 0);

    vblank.is_Node.ln_Type = NT_INTERRUPT;
    vblank.is_Node.ln_Pri = 0;
    vblank.is_Node.ln_Name = "Copperline template VBL";
    vblank.is_Data = 0;
    vblank.is_Code = (VOID (*)())vblank_handler;
    AddIntServer(INTB_VERTB, &vblank);
    CUSTOM_WORD(0x09a) = 0xc020; /* SET, master interrupt, VBL. */
    CUSTOM_WORD(0x096) = 0x83c0; /* SET, master, bitplane, Copper, blitter. */

    KPrintF("Copperline template running; click left mouse to exit.\n");
    while (CIAA_PRA & 0x40)
        WaitTOF();
    debug_unregister(copper);
    debug_unregister(screen);
    free_system();

cleanup:
    if (copper)
        FreeMem(copper, COPPER_WORDS * sizeof(UWORD));
    if (bob)
        FreeMem(bob, 16 * sizeof(UWORD));
    if (screen)
        FreeMem(screen, SCREEN_BYTES);
    CloseLibrary((struct Library *)GfxBase);
    return 0;
}
