/*
 * amiga-mini.h - the slice of the AmigaOS SDK retro32term.c needs, self
 * contained so the terminal builds with a bare m68k-amigaos-gcc even when
 * no (or a broken) NDK header set is installed. Struct layouts follow the
 * published AmigaOS ABI (RKM/NDK 3.x, 2-byte alignment, which is the m68k
 * ABI this compiler targets); library calls go through the NDK's standard
 * LPx inline macros (<inline/macros.h>, shipped with the bebbo toolchain),
 * with LVO offsets taken from the matching inline headers.
 */

#ifndef AMIGA_MINI_H
#define AMIGA_MINI_H

/* --- basic types ------------------------------------------------------- */

typedef unsigned char UBYTE;
typedef signed char BYTE;
typedef unsigned short UWORD;
typedef signed short WORD;
typedef unsigned long ULONG;
typedef signed long LONG;
typedef void *APTR;
typedef short BOOL;
typedef const char *CONST_STRPTR;
typedef UBYTE *PLANEPTR;

#define TRUE 1
#define FALSE 0
#ifndef NULL
#define NULL ((void *)0)
#endif

/* --- exec structures ---------------------------------------------------- */

struct Node {
    struct Node *ln_Succ;
    struct Node *ln_Pred;
    UBYTE ln_Type;
    BYTE ln_Pri;
    char *ln_Name;
};

struct List {
    struct Node *lh_Head;
    struct Node *lh_Tail;
    struct Node *lh_TailPred;
    UBYTE lh_Type;
    UBYTE l_pad;
};

struct Library {
    struct Node lib_Node;
    UBYTE lib_Flags;
    UBYTE lib_pad;
    UWORD lib_NegSize;
    UWORD lib_PosSize;
    UWORD lib_Version; /* offset 20 */
    UWORD lib_Revision;
    APTR lib_IdString;
    ULONG lib_Sum;
    UWORD lib_OpenCnt;
};

struct MsgPort {
    struct Node mp_Node;
    UBYTE mp_Flags;
    UBYTE mp_SigBit; /* offset 15 */
    APTR mp_SigTask;
    struct List mp_MsgList;
};

struct Message {
    struct Node mn_Node;
    struct MsgPort *mn_ReplyPort;
    UWORD mn_Length;
};

struct IORequest {
    struct Message io_Message;
    APTR io_Device;
    APTR io_Unit;
    UWORD io_Command;
    UBYTE io_Flags;
    BYTE io_Error;
};

struct IOStdReq {
    struct Message io_Message;
    APTR io_Device;
    APTR io_Unit;
    UWORD io_Command;
    UBYTE io_Flags;
    BYTE io_Error;
    ULONG io_Actual;
    ULONG io_Length;
    APTR io_Data;
    ULONG io_Offset;
};

/* devices/serial.h */
struct IOTArray {
    ULONG TermArray0;
    ULONG TermArray1;
};

struct IOExtSer {
    struct IOStdReq IOSer;
    ULONG io_CtlChar;
    ULONG io_RBufLen;
    ULONG io_ExtFlags;
    ULONG io_Baud;
    ULONG io_BrkTime;
    struct IOTArray io_TermArray;
    UBYTE io_ReadLen;
    UBYTE io_WriteLen;
    UBYTE io_StopBits;
    UBYTE io_SerFlags;
    UWORD io_Status;
};

struct Task; /* opaque */
struct Resident; /* opaque */

#define NT_MSGPORT 4
#define NT_MESSAGE 5

#define PA_SIGNAL 0

#define SDCMD_QUERY 9
#define SDCMD_SETPARAMS 11
#define SERF_XDISABLED (1 << 7)
#define SERF_SHARED (1 << 5)

/* Unit -1 opens console.device in library mode: no window, no unit task,
 * just the device function vectors (RawKeyConvert). */
#define CONU_LIBRARY 0xFFFFFFFFUL

#define SIGBREAKF_CTRL_C (1UL << 12)

/* --- Paula serial hardware ----------------------------------------------- */

#define CUSTOM_SERDATR (*(volatile UWORD *)0xDFF018)
#define CUSTOM_SERDAT (*(volatile UWORD *)0xDFF030)
#define CUSTOM_SERPER (*(volatile UWORD *)0xDFF032)
#define CUSTOM_INTENAR (*(volatile UWORD *)0xDFF01C)
#define CUSTOM_INTENA (*(volatile UWORD *)0xDFF09A)
#define CUSTOM_INTREQ (*(volatile UWORD *)0xDFF09C)
#define CUSTOM_DMACON (*(volatile UWORD *)0xDFF096)

