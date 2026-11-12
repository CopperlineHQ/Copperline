# "Toaster Flyer Essentials" tutorial vs Copperline: function map, guards, next steps

Source: the Desktop Images "Toaster Flyer Essentials" transcript (Stan Shumlick). Status comes from
reading the emulator (`Copperline/src/flyer/{ops,fs,play}.rs`, `src/toaster/`, `docs/`) and the 4.2
sources (`fly/Edit/Edit/src`, `vt/full/switcher/SRC`, `fly/amiflyer`). Nothing was run.
**OK** = works, **Partly** = works with gaps, **No** = not emulated (what happens is given).
[guess] = inferred, not traced end to end.

## 0. Setup facts the tutorial depends on

- **Grazer tabs** come from `Toaster:Programs/Edit_Support/dirtabs`:
  VideoA = `FA0:`, VideoB = `FB0:`, Audio = `FC0:`, NewTek = `toaster:`,
  Effects = `toaster:Effects`, Projects = `toaster:Projects`, RXTools = `toaster:Arexx/Editor`.
  The second row (circle-arrow button) is empty. **Set Tab** rewrites the same file.
- The test install (`amiga/boot-flyer`) mounts only `DEVS:DOSDrivers/FA0`, and `media/Flyer` holds only
  `FA0`. So the **VideoB and Audio tabs show nothing**. The Editor sets `pr_WindowPtr = -1`, so no
  "insert volume" requester appears. This is harmless.
- **Where settings are saved:**
  - Editor Options: `Toaster:Programs/Edit_Support/prefs`.
  - Hardware setup: `Toaster:Programs/Switcher_Support/HS` (factory copy: `HS.default`).
  - Default project: `Switcher_Support/DefaultProject`.
  - Flyer options (Enable HQ5/HQ6, dropped-frame detection): the emulated EEPROM. It is **reset at
    every emulator start**.
- The test `S:User-Startup` lacks `Assign REXX: Toaster:Arexx ADD` and `Assign HIIP:`.
  `configuration.md` says the CG Alt-F macros and the Import/Univ2Clip scripts need them.

## 1. Map of what the tutorial covers

### 1.1 Switcher and centre button strip
| Item | Tutorial use | Path | Status |
|---|---|---|---|
| Program/Preview rows, inputs 1-4 | 4-input live switcher | `media/Input 1..4`; 3/4 = Flyer ch A/B while "Flyer Out" is on | OK |
| DV1/DV2 | two frame buffers | `Toaster:Framestore` | OK (load/save/grab). AutoHue "DV1/DV2 colour mismatch" cannot happen here |
| BKG matte, T-bar/Auto/Take, wipes, DVE | transitions | `Toaster:Effects/4000FX/*` | OK |
| Views popup | Project/Switcher, Project/Files, Files/Files, Project/Project | | OK |
| Play (Tab), "ready to play" req, right mouse = stop | sequence playback | | OK (NEWSEQ...PLAYSEQ, RESETFLYER) |
| Play From + Stop crouton | play a short section | `Effects/4000FX/Control/Stop` | OK |
| Clone, All, Delete, Save, Controls (F9) | editing | | OK (Delete: see 1.2) |
| Programs popup | Switcher, LightWave, ToasterPaint, ToasterCG, ChromaFX, custom | | OK. ChromaFX is Partly: no LUT, so the picture passes unchanged |
| Tools popup | 1.6 | | |
| Record (`~`) | 1.3 | | |

### 1.2 Grazer (Files area)
| Item | Status |
|---|---|
| Flyer drive icons, hard drives, RAM: | OK. FA0 appears as `FlyerA0:`. A `.flyervol` file renames it (e.g. "Flyer 9A") |
| Volumes / Devices / Assigns button; parent arrow; scrollers; path line | OK |
| Right-Amiga: picons or names | OK. A picon needs `clip.I` (MAKEICON or the Flyer controls) |
| Double-click a clip = play it | OK (FLYPLAY onto input 3/4) |
| Tabs: VideoA, NewTek, Effects, Projects, RXTools; 2nd row; Set Tab | OK |
| Tabs: VideoB (FB0:), Audio (FC0:) | Partly. Empty until `Flyer/FB0` and `Flyer/FC0` exist and their DOSDrivers are copied in |
| FlyTape (`FLY_TAPE:`, back up to a SCSI tape on the Flyer bus) | No. The scan prints "No SCSI Tape drives found" [M: SCSIDIRECT answers OK with no data] |
| Delete | Flyer clips are refused (PROTECTED; an error requester appears). **Any other file, including everything on `NewTek:` and `ToasterFiles:`, is really deleted on the host** |

