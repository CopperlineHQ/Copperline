// SPDX-License-Identifier: GPL-3.0-or-later
//
// The Copperline debug extension: everything is in package.json except
// the one thing a declarative contribution cannot do, which is point VS
// Code at a user-installed copperline-ctl. No build step; plain JS.

const vscode = require("vscode");

function activate(context) {
  const factory = {
    createDebugAdapterDescriptor(session) {
      const settings = vscode.workspace.getConfiguration("copperline");
      const ctl = settings.get("ctlExecutable") || "copperline-ctl";
      const env = {};
      // A launch configuration's own "copperline" wins in the adapter;
      // the setting reaches it as the environment the bridge consults.
      const emulator = settings.get("emulatorExecutable");
      if (emulator) {
        env.COPPERLINE_BIN = emulator;
      }
      const options = Object.keys(env).length ? { env } : undefined;
      return new vscode.DebugAdapterExecutable(ctl, ["--dap"], options);
    },
  };
  context.subscriptions.push(
    vscode.debug.registerDebugAdapterDescriptorFactory("copperline", factory)
  );

  const provider = {
    // F5 with no launch.json: debug the active editor's directory's
    // executable named after the file.
    resolveDebugConfiguration(folder, config) {
      if (!config.type && !config.request && !config.name) {
        const editor = vscode.window.activeTextEditor;
        if (editor) {
          const file = editor.document.fileName;
          const stem = file.replace(/\.[^./\\]+$/, "");
          config.type = "copperline";
          config.request = "launch";
          config.name = "Run in Copperline";
          config.program = stem;
          config.stopOnEntry = true;
        }
      }
      if (!config.program && config.request === "launch") {
        return vscode.window
          .showInformationMessage("Copperline: set \"program\" to the Amiga executable to run.")
          .then(() => undefined);
      }
      return config;
    },
  };
  context.subscriptions.push(
    vscode.debug.registerDebugConfigurationProvider("copperline", provider)
  );

  async function captureProfile(frames) {
    const session = vscode.debug.activeDebugSession;
    if (!session || session.type !== "copperline") {
      void vscode.window.showInformationMessage(
        "Copperline: start or attach a debug session before profiling."
      );
      return;
    }
    try {
      const result = await session.customRequest("copperline/profile", { frames });
      if (result && result.path) {
        await vscode.commands.executeCommand(
          "vscode.open",
          vscode.Uri.file(result.path)
        );
      }
    } catch (error) {
      void vscode.window.showErrorMessage(`Copperline profile failed: ${error}`);
    }
  }

  context.subscriptions.push(
    vscode.commands.registerCommand("copperline.profile", () => captureProfile(1)),
    vscode.commands.registerCommand("copperline.profileMulti", async () => {
      const value = await vscode.window.showInputBox({
        title: "Copperline Profile (Multi)",
        prompt: "Number of emulated frames to capture",
        value: "60",
        validateInput(text) {
          const frames = Number(text);
          return Number.isInteger(frames) && frames >= 1 && frames <= 100000
            ? undefined
            : "Enter an integer from 1 to 100000";
        },
      });
      if (value !== undefined) {
        await captureProfile(Number(value));
      }
    })
  );
}

function deactivate() {}

module.exports = { activate, deactivate };
