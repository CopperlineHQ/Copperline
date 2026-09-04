// SPDX-License-Identifier: GPL-3.0-or-later

const childProcess = require("child_process");
const path = require("path");
const vscode = require("vscode");

function ctlExecutable() {
  return vscode.workspace.getConfiguration("copperline").get("ctlExecutable") || "copperline-ctl";
}

function runCtl(args, cwd) {
  return new Promise((resolve, reject) => {
    childProcess.execFile(ctlExecutable(), args, { cwd }, (error, stdout, stderr) => {
      if (error) reject(new Error((stderr || error.message).trim()));
      else resolve(stdout.trim());
    });
  });
}

async function fileExists(uri) {
  try {
    await vscode.workspace.fs.stat(uri);
    return true;
  } catch (_) {
    return false;
  }
}

async function chooseProgram(title) {
  const selected = await vscode.window.showOpenDialog({
    title, canSelectFiles: true, canSelectFolders: false, canSelectMany: false,
  });
  return selected && selected[0];
}

class CustomRegisterProvider {
  constructor() {
    this.items = [];
    this.changed = new vscode.EventEmitter();
    this.onDidChangeTreeData = this.changed.event;
  }
  getChildren() { return this.items; }
  getTreeItem(register) {
    const item = new vscode.TreeItem(register.name, vscode.TreeItemCollapsibleState.None);
    const offset = Number(register.offset).toString(16).toUpperCase().padStart(3, "0");
    item.description = `${register.value}  $${offset}`;
    item.tooltip = new vscode.MarkdownString(register.documentation || register.description || "");
    item.contextValue = "copperlineCustomRegister";
    item.iconPath = new vscode.ThemeIcon(register.access === "read" ? "eye" : "symbol-field");
    return item;
  }
  async refresh() {
    const session = vscode.debug.activeDebugSession;
    if (!session || session.type !== "copperline") {
      this.items = [];
      this.changed.fire(undefined);
      return;
    }
    try {
      const result = await session.customRequest("copperline/chipset", {});
      this.items = (result.registers || []).filter((item) => Number.isInteger(item.offset));
    } catch (_) {
      this.items = [];
    }
    this.changed.fire(undefined);
  }
}

async function detectBartmanBinPath() {
  const commands = await vscode.commands.getCommands(true);
  if (!commands.includes("amiga.bin-path")) return undefined;
  try {
    const found = await vscode.commands.executeCommand("amiga.bin-path");
    const value = found instanceof vscode.Uri ? found.fsPath
      : typeof found === "string" ? found
        : found && (found.binPath || found.path || found.fsPath);
    if (!value) return undefined;
    const base = path.basename(value).toLowerCase();
    return base.includes("m68k-") || base === "elf2hunk" ? path.dirname(value) : value;
  } catch (_) {
    return undefined;
  }
}

function launchConfiguration(name, extra) {
  return Object.assign({
    type: "copperline", request: "launch", name,
    program: "${workspaceFolder}/demo", symbolFile: "${workspaceFolder}/demo.elf",
    entryPoint: "main", stopOnEntry: true, factory: true,
  }, extra);
}