### 1.3 Record Raw Footage panel (`~`, Record button, Space)
| Item | Status |
|---|---|
| L/R gain sliders, peak meters with clip lamp | Partly. Stored (CODECCTRL). The meters show the peak of the Toaster's program sound |
| Video source | Partly. Stored (INPUTSEL); nothing is captured |
| Drive pull-down (a speaker icon marks the audio drive); quality EP/SP/HQ5/HQ6/audio only | Partly. Listed. HQ5 appears only after Hardware setup > Enable HQ5, which is lost at emulator exit |
| Time available / reorganised time; Reorganize | Partly. The drive is a virtual 8 GB, both times are equal, and Reorganize is a no-op (DEFRAG returns OK) |
| Record / Pause / Stop / Space | **No**. FLYRECORD returns PROTECTED and the panel shows "Recording stopped: Internal Flyer Error # 42". **No hang.** Cancel/Esc deletes the (missing) temp clip |
| Cut Clips button | goes to 1.4. After a failed record there is no temp clip, and the Editor warns |

### 1.4 Cutting room, Cut Clip, Process Clip (`panel.c` CutClipPanel)
| Item | Status |
|---|---|
| Name with .01 auto-increment, comment, in/out sliders, bump arrows, duration, segment timeline, Remove/Previous/Next, icon slider, Video+Audio/Audio/Video, Destination drive | The UI works, and the Program monitor follows the sliders (SRCHARGS/DOSEARCH) [guess: tested on the jog requester and the clip controls, not this panel] |
| Perform | OK. NEWCUTLIST/ADDSUBCLIP/MAKESUBCLIPS follow `clip.c` (`MakeSubClips`): each piece is copied from the master's coded frames (whole colour frames; picture, sound or both, `StripAudioRaw`/`StripVideoRaw`) into `_tmp_<ref>` on its drive, and flyer.library renames it; the Editor then makes its icon. Destructive: the master's file moves to `Flyer/Trash` (not deleted) |

### 1.5 Clip controls (F9): Fine Tune, Quick Tune, Ctrl quick adjust
| Item | Status |
|---|---|
| Video in/out; separate audio in/out (split edits); Alt-click to match; Alt-drag to lock; Shift-drag to slide; Shift+Alt; Ctrl / Ctrl+Shift / Ctrl+Shift+Alt with L/R mouse | OK: the values are honoured on play, in 2-frame (colour-frame) steps, and Program shows (and the speakers play) the colour frame at the point being moved. The frame's sound loops until the next move or until the panel closes: the Editor's quiet-after-five-ticks (`panel.c` TilQuiet) never fires in the emulated session [open issue] |
| Shuttle arrows (hold, then move the mouse left/right) | OK [guess: the pacing is tested through the mailbox, the drag itself was not run]. DOSEARCH waits for the previous colour frame to take, so `Flyer_WaitAction` paces Hand_Shuttle to a frame per colour frame |
| Play/Stop buttons in the panel | OK (FLYPLAY) |
| Volume, balance, fade in/out + Auto, channels 1+2/1/2/none, Lock, delay Time, 8 channels (4 stereo pairs) | OK. The tutorial says "95 = as recorded" (the real DSP has headroom). The emulator treats `$FFFF` as unity (linear, spec [L]), so levels may differ a little [guess] |
| Process clip / Cut clip buttons, Destination | see 1.4 |
| Framestore, Flyer-still and CG-page controls (length; 10 min default; key delay of at least 1 s; fades S/M/F) | OK |

### 1.6 Tools popup and its panels
| Item | Status |
|---|---|
| Audio on/off (Alt-A), Lock/Unlock (Alt-L), Quick Adjust (Ctrl) | OK (Quick Adjust shows the picture [guess]) |
| Auto insert (Alt-I); Edit to all audio; Edit to crouton (tap to the beat while it plays) | OK [guess] |
| Cut clip (Alt-C) | **No.** See guard 1 |
| Process clip (Alt-P) | **No.** Silent no-op |
| User tools: FlyerProc, MaxVidLength, Measure (`Toaster:Arexx/Editor/StartUpScripts`, added by StartToaster/UpdateTools) | see 1.7 |
| **Hardware setup** (saves to `Switcher_Support/HS`): RGB vs superimposed on Preview | Partly. Needs `[video_slot] preview_overlay = true` |
| Input termination | Stored; has no effect |
| GPI off / pulse front / pulse back (joystick fire on port 2) | OK [guess] |
| Flyer outputs (inputs 3/4) | OK. Unchecked gives the host Input 3/4 folders; playing a clip takes them back |
| Enable HQ5/HQ6 | Stored in the EEPROM; lost at emulator exit |
| Hidden RAlt+RShift+F10 Manual Adjust: PED/GAIN/Hue/matte trims, AutoAdjust | The trims have no effect. AutoAdjust: see guard 4 |
| **Options** (`Edit_Support/prefs`): Novice/Super Genius | OK. Keep Novice: Super Genius drops the delete/overwrite confirmations |
| Stop recording on dropped frame | Stored; moot |
| Stop sequence on timing error | OK. If emulator timing produces switcher timing errors, uncheck it, as the tutorial advises [guess] |
| Prefer fine tune; CG key on/off | OK |