#define DMAF_BLITHOG 0x0400

#define SERDATR_OVRUN (1 << 15) /* receive overrun; cleared by RBF ack */
#define SERDATR_RBF (1 << 14)   /* receive buffer full */
#define SERDATR_TBE (1 << 13)   /* transmit buffer empty */

#define INTF_SETCLR 0x8000
#define INTF_INTEN 0x4000
#define INTF_DSKSYN 0x1000
#define INTF_RBF 0x0800
#define INTF_EXTER 0x2000
#define INTF_PORTS 0x0008

/* VHPOSR: vertical beam position (low 8 bits) in the high byte. One
 * raster line is 63.5 us; counting its transitions gives a delay that
 * needs no timer hardware. */
#define CUSTOM_VHPOSR (*(volatile UWORD *)0xDFF006)

/* --- CIA-A (keyboard) ---------------------------------------------------- */

#define CIAA_SDR (*(volatile UBYTE *)0xBFEC01) /* keyboard shift register */
#define CIAA_ICR (*(volatile UBYTE *)0xBFED01) /* int flags; read clears */
#define CIAA_CRA (*(volatile UBYTE *)0xBFEE01)

#define CIAICRF_SP 0x08     /* serial-port (keyboard byte) int flag */
#define CIACRAF_SPMODE 0x40 /* SP line output mode: drives KDAT low */

/* --- CIA-B (RS-232 control lines) ---------------------------------------- */

#define CIAB_PRA (*(volatile UBYTE *)0xBFD000)
#define CIAB_DDRA (*(volatile UBYTE *)0xBFD200)

/* Port-A bits 7:6 drive the serial connector through inverting 1488
 * drivers, so a low CIA pin is an asserted RS-232 line. */
#define CIAF_COMDTR 0x80 /* /DTR: low = terminal ready */
#define CIAF_COMRTS 0x40 /* /RTS: low = request to send */

/* --- input events (devices/inputevent.h) --------------------------------- */

#define IECLASS_RAWKEY 0x01
#define IECODE_UP_PREFIX 0x80

struct InputEvent {
    struct InputEvent *ie_NextEvent;
    UBYTE ie_Class;
    UBYTE ie_SubClass;
    UWORD ie_Code;
    UWORD ie_Qualifier;
    ULONG ie_EventAddress; /* union ie_position; zeroed here */
    ULONG ie_Seconds;      /* struct timeval ie_TimeStamp */
    ULONG ie_Micros;
};

struct KeyMap; /* opaque; passing NULL selects the default keymap */

/* --- utility tags --------------------------------------------------------- */

struct TagItem {
    ULONG ti_Tag;
    ULONG ti_Data;
};

#define TAG_DONE 0UL
#define TAG_USER 0x80000000UL

/* --- intuition / graphics ------------------------------------------------ */

struct Window; /* opaque; the one field used is reached via WINDOW_USERPORT */

/* intuition/intuition.h: Window.UserPort, the IDCMP message port
 * intuition creates when a window opens with IDCMPFlags set. */
#define WINDOW_USERPORT(w) (*(struct MsgPort **)((UBYTE *)(w) + 86))

/* graphics/gfx.h */
struct BitMap {
    UWORD BytesPerRow;
    UWORD Rows;
    UBYTE Flags;
    UBYTE Depth;
    UWORD pad;
    PLANEPTR Planes[8];
};

/* graphics/view.h: needed only for its size inside struct Screen. */
struct ViewPort {
    struct ViewPort *Next;
    APTR ColorMap;
    APTR DspIns;
    APTR SprIns;
    APTR ClrIns;
    APTR UCopIns;
    WORD DWidth, DHeight;
    WORD DxOffset, DyOffset;
    UWORD Modes;
    UBYTE SpritePriorities;
    UBYTE ExtendedModes;
    APTR RasInfo;
};

/* graphics/rastport.h: needed for its size and the BitMap pointer. */
struct RastPort {
    APTR Layer;
    struct BitMap *BitMap;
    UWORD *AreaPtrn;
    APTR TmpRas;
    APTR AreaInfo;
    APTR GelsInfo;
    UBYTE Mask;
    BYTE FgPen, BgPen, AOlPen;
    BYTE DrawMode, AreaPtSz, linpatcnt, dummy;
    UWORD Flags;
    UWORD LinePtrn;
    WORD cp_x, cp_y;
    UBYTE minterms[8];
    WORD PenWidth, PenHeight;
    APTR Font;
    UBYTE AlgoStyle, TxFlags;
    UWORD TxHeight, TxWidth, TxBaseline;
    WORD TxSpacing;
    APTR *RP_User;
    ULONG longreserved[2];
    UWORD wordreserved[7];
    UBYTE reserved[8];
};

