# ToasterCG titles keyed over Program (Toaster 4.2), driven from outside

Findings from source and binaries only. The emulator was not run, so nothing here is tested.

Sources and abbreviations:

* **SW** = `<vt-src>/full/switcher/SRC`.
* **OLDRX** = `<vt-src>/old/switcher/Pre2-4-94/src/arexxhandler.a`. The file has CR line endings; line numbers are after `tr '\r' '\n'`.
* **CG** = `<vt-src>/full/CG/src`.
* **ED** = `<ovt-src>/Edit/Edit/src`.
* **DOC** = `<installed>/NewTek/Programs/Documentation/Arexx`.
* **AA** = the 4.2 AA switcher with symbols (`<installed>/NewTek/Programs/Switcher.main.AA`, linked at $100000, symbols in `a symbol dump, not included`; disassemble with `vt/dve-tools/vtdis.py`).

The installed `switcher.main` and `switcher.main.aa` were checked with byte patterns for the same code where noted.

---------------------------------------------------------------------------

## 0. Short answer

* **There is no ARexx command that enters the CG.**
  * The 4.2 command table has `STTP` (enter Paint) and `STLW` (enter LightWave) but no `STCG`.
  * `GOCG` is a no-op.
  * `LDCG` only *loads* the CG.
* **The CG is entered by a second `FGC_SELECTK`/`FGC_SELECT` to the CG slice gadget once the CG is loaded.** Three things send it:
  * the **F9** key on the switcher window;
  * the project editor's **Programs → ToasterCG** popup;
  * an **EditSwit message `ES_ToasterCG`/`FGC_SELECT`** posted to `ToasterARexx.port`. A 176-byte helper is given in 1.4.
* **CG_AREXX** is a *function host*. It is serviced only inside the CG's edit loop.
  * While the CG is on screen the switcher is **blocked** and does not read its own port.
  * So `ToasterARexx.port` must **not** be ahead of `CG_AREXX` in the global ARexx library list. The Copperline link currently puts it there (section 2.1). This is a likely cause of "CG_AREXX never answers".
* **Making a title:**
  * `SET_PAGE` to an empty page, then `SET_PAGE('TYPE','K')`.
  * `LOADFONT`/`SET_CHAR`/`SET_BORD`/`SET_SHAD`, then `MAKELINE`.
  * `SET_LINE` SPOT/JUST to place it.
  * `SAVEPAGE(path)`. This writes a **Key Page crouton** (CrUD `KEYF`).
  * `EXIT()`.
* **Keying it:** back in the switcher, `CROUTONLOAD(path)` (project port) adds and selects the crouton, which loads it into the spare DV bank plus the alpha bitmap. `Switcher('AUTO')` fades it on, and again off. `TAKE` cuts it on and off.
* **Fallbacks without the CG:**
  * `Switcher('LRGB', iff24, bank)`, then an overlay luma key.
  * Build a `KEYF` crouton on the host from an `FMSV` framestore by adding two alpha planes (4.2).

---------------------------------------------------------------------------

## 1. Getting from the switcher into the CG and back

### 1.1 What the switcher's ARexx table has

The 4.2 table, as strings in the binaries, is in `switcher.main` at file offset $76338 and in `switcher.main.aa` at $2001c:

`AUTO TBAR TAKE ... CKTP LDTP DPTP STTP CKCG LDCG DPCG CKLW LDLW DPLW STLW ... GOCG GOFX GOLD GOSA KNUM CHGR DOEN UATO ...`

No `STCG` appears in any build. The handlers in the AA build:

| Cmd | AA addr | What it does |
|---|---|---|
| `CKCG` | $127f2e | Returns 1 if the CG slice FG is "on" (`FG_PTRIndex`). |
| `LDCG` | $127f6e | If the CG is not on, `HandlerMainBody` sends **FGC_SELECTK** to the CG slice FG; if it is already on, it does nothing. Same logic as OLDRX:2719-2729 and OLDRX:4689-4700. |
| `DPCG` | $127f8c | Sends FGC_UNLOAD. |
| `STTP` | $127f10 | Paint only: SELECTK when loaded = enter (OLDRX:2667-2677). There is no CG twin. |
| `GOCG` | $1280d4 | `bra HandlerGood` (no-op). DOC/SwitcherARexx.Doc:155-159 says "obsolete". |
| `PAGE` | $1278de | `bra HandlerBad` (removed). |
| `DOEN` | $1280ba | `DoEnterKey` (keypad Enter). Unrelated to the CG in 4.x. |

