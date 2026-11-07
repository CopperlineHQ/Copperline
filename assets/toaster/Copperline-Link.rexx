/* Copperline link: runs the ARexx jobs the PC drops into
   ToasterFiles:.copperline/ and hands their output back.

   The PC side is copperline-ctl's MCP server (toaster_rexx,
   toaster_switcher, toaster_project). ToasterFiles: is the emulated
   Toaster's host folder, mounted by [video_slot] volume = "ToasterFiles".
   Start it from S:User-Startup:
       Run >NIL: SYS:Rexxc/RX S:Copperline-Link.rexx
   It does nothing (and exits) when ToasterFiles: is not mounted. */

OPTIONS RESULTS
IF ~SHOW('Libraries','rexxsupport.library') THEN
  call addlib('rexxsupport.library', 0, -30, 0)

/* Assign EXISTS looks without asking for a missing volume. */
address command 'Assign >NIL: ToasterFiles: EXISTS'
IF rc ~= 0 THEN exit 0

dir = 'ToasterFiles:.copperline'
box = dir || '/'

/* The job runner: RX the job with its output to out-ID.txt, then
   done-ID.txt with RX's return code. */
IF OPEN('r', 'T:copperline-job.rexx', 'W') THEN DO
  call WRITELN('r', "/* Copperline link: run one job */")
  call WRITELN('r', "PARSE ARG id")
  call WRITELN('r', "box = '" || box || "'")
  call WRITELN('r', "job = box || 'run-' || id || '.rexx'")
  call WRITELN('r', "address command 'SYS:Rexxc/RX >' || box || 'out-' || id || '.txt ' || job")
  call WRITELN('r', "r = rc")
  call WRITELN('r', "address command 'Delete >NIL: ' || job || ' QUIET'")
  call WRITELN('r', "IF OPEN('d', box || 'done-' || id || '.tmp', 'W') THEN DO")
  call WRITELN('r', "  call WRITELN('d', r)")
  call WRITELN('r', "  call CLOSE('d')")
  call WRITELN('r', "  address command 'Rename >NIL: ' || box || 'done-' || id || '.tmp ' || box || 'done-' || id || '.txt'")
  call WRITELN('r', "END")
  call CLOSE('r')
END

main:
/* StartToaster removes and re-adds rexxsupport.library; a call landing
   in between fails, so a failure starts the loop again. */
SIGNAL ON SYNTAX
IF ~EXISTS(dir) THEN call MAKEDIR(dir)
DO FOREVER
  files = SHOWDIR(dir, 'FILE', '/')
  DO WHILE files ~= ''
    PARSE VAR files f '/' files
    IF UPPER(LEFT(f, 4)) = 'JOB-' & UPPER(RIGHT(f, 5)) = '.REXX' THEN call runjob f
  END
  call DELAY(10)
END

/* Each job runs in a process of its own (T:copperline-job.rexx), so one
   that never returns (a program that does not answer its port) does not
   stop the link. The job is renamed run-ID.rexx first: the PC can tell
   it was taken. */
runjob: PROCEDURE EXPOSE dir box
  PARSE ARG f
  id = SUBSTR(f, 5, LENGTH(f) - 9)
  IF ~RENAME(box || f, box || 'run-' || id || '.rexx') THEN RETURN
  address command 'Run >NIL: SYS:Rexxc/RX T:copperline-job.rexx' id
  RETURN

SYNTAX:
  address command 'Wait 1'
  IF ~SHOW('Libraries','rexxsupport.library') THEN
    call addlib('rexxsupport.library', 0, -30, 0)
  SIGNAL main