/* intuition/screens.h, truncated after the members this program reads
 * (RastPort at offset 84, BitMap at 184 per the published ABI); never
 * used by value, so the missing tail is harmless. */
struct Screen {
    struct Screen *NextScreen;
    struct Window *FirstWindow;
    WORD LeftEdge, TopEdge;
    WORD Width, Height;
    WORD MouseY, MouseX;
    UWORD Flags;
    UBYTE *Title;
    UBYTE *DefaultTitle;
    BYTE BarHeight, BarVBorder, BarHBorder, MenuVBorder, MenuHBorder;
    BYTE WBorTop, WBorLeft, WBorRight, WBorBottom;
    struct TextAttr *Font;
    struct ViewPort ViewPort;
    struct RastPort RastPort;
    struct BitMap BitMap;
};

/* intuition/intuition.h: SA_Colors entry; 4-bit gun values, terminated by
 * ColorIndex = -1. */
struct ColorSpec {
    WORD ColorIndex;
    UWORD Red;
    UWORD Green;
    UWORD Blue;
};

/* graphics/text.h: font request, for NewScreen.Font. */
struct TextAttr {
    const char *ta_Name;
    UWORD ta_YSize;
    UBYTE ta_Style;
    UBYTE ta_Flags;
};

/* graphics/text.h: opened font, for pulling glyph bitmaps out of the
 * ROM Topaz. tf_CharData is a wide bitmap tf_Modulo bytes per row;
 * tf_CharLoc holds one ULONG per glyph, bit offset in the high word
 * and bit width in the low. Truncated after the members read here. */
struct TextFont {
    struct Message tf_Message;
    UWORD tf_YSize;
    UBYTE tf_Style;
    UBYTE tf_Flags;
    UWORD tf_XSize;
    UWORD tf_Baseline;
    UWORD tf_BoldSmear;
    UWORD tf_Accessors;
    UBYTE tf_LoChar;
    UBYTE tf_HiChar;
    APTR tf_CharData;
    UWORD tf_Modulo;
    APTR tf_CharLoc;
};

/* intuition/intuition.h: IDCMP message, for RAWKEY input. */
struct IntuiMessage {
    struct Message ExecMessage;
    ULONG Class;
    UWORD Code;
    UWORD Qualifier;
    APTR IAddress;
    WORD MouseX, MouseY;
    ULONG Seconds, Micros;
    struct Window *IDCMPWindow;
    struct IntuiMessage *SpecialLink;
};

#define IDCMP_RAWKEY 0x00000400UL

/* intuition/screens.h screen tags (V36+). */
#define SA_Dummy (TAG_USER + 32)
#define SA_Width (SA_Dummy + 3)
#define SA_Height (SA_Dummy + 4)
#define SA_Depth (SA_Dummy + 5)
#define SA_Colors (SA_Dummy + 9)
#define SA_Type (SA_Dummy + 13)
#define SA_ShowTitle (SA_Dummy + 22)
#define SA_Quiet (SA_Dummy + 24)

struct NewScreen {
    WORD LeftEdge, TopEdge, Width, Height, Depth;
    UBYTE DetailPen, BlockPen;
    UWORD ViewModes;
    UWORD Type;
    APTR Font;
    UBYTE *DefaultTitle;
    APTR Gadgets;
    APTR CustomBitMap;
};

struct NewWindow {
    WORD LeftEdge, TopEdge, Width, Height;
    UBYTE DetailPen, BlockPen;
    ULONG IDCMPFlags;
    ULONG Flags;
    APTR FirstGadget;
    APTR CheckMark;
    UBYTE *Title;
    struct Screen *Screen;
    APTR BitMap;
    WORD MinWidth, MinHeight;
    UWORD MaxWidth, MaxHeight;
    UWORD Type;
};

#define HIRES 0x8000
#define CUSTOMSCREEN 0x000F
#define SCREENQUIET 0x0100

#define WFLG_SIMPLE_REFRESH 0x00000040UL
#define WFLG_BACKDROP 0x00000100UL
#define WFLG_BORDERLESS 0x00000800UL
#define WFLG_ACTIVATE 0x00001000UL
#define WFLG_RMBTRAP 0x00010000UL