### 1.7 RXTools (`Toaster:Arexx/Editor`)
- **OK** (they only touch the Editor): ButtAudio, MaxLength, MaxVidLength, UnMaxAV, UnMaxVid, PrepStills, VolumeAbs/Rel,
  Measure, JobTitle, Haiku, FixProject, Export, UpdateTools, RePlay (loop crouton, [guess]), and SetDefault
  (writes `Switcher_Support/DefaultProject`).
- **Partly**:
  - ChangeIcon: OK. REQ_FLYERJOG shows the clip on Program as its sliders move (tested).
  - FlyerProc (slow/fast/strobe): reads fields, then stops on "Write open error 42", so it is effectively No.
- **No** (they need recording or still writing; each ends in an error requester): IFF2Clip, Univ2Clip, BatchStills,
  FlyerLapse, FlyerStopFrame. They use `Flyer_Support/c/AppendFields` + `SetPlayMode REC`, or `recordadd`.
- The framestore versions of StopFrame and TimeLapse (FMSV/SRGB to disk) should work [guess]. They **wait on
  `Switcher(WAIT,GPI)` or a timer by design**, so they look hung.
- Import needs `HIIP:`.

### 1.8 CG, Paint, projects, Workbench, NewTek/Programs
| Item | Status |
|---|---|
| CG: type, auto-centre, kern; Controls > Save/Load page into `Toaster:CG/{Books,Crawls,Frames,Keys,Scrolls,Textfiles}`; frame page as framestore crouton; key page as key crouton; re-save to update the project | OK. Known hang: CG macros hang if `ToasterARexx.port`/`PROJECT_REXX_PORT` are left on the ARexx library list (Copperline link; documented) |
| Paint (save a blank page as `Toaster:Framestore/black`), LightWave | OK. "Save to Flyer" still: No (OPENWRITEFLD returns PROTECTED, so an error) |
| Projects: save/load in `Toaster:Projects`, numbered versions, missing-file "?" crouton, Alt-drag to replace and inherit, Project/Project view with New project, bars/black/slate head, default project | OK (Editor only) |
| `VoidAll` in a Shell; Left-Amiga N/M | OK (VOIDALL is a no-op: no heads exist) |
| FlyerCalibrate | No: guard 3 |
| FlyerHDTools: rescan, read and read/write tests, format/short stroke, cache options, config | No: guard 2 |
| AutoHue icon | This install's `Programs/AutoHue` is a 6-byte stub ("empty") and does nothing. The real routine is guard 4 |
| Utilities/Genlock | Partly [guess] (the encoder/Amiga picture and genlock transparency are modelled) |
| ScreamerNet | n/a (needs networked Amigas) |
| StartUps (ST_Files-Files, ST_Proj-Files, LWStart); CheckSoftware, MultiRename, Flyer Version | OK [guess] |
| External TBC, waveform/vectorscope, audio mixer | n/a. Inputs are host files; the Flyer's TBC reports "no module" |

## 2. Disable or guard, by priority (least invasive fix first)

1. **Destructive Cut Clip** (Alt-C, Tools, the Fine Tune button, Perform after a raw record).
   - Done: the cut list is modelled (see 1.4), so Perform makes the pieces. The master's file is moved
     to `Flyer/Trash` beside the drive folders instead of being deleted, so a destructive cut can be
     undone on the PC.
2. **FlyerHDTools / flyerscsi.device.**
   - Risk: SCSIDIRECT, SCSICALL and READTEST to a present drive answer OKAY and do nothing. INQUIRY and READ
     CAPACITY come back empty and the speed test finishes at once: blank drive data, nonsense speeds, and a
     possible divide-by-zero guru [guess].
   - FORMAT and WRITETEST already return PROTECTED, so host folders are never formatted.
   - Fix (emulator side): return `SELTIMEOUT` (or a CHECK CONDITION) for SCSIDIRECT/SCSICALL and `CMDFAILED` for
     READTEST.
   - Fix (Toaster side): rename `Toaster:Programs/FlyerHDTools(.info)`.