**Why LDCG never enters.** The CG slice handler (`CGSliceCode`, SW/CGSliceCode.a:223 table; `SelectK` 342; `SelectMainBody` 369-460) works like this:

* **CG not loaded:** it calls `LoadCGCode`. Then `cmp.l #FGC_SELECTK,4(sp) / bne .DoSelect` (CGSliceCode.a:392):
  * a key-select (SELECTK) **only loads**;
  * a mouse select (FGC_SELECT) loads *and* enters.
* **CG already loaded:** `.DoSelect` (400) sends FGC_SELECT to the CG process with `SendCGMessage`.
  * That is a PutMsg followed by WaitPort (CGSliceCode.a:1168-1210), so **the switcher task blocks until the CG exits**.
  * On return it calls `DitchKey`, `SelectStdEfx` and `ReDoDisplay` (449-452). Any key made inside the CG is dropped on return.

The same branch is in all builds: AA $1198b4, installed `switcher.main.aa` $1137c4, `switcher.main` $132390 (`0caf 0000 000b 0004 6606`).

So after `LDCG` (load), **one more SELECTK** enters the CG. None of the ARexx commands sends it, because LDCG skips when the CG is already loaded.

### 1.2 F9 on the switcher window

* SW/MBDispatch.a:3005 maps raw key **$58 (F9)** to `CGSelectRKH` (MBDispatch.a:1592-1617), which sends **FGC_SELECTK** to the CG slice FG. Checked in all three binaries: the table entry at index $58 points at the SELECTK routine, and F1-F7 are `MainRowRKH`.
* Pressing it twice therefore works: the first press loads and the second enters. After `LDCG`, one press enters.
* (F8 = $57 = `ToggleEditor`, which brings up the project editor if it is down.)
* **Catch:** raw keys go to the *active Intuition window*. When the project editor window is active, Edit handles F9 itself as "Info/Controls" (ED/handlecommon.c:1242-1245). Edit forwards only the keys it does not handle (`SendIntuiSwitcher`, handlecommon.c:1281-1290).
* So F9 reaches the switcher only after the switcher's own window has been made active, by a click on the switcher area outside the editor window. Pick an empty spot from a screenshot: a click on a switcher button would also fire that button.
* With Copperline: `input_key rawkey=0x58`.

### 1.3 Programs popup in the project editor (what the user found)

**What the popup does** (ED/graphichelp.c `HandlePrograms` 1429-1520):

* Items are `" "`, `LightWave`, `ToasterPaint`, `ToasterCG`, `ChromaFX`. The divider is trimmed when there are no custom items; the current item is 0 (1384-1399, 1448-1456).
* Choosing ToasterCG sets `global_LoadSliceCommand = ES_ToasterCG` (1500).
* The editor loop then runs `LoadSlice` (edit.c:1471-1475; graphichelp.c:1562-1580):
  * `SendSwitcherReply(ES_ToasterCG, FGC_LOAD)` (pecomm.a `SendSliceCommand` 5757: load → `FGC_SelectKCommand` if not loaded);
  * hides the editor windows (`MakeLayout(EW_EMPTY...)`, `SwitcherSwitch(TRUE)`);
  * `PutSwitcher(ES_ToasterCG, FGC_SELECT)`, which becomes SELECTK when loaded, i.e. enter.
* Holding Shift while choosing unloads the program instead.

**Where it is on screen.** Derived from layout code; check it on a screenshot.

* The project window opens at screen x=32, y=TopEdge+44 with TopEdge=0 (ED/editwindow.c:381-388; handlecommon.c:761).
* The button strip sits at window y = Grid->Height+5 = 205 (Switcher/TOP_SMALL layout, project.c:1236-1239, 1299-1322). Gadget widths come from ED/ImageInfo.c.
* Strip left to right: Delete 32 (56 wide), All 97 (35), Duplicate 132 (59), Tools 191 (59), Controls 251 (68), Play 328, PlayPart 369, Stop 411 (39), Save 459 (47), **Programs 515 (92×19)**, Views 608 (65).
* **"Programs" in screen coordinates: x 547-639, y 249-267, centre ≈ (593, 258).** It is the second control from the right, just left of "Views", on the strip under the project grid.

**How to work the popup** (ED/popup.c):