/* --- library bases and calls --------------------------------------------- */

#define SysBase (*(struct Library **)4UL)
extern struct Library *IntuitionBase;
extern struct Library *GfxBase;

/* <inline/stubs.h> only pulls NDK headers and forward-declares structs;
 * everything this program needs is declared above, so satisfy its guard
 * instead of including it. */
#define __INLINE_STUB_H
#include <inline/macros.h>

/* exec.library (offsets from inline/exec.h) */
#define Forbid() \
    LP0NR(0x84, Forbid, , SysBase)
#define Permit() \
    LP0NR(0x8a, Permit, , SysBase)
#define GetMsg(port) \
    LP1(0x174, struct Message *, GetMsg, struct MsgPort *, port, a0, , SysBase)
#define ReplyMsg(message) \
    LP1NR(0x17a, ReplyMsg, struct Message *, message, a1, , SysBase)
#define OpenLibrary(name, ver) \
    LP2(0x228, struct Library *, OpenLibrary, CONST_STRPTR, name, a1, ULONG, ver, d0, , SysBase)
#define CloseLibrary(lib) \
    LP1NR(0x19e, CloseLibrary, struct Library *, lib, a1, , SysBase)
#define OpenDevice(name, unit, req, flags) \
    LP4(0x1bc, BYTE, OpenDevice, CONST_STRPTR, name, a0, ULONG, unit, d0, \
        struct IORequest *, req, a1, ULONG, flags, d1, , SysBase)
#define CloseDevice(req) \
    LP1NR(0x1c2, CloseDevice, struct IORequest *, req, a1, , SysBase)
#define DoIO(req) \
    LP1(0x1c8, BYTE, DoIO, struct IORequest *, req, a1, , SysBase)
#define SendIO(req) \
    LP1NR(0x1ce, SendIO, struct IORequest *, req, a1, , SysBase)
#define CheckIO(req) \
    LP1(0x1d4, struct IORequest *, CheckIO, struct IORequest *, req, a1, , SysBase)
#define WaitIO(req) \
    LP1(0x1da, BYTE, WaitIO, struct IORequest *, req, a1, , SysBase)
#define AbortIO(req) \
    LP1NR(0x1e0, AbortIO, struct IORequest *, req, a1, , SysBase)
#define FindName(list, name) \
    LP2(0x114, struct Node *, FindName, struct List *, list, a0, CONST_STRPTR, name, a1, , SysBase)
#define FindResident(name) \
    LP1(0x60, struct Resident *, FindResident, CONST_STRPTR, name, a1, , SysBase)
#define SetFunction(lib, offset, func) \
    LP3(0x1a4, APTR, SetFunction, struct Library *, lib, a1, LONG, offset, a0, \
        APTR, func, d0, , SysBase)
#define FindTask(name) \
    LP1(0x126, struct Task *, FindTask, CONST_STRPTR, name, a1, , SysBase)
#define SetSignal(newSignals, signalSet) \
    LP2(0x132, ULONG, SetSignal, ULONG, newSignals, d0, ULONG, signalSet, d1, , SysBase)
#define AllocSignal(num) \
    LP1(0x14a, BYTE, AllocSignal, LONG, num, d0, , SysBase)
#define FreeSignal(num) \
    LP1NR(0x150, FreeSignal, LONG, num, d0, , SysBase)

#define AllocMem(size, reqs) \
    LP2(0xc6, APTR, AllocMem, ULONG, size, d0, ULONG, reqs, d1, , SysBase)
#define FreeMem(mem, size) \
    LP2NR(0xd2, FreeMem, APTR, mem, a1, ULONG, size, d0, , SysBase)

#define MEMF_CHIP 0x2UL
#define MEMF_CLEAR 0x10000UL

/* intuition.library (offsets from inline/intuition.h) */
#define OpenScreen(ns) \
    LP1(0xc6, struct Screen *, OpenScreen, struct NewScreen *, ns, a0, , IntuitionBase)
#define OpenScreenTagList(ns, tags) \
    LP2(0x264, struct Screen *, OpenScreenTagList, struct NewScreen *, ns, a0, \
        struct TagItem *, tags, a1, , IntuitionBase)
#define CloseScreen(s) \
    LP1(0x42, BOOL, CloseScreen, struct Screen *, s, a0, , IntuitionBase)
#define OpenWindow(nw) \
    LP1(0xcc, struct Window *, OpenWindow, struct NewWindow *, nw, a0, , IntuitionBase)