3. **FlyerCalibrate** (with CalibNew and SetFloobyCal).
   - It needs genlock/phase hardware and a recorded `TestSignal` clip. `SWITCHER(FLY1)` cannot record, so it exits
     with "unable to create a Test Clip".
   - If a TestSignal clip exists, it runs long sweeps against phase that is not modelled, and can save DHUE + MKHS
     into `HS` and write calibration to the EEPROM [guess].
   - Fix: rename the icon.
4. **AutoHue / AutoAdjust** (RAlt+RShift+F10 Manual Adjust slice).
   - It reprograms the card with the `vta`/`vta2` patches and loops on a "connect a camera to input 1" requester
     (Cancel exits).
   - AutoHue reads the colour burst in blanking, which is not modelled, so bogus EP/DP values get saved into the
     host's `Switcher_Support/HS` [M].
   - Fix: document it; back up `HS` (restore it from `HS.default`).
5. **Record panel and recording scripts.** These fail cleanly (#42). Keep PROTECTED.
   - Optional (Toaster side): move IFF2Clip, Univ2Clip, BatchStills, FlyerLapse, FlyerStopFrame and FlyerProc into a
     subfolder (e.g. `Toaster:Arexx/Editor/NeedsRecord`).
   - Move FlyerProc out of `StartUpScripts` too, then run UpdateTools, so they leave RXTools and the Tools menu.
6. **Shuttle racing.** Fixed: DOSEARCH waits for the previous colour frame to take, as the firmware does.
7. **Scripts waiting on GPI or a timer** (StopFrame, FlyerStopFrame, TimeLapse). Document: GPI = joystick fire in
   port 2 (Hardware setup GPI on), or stop the script.
8. **VideoB/Audio tabs.** Create `media/Flyer/FB0` and `FC0`, and copy `System_Support/Devs/DOSDrivers/FB0`, `FC0`
   into `DEVS:DOSDrivers`. Or use Set Tab to point them at FA0: folders.
9. **Keep the Novice level.** Deletes and overwrites on `NewTek:`/`ToasterFiles:` act on real host files.
10. **Don't copy the emulator's `HS`, `Edit_Support/prefs`, `dirtabs` or `DefaultProject` back to the real Toaster**
    unreviewed.
11. **EEPROM resets at exit** (HQ5 and the other options). Emulator side: persist the EEPROM, e.g.
    `media/Flyer/.flyereeprom` (small).
12. **CG ARexx host ordering.** Already documented; keep the link's `remlib`.

## 3. Top 5 features the tutorial shows, not yet emulated

1. **Jog, shuttle and scrub** (SRCHARGS/DOSEARCH). Done: the colour frame loops on channel A/B with its sound, paced
   as the firmware paces it.
2. **Recording** (FLYRECORD with pause/resume/abort, audio-only, APPEND).
   - What: record from the INPUTSEL source (Input 1/2, Program, Preview; camcorder = Input 1) through
     `flyerclip::ClipWriter`. The grade comes from the quality setting and GETSETOPT; the meters follow the source's
     sound.
   - This is the tutorial's first half. It also unlocks the AppendFields scripts and FlyerCalibrate's test clip.
3. **Cut lists + COPYCLIP** (cutting room Perform, Cut clip, Process clip, Destination).
   - What: copy colour-frame ranges into `_tmp_NNN` clips. The library renames them; MAKEICON follows.
   - Why: it makes long `flyerclip import`ed videos editable on the Toaster, and it fixes guard 1 for real.
   - Destructive mode may delete the original only after every subclip is written.
4. **Flyer stills and field writes** (OPENWRITEFLD/CLOSEFIELD, FLOOBY still mode).
   - Unlocks: CG/Paint/LightWave "save to Flyer", BatchStills, IFF2Clip/Univ2Clip, and FlyerProc's output.
   - Effort: mostly ClipWriter plumbing.
5. **Proper answers for the HD tools.**
   - What: synthesised SCSIDIRECT (INQUIRY, READ CAPACITY, MODE SENSE/SELECT) and a timed READTEST, so FlyerHDTools
     and FlyTape show real drive data instead of a crash risk. Add EEPROM persistence.
   - Runners-up: the LUT (ChromaFX), burst in blanking (AutoHue), and the DSP volume law ("95 = unity").