* The popup is a press-drag-release menu.
* Item 0 is centred on the gadget's centre (`y = y0 - yMargin - itemH/2`, popup.c:300-307).
* Item height = InterfaceLG font height 11 + textGap 2 = **13** (edit.c:362-364, EditWindow.h:76, popup.c:80-97, 267).
* So **ToasterCG (item 3) is ≈ 39 px below the button centre, about (593, 297).**
* Do: LMB down on the button, move down 39 px in several steps (it tracks MOUSEMOVE), then release. Any button event ends it (popup.c:540-640).
* `input_mouse_to` takes *screenshot* pixels, so convert after finding "Programs" in a capture.

### 1.4 ES message to `ToasterARexx.port` (no mouse; needs a tiny helper)

**How Edit drives the switcher.** Edit sends EditSwit messages to the port named `ToasterARexx.port` (ED/edit.c:992). The switcher tells them apart from RexxMsgs by `Cookie == $666` (OLDRX ProcessARexxMessage; editswit.h:296).

* `ES_ToasterCG` = **38** (checked in the AA dispatch table: `Hand_ToasterCG` sits at index 38 after `Hand_DUMMY`).
* `Hand_ToasterCG` → `SendSliceCommand(CGSLICEID, Data1)` (pecomm.a:5711-5718, 5757-5810).
* `FGC_LOAD` (0) loads; `FGC_SELECT` (2) enters if loaded (error 2 if not).
* ARexx cannot PutMsg a raw message (rexxsupport has no PutMsg), so a helper is needed.

**ESMessage layout** (offsets match the FMLD handler at AA $1278e2):

* struct Message (20 bytes), then `WORD Cookie`@20, `WORD Type`@22, `LONG Reply`@24, `LONG Error`@28, `LONG Data1`@32.

**The helper `EnterCG`.** Hand-assembled; the disassembly was checked with capstone; never run.

* It runs CreateMsgPort, Forbid, FindPort("ToasterARexx.port"), PutMsg, Permit, WaitPort, GetMsg and DeleteMsgPort.
* Its message is {Cookie $666, Type 38, Data1 2}.
* It is fully PC-relative (no relocs): one 140-byte code hunk, 176 bytes as a file. The hex of the whole file:

```
000003f30000000000000001000000000000000000000023000003e9000000232c7800044eaefd662e00674445fa00462547000e4eaeff7c43fa005e4eaefe7a4a8067222040224a4eaefe924eaeff7620474eaefe8020474eaefe8c20474eaefd6070004e754eaeff7620474eaefd6070144e75000000000000000005000000000000000000002406660026000000000000000000000002546f617374657241526578782e706f7274000000000003f2
```

**How to use it:**

* `Switcher('LDCG')` (synchronous load), then `address command 'Run >NIL: ToasterFiles:.copperline/EnterCG'`.
* Run it **detached**. The switcher replies only when the CG exits, because `SendCGMessage` blocks. The helper waits for that reply and then exits.
* It does not hide the editor windows the way LoadSlice does. The F9 path does not either, so this should be fine.

### 1.5 Leaving the CG

* `EXIT()` on CG_AREXX (CG/cgrexx.c:1596-1600) returns `REFRESH_EXIT`. The `DoCG` loop ends and `FreeToaster` puts the switcher back (CG/cg.a:931 onward). The switcher then replies, runs `DitchKey`, `SelectStdEfx` and `ReDoDisplay`, and starts serving its ARexx port again.
  * The link's `call EXIT()` should work.
  * If the ARexx keyword `EXIT` gets in the way, call `x = EXITCG()`. Matching uses only the first 4 characters (cgrexx.c:2004-2009).
* Keyboard: **Esc** (raw $45) → `_CaseEscape` (CG/newrender.a:2357-2380). Probably "exit to switcher"; not checked further.
* The CG stays loaded afterwards: CKCG=1 and its `CG_AREXX` port still exists. `DPCG` unloads it.

### 1.6 Knowing the CG is ready