async function initProject(context) {
  const selected = await vscode.window.showOpenDialog({
    title: "Create Copperline Amiga Project", openLabel: "Create Here",
    canSelectFiles: false, canSelectFolders: true, canSelectMany: false,
  });
  if (!selected || !selected[0]) return;
  const destination = selected[0];
  const bartmanBin = await detectBartmanBinPath();
  const choices = [
    { label: "Auto-detect", id: "auto", detail: "Choose the first toolchain found on PATH" },
    { label: "Bartman m68k-amiga-elf", id: "bartman", detail: bartmanBin || "m68k-amiga-elf-gcc + elf2hunk" },
    { label: "Bebbo amiga-gcc", id: "bebbo", detail: "m68k-amigaos-gcc" },
    { label: "vbcc + vasm", id: "vasm", detail: "vc with the Kickstart 1.3 target" },
  ];
  if (bartmanBin) choices[1].description = "Detected";
  const toolchain = await vscode.window.showQuickPick(choices, {
    title: "Amiga toolchain",
    placeHolder: bartmanBin ? "Bartman's toolchain was detected" : "Select a toolchain",
  });
  if (!toolchain) return;

  const relativeFiles = ["main.c", "uaelib.c", "uaelib.h", "startup.s", "runtime.c", "format_gcc.s", "format_vasm.asm", "Makefile"];
  const vscodeDir = vscode.Uri.joinPath(destination, ".vscode");
  const targets = relativeFiles.map((name) => vscode.Uri.joinPath(destination, name));
  targets.push(vscode.Uri.joinPath(vscodeDir, "launch.json"));
  targets.push(vscode.Uri.joinPath(vscodeDir, "tasks.json"));
  const conflicts = [];
  for (const target of targets) if (await fileExists(target)) conflicts.push(target.fsPath);
  if (conflicts.length) {
    void vscode.window.showErrorMessage(`Copperline project not created; these files already exist: ${conflicts.join(", ")}`);
    return;
  }

  await vscode.workspace.fs.createDirectory(destination);
  await vscode.workspace.fs.createDirectory(vscodeDir);
  const template = vscode.Uri.file(context.asAbsolutePath(path.join("templates", "project")));
  for (const name of relativeFiles) {
    await vscode.workspace.fs.copy(vscode.Uri.joinPath(template, name), vscode.Uri.joinPath(destination, name));
  }
  const launch = {
    version: "0.2.0",
    configurations: [
      launchConfiguration("A500 KS1.3 512K+512K", {
        model: "A500", cpu: "68000", chip: "512K", slow: "512K", rom: "${input:kick13}",
      }),
      launchConfiguration("A1200", { model: "A1200" }),
      launchConfiguration("A1200-FAST", { model: "A1200", fast: "8M" }),
      launchConfiguration("A1200-030", { model: "A1200", cpu: "68030", fast: "8M" }),
      launchConfiguration("A3000", { model: "A3000" }),
      launchConfiguration("A4000", { model: "A4000" }),
    ],
    inputs: [{ id: "kick13", type: "promptString", description: "Path to a licensed Kickstart 1.3 ROM" }],
  };
  const taskEnvironment = {};
  if (bartmanBin) {
    taskEnvironment.PATH = [path.join(bartmanBin, "opt", "bin"), bartmanBin, "${env:PATH}"].join(path.delimiter);
  }
  const makeCommand = bartmanBin && process.platform === "win32" ? "gnumake.exe" : "make";
  const task = (label, target, group) => ({
    label, type: "process", command: makeCommand, args: [`TOOLCHAIN=${toolchain.id}`, target],
    options: { cwd: "${workspaceFolder}", env: taskEnvironment },
    problemMatcher: ["$gcc"], group,
  });
  const tasks = { version: "2.0.0", tasks: [
    task("Copperline: Build", "all", { kind: "build", isDefault: true }),
    task("Copperline: Clean", "clean"),
    task("Copperline: Convert EXE to ADF", "adf"),
    task("Copperline: Profile File Size", "size"),
  ] };
  const encoder = new TextEncoder();
  await vscode.workspace.fs.writeFile(vscode.Uri.joinPath(vscodeDir, "launch.json"), encoder.encode(`${JSON.stringify(launch, null, 2)}\n`));
  await vscode.workspace.fs.writeFile(vscode.Uri.joinPath(vscodeDir, "tasks.json"), encoder.encode(`${JSON.stringify(tasks, null, 2)}\n`));
  void vscode.window.showInformationMessage(`Copperline Amiga project created with ${toolchain.label}.`);
}