#define CloseWindow(w) \
    LP1NR(0x48, CloseWindow, struct Window *, w, a0, , IntuitionBase)
#define ViewPortAddress(w) \
    LP1(0x12c, struct ViewPort *, ViewPortAddress, struct Window *, w, a0, , IntuitionBase)
#define SetPointer(w, p, h, wd, x, y) \
    LP6NR(0x10e, SetPointer, struct Window *, w, a0, UWORD *, p, a1, LONG, h, d0, \
          LONG, wd, d1, LONG, x, d2, LONG, y, d3, , IntuitionBase)
#define ClearPointer(w) \
    LP1NR(0x3c, ClearPointer, struct Window *, w, a0, , IntuitionBase)
#define ShowTitle(s, showit) \
    LP2NR(0x11a, ShowTitle, struct Screen *, s, a0, BOOL, showit, d0, , IntuitionBase)

/* graphics.library (offsets from inline/graphics.h) */
#define LoadRGB4(vp, colors, count) \
    LP3NR(0xc0, LoadRGB4, struct ViewPort *, vp, a0, const UWORD *, colors, a1, \
          WORD, count, d0, , GfxBase)
#define BltBitMap(srcBitMap, xSrc, ySrc, destBitMap, xDest, yDest, xSize, ySize, minterm, mask, tempA) \
    LP11(0x1e, LONG, BltBitMap, struct BitMap *, srcBitMap, a0, LONG, xSrc, d0, \
         LONG, ySrc, d1, struct BitMap *, destBitMap, a1, LONG, xDest, d2, \
         LONG, yDest, d3, LONG, xSize, d4, LONG, ySize, d5, ULONG, minterm, d6, \
         ULONG, mask, d7, PLANEPTR, tempA, a2, , GfxBase)
#define WaitBlit() \
    LP0NR(0xe4, WaitBlit, , GfxBase)
#define OpenFont(textAttr) \
    LP1(0x48, struct TextFont *, OpenFont, struct TextAttr *, textAttr, a0, , GfxBase)
#define CloseFont(textFont) \
    LP1NR(0x4e, CloseFont, struct TextFont *, textFont, a1, , GfxBase)

/* console.device library-style call (offset from inline/console.h; the
 * base is the opened device, io_Device from any console IORequest).
 * Translates one IECLASS_RAWKEY InputEvent through the keymap without
 * involving any other task, so it is callable under Forbid(). */
extern struct Library *ConsoleDevice;
#define RawKeyConvert(events, buffer, length, keyMap) \
    LP4(0x30, LONG, RawKeyConvert, struct InputEvent *, events, a0, \
        UBYTE *, buffer, a1, LONG, length, d1, \
        struct KeyMap *, keyMap, a2, , ConsoleDevice)

/* --- additions for the cd32-probe (LVOs from the readable NDK inline
 * headers: exec.h, graphics.h, timer.h) ------------------------------- */

struct EClockVal {
    ULONG ev_hi;
    ULONG ev_lo;
};

#define Disable() \
    LP0NR(0x78, Disable, , SysBase)
#define Enable() \
    LP0NR(0x7e, Enable, , SysBase)
#define AvailMem(requirements) \
    LP1(0xd8, ULONG, AvailMem, ULONG, requirements, d1, , SysBase)
#define Wait(signalSet) \
    LP1(0x13e, ULONG, Wait, ULONG, signalSet, d0, , SysBase)
#define OldOpenLibrary(libName) \
    LP1(0x198, struct Library *, OldOpenLibrary, CONST_STRPTR, libName, a1, , SysBase)

#define GfxText(rp, string, count) \
    LP3(0x3c, LONG, Text, struct RastPort *, rp, a1, CONST_STRPTR, string, a0, \
        UWORD, count, d0, , GfxBase)
#define GfxMove(rp, x, y) \
    LP3NR(0xf0, Move, struct RastPort *, rp, a1, WORD, x, d0, WORD, y, d1, , GfxBase)
#define SetAPen(rp, pen) \
    LP2NR(0x156, SetAPen, struct RastPort *, rp, a1, UBYTE, pen, d0, , GfxBase)

/* timer.device: ReadEClock through the opened device base. */
extern struct Library *TimerBase;
#define ReadEClock(dest) \
    LP1(0x3c, ULONG, ReadEClock, struct EClockVal *, dest, a0, , TimerBase)

#endif /* AMIGA_MINI_H */