* Inside the CG each entry runs `DoStartupMacro` immediately before `DoCG` (CG/cg.a:929-931).
* The macro comes from `CG_Support/CG-Config` `#Startup` = `REXX:CG/StartUp.rexx` (cg-config:23-24). The config is read relative to Toaster:Programs.
* With `REXX:` = `S:` only (S:Startup-Sequence:32) the file `S:CG/StartUp.rexx` does not exist and the macro silently fails. NewTek's installer assigns `REXX:` to `Toaster:Arexx` (Programs/Utilities/UN-assign undoes it); `Assign REXX: Toaster:Arexx ADD` in S:User-Startup makes it run `Toaster:Arexx/CG/Startup.Rexx`, which loads Benjamin-Book 35 and 40 (checked: the CG comes up on Benjamin-Book 40). `SaveState.Rexx` (Alt-F3) rewrites that file with the fonts loaded at the time.
* The macro calls CG functions from a process of its own, so it hangs if `ToasterARexx.port` or `PROJECT_REXX_PORT` is on the library list (2.1).
* The macro is sent RXCOMM|NOIO to the **`AREXX`** port, which launches it asynchronously (installed CG $22b34c; source cgrexx.c:1837-1883). The CG is therefore already in its loop when the macro runs.
* **Hook (optional, needs a new file in the guest):** create `S:CG/StartUp.rexx` that writes a flag such as `ToasterFiles:.copperline/cg-ready`. Then the host knows when it is safe to send CG commands.
  * The CG-Config Alt-F1…F10 macros point at `REXX:CG/...` too and are equally absent. The scripts themselves are in `Toaster:Arexx/CG`.

---------------------------------------------------------------------------

## 2. CG_AREXX: making a title page

### 2.1 How the host behaves (important)

* **It is a function host named `CG_AREXX`** (cgrexx.c:96, 1740-1750). Command messages get error 10 (`HandleARexxMess` 1949-1971).
* **It is served only inside `DoCG`** (CG/NewGadgets.c:296-345). It is not served while the CG is loaded but off screen, which is the observed behaviour.
* **Function name = 4-char command + 4-char object**, matched case-insensitively (cgrexx.c:143-246, 1973-2009):
  * commands `GET_ SET_ PICK MAKE KILL LOAD SAVE EXIT FEP_ REQ_ REND REXX MACR`;
  * objects `FONT CHAR BRUS LINE PAGE BOOK PICT RECT SHAD BORD BACK TEXT DRAW`;
  * attribute arguments matched on 4 chars: `FACE TOPR BOTR TYPE SIZE JUST PRIO SPOT VALU RGBA NEXT PREV FIRS(T) FILL TALL STAT LAST`.
  * An unknown command gets RC 5 with error 1, which means "not mine", so ARexx keeps searching.
* **Library-list hazard: the switcher host answers every function call.**
  * The switcher's handler ignores the function name and parses ARG1 as a 4-character command. Unknown commands get **RC 10, error 0** (OLDRX ProcessARexxMessage/`HandlerBad`), which stops ARexx's search instead of letting it continue.
  * A CG argument that happens to be a switcher word (for example `MAKELINE('TAKE')`) would even run that switcher command.
  * While the CG is up, the switcher task is blocked in `SendCGMessage`, so a call routed to it **hangs** until the CG exits.
  * The Copperline `wrap()` (src/control/toaster_link.rs) puts `ToasterARexx.port` and `PROJECT_REXX_PORT` on the *global* library list at priority -1 for each job and takes them off when the job ends; `S:Toaster-Project.rexx` likewise takes `PROJECT_REXX_PORT` off after loading the start-up project. (Earlier both stayed on the list at priority 0 after the first job, so every CG macro, the start-up macro included, hung.)
  * The reverse is also bad. If `CG_AREXX` ends up first while the CG is off screen, every `Switcher()` call hangs, because the CG port exists but nobody reads it.
  * NewTek's own StartToaster `remlib`s both Toaster hosts when it finishes (Programs/StartToaster, quit:), so the shipped CG scripts never met this.
  * **Rule for CG jobs:** `remlib('ToasterARexx.port')` and `remlib('PROJECT_REXX_PORT')`, then `addlib('CG_AREXX',0)`, then the work, then `remlib('CG_AREXX')`, then re-add the other two. Run jobs one at a time.
  * Check with `say SHOW('L')`, which shows the library list order.
* **Bugs in the CG's message loop** (NewGadgets.c:319-333):
  * A message is sent to `HandleARexxMess` only if the *last* `Wait()` signal had the rexx bit set.
  * On entry the loop starts with `signal=0` (`goto Proc`). A CG_AREXX message already queued before the CG came up (for example from an earlier hung job) is handed to `ProcessMessage` as if it were an IntuiMessage.
  * The same can happen when Intuition input and ARexx arrive in the same batch.
  * So: keep the mouse and keyboard still while sending CG commands, send them one at a time, and do not leave stalled CG calls queued before entering.