function activate(context) {
  const factory = { createDebugAdapterDescriptor() {
    const settings = vscode.workspace.getConfiguration("copperline");
    const env = {};
    const emulator = settings.get("emulatorExecutable");
    if (emulator) env.COPPERLINE_BIN = emulator;
    const options = Object.keys(env).length ? { env } : undefined;
    return new vscode.DebugAdapterExecutable(ctlExecutable(), ["--dap"], options);
  } };
  context.subscriptions.push(vscode.debug.registerDebugAdapterDescriptorFactory("copperline", factory));

  const provider = { resolveDebugConfiguration(folder, config) {
    if (!config.type && !config.request && !config.name) {
      const editor = vscode.window.activeTextEditor;
      if (editor) Object.assign(config, {
        type: "copperline", request: "launch", name: "Run in Copperline",
        program: editor.document.fileName.replace(/\.[^./\\]+$/, ""), stopOnEntry: true,
      });
    }
    if (!config.program && config.request === "launch") {
      return vscode.window.showInformationMessage("Copperline: set \"program\" to the Amiga executable to run.").then(() => undefined);
    }
    return config;
  } };
  context.subscriptions.push(vscode.debug.registerDebugConfigurationProvider("copperline", provider));

  const registers = new CustomRegisterProvider();
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("copperline.customRegisters", registers), registers.changed,
    vscode.debug.onDidChangeActiveDebugSession(() => void registers.refresh()),
    vscode.debug.onDidChangeActiveStackItem(() => void registers.refresh()),
    vscode.debug.onDidStartDebugSession(() => void registers.refresh()),
    vscode.debug.onDidTerminateDebugSession(() => void registers.refresh())
  );

  async function sessionRequest(method, args, what) {
    const session = vscode.debug.activeDebugSession;
    if (!session || session.type !== "copperline") {
      void vscode.window.showInformationMessage(`Copperline: start or attach a debug session before ${what}.`);
      return undefined;
    }
    try { return await session.customRequest(method, args); }
    catch (error) {
      void vscode.window.showErrorMessage(`Copperline ${what} failed: ${error}`);
      return undefined;
    }
  }
  async function captureProfile(frames) {
    const result = await sessionRequest("copperline/profile", { frames }, "profile");
    if (result && result.path) await vscode.commands.executeCommand("vscode.open", vscode.Uri.file(result.path));
  }
  async function showNative(window) {
    await sessionRequest("copperline/ui.show", { window }, `open ${window}`);
  }
  async function exe2adf() {
    const program = await chooseProgram("Select an Amiga executable");
    if (!program) return;
    const output = await vscode.window.showSaveDialog({
      title: "Write Amiga disk image", defaultUri: program.with({ path: `${program.path}.adf` }),
      filters: { "Amiga disk images": ["adf"] },
    });
    if (!output) return;
    const boot = await vscode.window.showQuickPick(["Bootable", "Data disk"], { title: "ADF type" });
    if (!boot) return;
    try {
      const args = ["exe2adf", program.fsPath, "--out", output.fsPath];
      if (boot === "Bootable") args.push("--boot");
      await runCtl(args, path.dirname(program.fsPath));
      void vscode.window.showInformationMessage(`Wrote ${output.fsPath}`);
    } catch (error) { void vscode.window.showErrorMessage(`Copperline EXE to ADF failed: ${error}`); }
  }
  async function sizeReport() {
    const program = await chooseProgram("Select an Amiga executable to profile");
    if (!program) return;
    const output = await vscode.window.showSaveDialog({
      title: "Write size profile", defaultUri: program.with({ path: `${program.path}.size.cpuprofile` }),
      filters: { "CPU profiles": ["cpuprofile"] },
    });
    if (!output) return;
    const args = ["size-report", program.fsPath, "--out", output.fsPath];
    const elf = vscode.Uri.file(`${program.fsPath}.elf`);
    if (await fileExists(elf)) args.push("--elf", elf.fsPath);
    try {
      await runCtl(args, path.dirname(program.fsPath));
      await vscode.commands.executeCommand("vscode.open", output);
    } catch (error) { void vscode.window.showErrorMessage(`Copperline size profile failed: ${error}`); }
  }

  context.subscriptions.push(
    vscode.commands.registerCommand("copperline.profile", () => captureProfile(1)),
    vscode.commands.registerCommand("copperline.profileMulti", async () => {
      const value = await vscode.window.showInputBox({
        title: "Copperline Profile (Multi)", prompt: "Number of emulated frames to capture", value: "60",
        validateInput(text) {
          const frames = Number(text);
          return Number.isInteger(frames) && frames >= 1 && frames <= 100000 ? undefined : "Enter an integer from 1 to 100000";
        },
      });
      if (value !== undefined) await captureProfile(Number(value));
    }),
    vscode.commands.registerCommand("copperline.showDebugger", () => showNative("debugger")),
    vscode.commands.registerCommand("copperline.showConsole", () => showNative("console")),
    vscode.commands.registerCommand("copperline.showAnalyzer", () => showNative("analyzer")),
    vscode.commands.registerCommand("copperline.refreshCustomRegisters", () => registers.refresh()),
    vscode.commands.registerCommand("copperline.initProject", () => initProject(context)),
    vscode.commands.registerCommand("copperline.exe2adf", exe2adf),
    vscode.commands.registerCommand("copperline.sizeReport", sizeReport)
  );
  void registers.refresh();
}

function deactivate() {}
module.exports = { activate, deactivate };