* **Calls must carry all their arguments.** `SET_CHAR/SHAD/BORD('RGBA'|'TOPR', r,g,b,a)` checks `args<4` but reads ARG5 (cgrexx.c:839-848, 1103-1111, 1190-1198). Always pass alpha. **255 = opaque** (NewTek's AttribScripts use 255; 96 for soft shadows).
  * On key pages the CG keys live video with **2-bit alpha** (CG/doc/CGBetaChanges:423-425), so alpha falls into 4 bands.
* `LOADFONT` with fewer than 2 arguments opens a **file requester**. Always pass the height (minimum 10) (cgrexx.c:1506-1520). `REQ_*` calls, `KILLPAGE`, `LOADBRUSH()`/`LOADPICT()` with no argument also open requesters. Avoid them.

### 2.2 Commands that matter

Source is cgrexx.c; the doc is DOC/CGARexx.Doc.

| Need | Call | Notes |
|---|---|---|
| Page number / type / line count | `GET_PAGE()`, `GET_PAGE('TYPE')` → `Empty Key Framestore Scroll Crawl`, `GET_PAGE('SIZE')` | 525-547 |
| Go to page n | `SET_PAGE(n)` (0-99) | 1058-1068. Never sends a success reply, so expect an empty result; the switch happens on the next refresh. |
| Make it a key page | `SET_PAGE('TYPE','K')` | 1026-1048. `SAVEPAGE` writes **nothing** for an Empty page (NewBook.c:1452-1497 has no default case) yet returns OK. |
| Select / deselect | `PICKPAGE()`, `PICKPAGE('NOT')`, `PICKLINE()`, `PICKCHAR()` | 1298-1330 |
| Load a font | `LOADFONT(path, height)` → "1"/"0" | `RexxAddFont` 1418-1464. A *newly* loaded font becomes the **default for typing**; an already-loaded one returns "0" and is not made default. |
| List loaded fonts | `GET_FONT('NEXT')` repeatedly → `"name,height"` | 568-597. `GET_FONT('FIRST')` resets and always returns "" (bug). Use the returned name in SET_CHAR(FACE). |
| Default/selected font | `SET_CHAR('FACE', name, height)` or `SET_FONT(name, height)` | 889-903 (sets the default and the selected text), 1075-1090 (selected only) |
| Face colour | `SET_CHAR('RGBA',r,g,b,a)`, `SET_CHAR('BOTR',...)`, `SET_CHAR('FILL',0/1)` (1 = top/bottom gradient) | 837-873 |
| Outline | `SET_BORD('SIZE',n)` (0 = none), `SET_BORD('RGBA',r,g,b,a)` | 1179-1246 |
| Shadow | `SET_SHAD('TYPE','N'/'D'/'C')`, `SET_SHAD('SIZE',1..5)`, `SET_SHAD('SPOT',0..7)` (direction), `SET_SHAD('RGBA',r,g,b,a)`, `SET_SHAD('PRIO',0/1)` | 1092-1177 |
| Type a line | `MAKELINE('text')` | Types each character, then Return; the new line becomes current (1348-1362). `MAKECHAR(c)` types one character. |
| Pick line n | `SET_LINE(n)` (creation order), `SET_LINE('FIRST'/'NEXT'/...)` (position order) | 795-810, 983-1013 |
| Position | `SET_LINE('SPOT', x, y)` | Raw X/YOffset, no redraw (948-957). Read back with `GET_LINE('SPOT')`. |
| Centre | `SET_LINE('JUST','C')` (L/R/N) | Re-justifies the current line (958-981). There is no ARexx for vertical centring; use SPOT y. |
| Box | `MAKERECT(x,y,w,h)` | Key and framestore pages only (1363-1378) |
| Save as crouton | `SAVEPAGE(path)` | `SaveCrouton` (NewBook.c:1444-1497); details below |
| Back to switcher | `EXIT()` | 1596-1600 |
| Render preview | `RENDER()` | Not needed for saving |

**What `SAVEPAGE` writes for a Key page** (NewBook.c:1444-1497, MyCrUD 153-192, RAMCRuD 1283-1300). Compare `Toaster:CG/Keys/LowerThird2`:

* `DoRenderDisk(path)` writes a framestore `FORM ILBM`:
  * 768×400, `BMHD y=43`, ByteRun1;
  * **18 planes**, `PLTP` = 06/7..0 and 07/7..0 (composite FRAME0/FRAME1), then **0B/1, 0B/0** (a 2-bit **alpha**). Plane types are in `old/switcher/INC/frameio.i:43-55`.
  * Decoding LowerThird2 shows the alpha is 0 transparent, 3 opaque, 1-2 anti-aliasing.
* Then two icon `FORM ILBM`s, each followed by a `FORM " PUS"` back-pointer.
* Then `FORM CrUD`:
  * `TYPE` = `KEYF`,0;
  * `LIBS` = $FFFFFCD0, `"effects.library"`;
  * `TAGS` = Duration 180, Speed 0, HoldFields 0, FadeOutDuration 0, then `TAG_Page` ($69) holding the CG page data (`ToasterBookFile`...), so the page can be edited again.
* Then the final PUS. The PUS value = (PUS offset − FORM offset − 8).
* It also leaves `path.i` (the icon) next to the file.

### 2.3 Example (run as one `toaster_rexx` job while the CG is on screen)

```rexx
/* cg-title.rexx -- build a lower-third key page and save it as a crouton */
OPTIONS RESULTS
/* keep the (blocked) switcher host out of the way of CG calls (2.1) */
call remlib('ToasterARexx.port'); call remlib('PROJECT_REXX_PORT')
IF ~SHOW('L','CG_AREXX') THEN call addlib('CG_AREXX', 0)

pg = -1
DO i = 1 TO 99                              /* find an empty page */
  call SET_PAGE(i)
  IF GET_PAGE('TYPE') = 'Empty' THEN DO; pg = i; LEAVE; END
END
IF pg < 0 THEN DO; say 'no empty page'; signal done; END
call SET_PAGE('TYPE', 'K')                  /* Key page -> KEYF crouton */
call PICKPAGE('NOT')                        /* attributes go to the typing default only */

font = 'Toaster:Programs/CG_Support/SimpleFuture.36'   /* installed CG fonts:
         CommonThin.10, SimpleFuture.36, SimpleThin.36; Toaster:PSFonts is empty */
say 'loadfont' LOADFONT(font, 36)
call SET_CHAR('FACE', font, 36)             /* if this errors, use the name GET_FONT('NEXT') reports */
call SET_CHAR('FILL', 0)
call SET_CHAR('RGBA', 255, 255, 255, 255)   /* white face, opaque */
call SET_BORD('SIZE', 2)
call SET_BORD('RGBA', 0, 0, 0, 255)         /* black outline */
call SET_SHAD('TYPE', 'D')
call SET_SHAD('SIZE', 2)
call SET_SHAD('SPOT', 7)
call SET_SHAD('RGBA', 0, 0, 0, 160)

call MAKELINE('TITLE LINE ONE')
call MAKELINE('second line')

call SET_LINE(1); call SET_LINE('SPOT', 0, 320); call SET_LINE('JUST', 'C')
call SET_LINE(2); call SET_LINE('SPOT', 0, 370); call SET_LINE('JUST', 'C')
say 'page' pg GET_PAGE('TYPE') 'lines' GET_PAGE('SIZE') 'spot1' GET_LINE('SPOT')

call SAVEPAGE('Toaster:CG/Keys/Title1')     /* writes Title1 (+ Title1.i) */
call EXIT()                                 /* back to the switcher */
done:
call remlib('CG_AREXX')                     /* CG port stays alive while loaded */
exit 0
```

The Y values are unverified. Key data covers frame lines 43-442, and lines dragged below the key area are cut off (CGBetaChanges:486-490). Read `GET_LINE('SPOT')` after the first MAKELINE to calibrate.

**Fallback for text entry.** Once in the CG, plain typing works: `input_type`, Return for a new line. CG keys (newrender.a:2357-2398):

* F1 shadow type, F2 shadow direction, F3 shadow length, F4 outline, F5 shadow priority;
* F6 justify, F7 render-disk, F8 render line, F9 render, F10 take;
* Alt/Shift-Alt + F-key run the CG-Config macros (cgrexx.c CaseMacro).

---------------------------------------------------------------------------

## 3. How the switcher keys a CG page over Program

**Crouton type.** CrUD `KEYF` = Edit `CT_KEY` "Key Page", handled like a framestore (`DT_FRAMESTORE`) (ED/filetypes.c:76-110, 179-200). `SCRO`/`CRAW` are Scroll/Crawl pages (`DT_CGPAGE`), handled by `ScrollHandler`/`CrawlHandler` (SW/cgsupport.a:183-276).

**Handler.** `KeyHandler` (SW/cgsupport.a:279-406) is the crouton's FG function.

| FGC | Effect |
|---|---|
| SELECT | `FrameSelect` (610-866), below. A second SELECT on an already-selected crouton puts it on Preview. |
| AUTO | Fade on (`FrameToMain` → `PutUpKey` + `HandleAlphaAutoOn`, 554-604) or fade off (`HandleAlphaAuto`), toggled by `FadeOffFlag`. Fade length from tag `FadeInDuration` (339-344), else the current speed. |
| TAKE | Cut on (`CutFrameToMain` 510-552 → `CUTITONOFF`) or cut off (468-495). |
| TBAR | `HandleAlphaTBar`, a manual fade (1423 onward). |
| OBUTTON | `FrameOLay`. |
| REMOVE | `KillAlphaKey` (390-395). |

**What `FrameSelect` does:**

* Turns live DVE off, freezes, and picks the DV bank not used by Main or Overlay (`OBR`).
* `QueryFile` + `FrameLoadNew`: composite planes go to the bank and alpha planes to `TB_AlphaFastBM` (768×400, at most 4 planes).
* Sets `KeyTop = bmh_Y`.
* `FrameLoadNew` rejects non-composite files ("Frame corrupt", SW/frameslice.a:2718-2750). `bmh_Y≠0` triggers `KeyPageLoad`, which clears the bands (3047 onward). 400 is the only supported key height (`smartsend.i:32-33`).

**ARexx sequence** (switcher and project hosts in the library list, CG_AREXX not):

```rexx
say CROUTONLOAD('Toaster:CG/Keys/Title1')  /* append to project + ES_Select -> FrameSelect; "1" = selected */
call Switcher('AUTO')                      /* fade key on over Program */
/* ... */
call Switcher('AUTO')                      /* fade off   (or Switcher('TAKE') = cut on/off, Switcher('TBAR',n) = manual) */
```

* `CROUTONLOAD` = ED/ARexxPort.c:1220-1260. It needs the editor in a project view. `CROUTONPICK n` + `Switcher('DOEN')` also works for a crouton already in the project.
* **Do not use `Switcher('FMLD', keyfile)` or `CROUTONRUN`** for keys. In 4.2, FMLD = LoadCrouton → Select → Auto → FreeCrouton (AA $1278e2; ES 8/13/14/7), the same as CROUTONRUN (ARexxPort.c:1317-1345). FreeCrouton → `RemoveTBFG` → FGC_REMOVE → `KillAlphaKey` takes the key straight down again. This is predicted, not tested.

**What the card sees** (cross-check video-path-spec.md section 6):

* **AM** = Main (`Mask2AM`).
* **BM** = the DV bank holding the fill: `ELHPUT_BM_R Mask2BM(OLaySec)` → DAC0/DAC1 (cgsupport.a:958-960; AlphaOnELH 1838-1865 has BM = OLay).
* **CDS:** `PutUpKey` sets **DIB (5)** for ≤2 alpha planes or **DIBGR (4)** for 3-4 (962-966). The AA cut/fade paths then force **DIBGR** even for a 2-bit key: CUTITONOFF 1364-1410, HandleAlpha 1462-1530, "Not DIB, even though we're doing a 2 bit key". It is not PIXELSW.
* **CD** = $FF fully on, $00 off. During a fade it is `level | $C0` (HandleAlpha). Palettes `AAFadeDIB0..3` are picked by the fade level from the table at 3004-3008; full on selects `AAFadeDIB0` and the cut-off path selects `AAFadeDIB3`.
* **The Amiga side** shows `TB_CurrentAlphaBM`: the 2 key planes, plus up to 5 interface planes on AA (`AAKeysI` table, 2832-2860).
* **Where the alpha code lives:**
  * AA: in the colour's low bits, `PACO_DIB0..3` = $000-$003 (blue bits 1-0); DIBGR uses r1 $200, g1 $020, b0 $001, b1 $002 (`old/switcher/INC/tags.i:2006, 2049-2070`).
  * ECS: in the blue low nibble, **$6/$8/$7/$9 = codes 0/1/2/3** (`NonAAKey2`, cgsupport.a:2742-2750).
* These palette codes are switcher-internal. A crouton file only supplies raw 0-3 values in its alpha planes.

---------------------------------------------------------------------------

## 4. If the CG cannot be driven

**A. Luma-key a host RGB picture (all ARexx, no CG).**

* `Switcher('LRGB', 'ToasterFiles:title.iff', 0|1)` loads an **IFF24** (plain 24-plane ILBM, ≤752×480, `CommonBufferWidth/Height`, `old/switcher/INC/commonrgb.i:33-34`) or a framestore. It encodes it with `SendRGBPicture` into DV1 (0) or DV2 (1).
  * In 4.2 the bank argument appears to be used: it is saved in d3, and `SendRGBPicture` passes the saved d3 as d1 to LVO -$62a (AA `LoadRGBFILE` $1282a0, `SendRGBPicture` $139640).
  * It carries no alpha.
* Then put DV1 on the Overlay row, `Switcher('ODV1')`, and key it with `Switcher('KBLK')` (black transparent) or `KWHT`. Trim with `Switcher('CLIP', n)`; turn off with `KOFF`.
* The card is then in LUMKEY mode: AM=Main, BM=LK=OLay (video-path-spec.md section 6). Possibly `Switcher('FVID')` first so the DV bank shows the still.
* Cost: hard-edged luma key, and black inside the fill is keyed out too.

**B. Host-made `KEYF` crouton (anti-aliased, uses the stock key path in section 3).**

The switcher only takes **composite** data, so let it do the RGB→composite step:

1. Run `Switcher('LRGB', fill.iff, 0)`, then `Switcher('PDV1')`, then `Switcher('FMSV', 'ToasterFiles:fill')`. This saves a 4-field `FRAM` crouton: in 4.2 FMSV = ES_FoundFile 'FRAM' → `ReallyProcessSaveButton`, AA $127cbe; pecomm.a:2502-2535. The result is 768×480, 16 planes, PLTP 06/07, icons, CrUD.
2. On the host, rewrite the first `FORM ILBM`:
   * keep BODY rows **43…442**;
   * append two planes per row (alpha bit1, then bit0; 96 bytes each, 0 = transparent, 3 = opaque);
   * BMHD: `h=400, y=43, nPlanes=18`;
   * PLTP: add `0b 01 0b 00` (size 36);
   * re-ByteRun1 per plane-row.
3. Patch the CrUD `TYPE` from `FRAM` to `KEYF`. Leave the icon FORMs and their PUS markers alone; they point at the trailing FORMs, not the image.
4. Then `CROUTONLOAD` + `AUTO` as in section 3.
   * Up to 4 alpha planes are accepted (DIBGR, AA only), but 2 matches what the CG writes.
   * `vt/dve-tools/ilbm.py` and `crud.py` already read these chunks.

**C. Write the composite planes directly on the host.** This needs the Toaster NTSC encoder (SW/rgb2yiq.a, `CopyRGB` plus LVO -$630 used by SendRGBPicture). More work than B, for the same result.

**D. Use the CG as the encoder for a host image.** `LOADBRUSH(path)` puts an ILBM brush on a key page, then `SAVEPAGE`. This still needs the CG on screen and is untested. `LOADPICT(path)` only sets a Framestore-page background (no alpha).

---------------------------------------------------------------------------

## 5. Most promising procedure

1. **Job A (switcher):** `Switcher('CKCG')`; if 0, `Switcher('LDCG')`. This is synchronous and loads only.
2. **Enter the CG**, one of:
   * (a) `Run >NIL:` the `EnterCG` helper (1.4);
   * (b) Programs popup: press at about (593, 258), drag 39 px down, release;
   * (c) click an empty spot of the switcher panel, then `input_key 0x58`.
3. **Wait until the CG is ready:** a screenshot, or the `S:CG/StartUp.rexx` flag (1.6). Do not touch mouse or keyboard after that.
4. **Job B (CG):** the section 2.3 script. It removes the switcher and project hosts from the library list, builds the page, runs `SAVEPAGE('Toaster:CG/Keys/Title1')` and `EXIT()`, then `remlib('CG_AREXX')`.
5. **Job C (switcher + project):** `CROUTONLOAD('Toaster:CG/Keys/Title1')`, then `Switcher('AUTO')` to fade on and `Switcher('AUTO')` again to fade off.

**Bridge changes this implies** (Copperline/src/control/toaster_link.rs, not made):

* For `program=cg`, swap the host set as in 2.1 instead of only adding `CG_AREXX`.
* Have `open` for cg do LDCG plus one of the entry methods above, instead of LDCG alone.

## 6. Open questions / to test

* The ARexx search rule for function hosts (RC 10 with error 0 stops the search; error 1 continues) is taken from NewTek's own comments (ED/ARexxPort.c:3424) and should be confirmed with `SHOW('L')` plus one call.
* Font name format accepted by `SET_CHAR('FACE',...)` (full path vs. file part); read it back with `GET_FONT('NEXT')`.
* The Y coordinate convention of `SET_LINE('SPOT')`.
* Whether Esc in the CG exits cleanly.
* The CG binary contains the string `CG_Support/CGButtons.Pic`, which is not installed. No PC-relative reference to it was found, so it is probably unused.
* The `EnterCG` helper is untested.
