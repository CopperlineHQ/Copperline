# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise the web player kit packer without a wasm build or a browser."""

import base64
import contextlib
import hashlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import zlib


TOOL = Path(__file__).resolve().parents[1] / "publish-web"
loader = importlib.machinery.SourceFileLoader("publish_web", str(TOOL))
spec = importlib.util.spec_from_loader(loader.name, loader)
publish_web = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = publish_web
loader.exec_module(publish_web)

KitError = publish_web.KitError

SHELL = """<!doctype html>
<html><head><title>%%TITLE%%</title><link rel="icon" href="%%FAVICON%%">
<style>body { margin: 0; }</style></head>
<body><h1>%%TITLE%%</h1>
%%CONFIG%%
%%ASSETS%%
<script>
%%LOADER%%
</script>
</body></html>
"""

MODULES = {
    "./kit-player.js": (
        "import init from './pkg/copperline_web.js';\n"
        "import { gate } from './audio-gate.js';\n"
        "export const start = () => init(gate);\n"
    ),
    "./audio-gate.js": "export const gate = 1;\n",
    "./pkg/copperline_web.js": "export default function init() {}\n",
}

MANIFEST = """
[game]
title = "Test <Game> & Co"
id = "test-game"
version = "2.1"

[payload]
adf = "game.adf"

[machine]
model = "A500"
"""

# Deterministic bytes that do not compress to nothing, so a truncated or
# reordered block cannot pass for the original.
def blob(seed: int, size: int) -> bytes:
    out = bytearray()
    counter = 0
    while len(out) < size:
        out += hashlib.sha256(f"{seed}:{counter}".encode()).digest()
        counter += 1
    return bytes(out[:size])


ROM = blob(1, 0x80000)
EXT = blob(2, 0x80000)
DISK = blob(3, 901120)
WASM = b"\0asm" + blob(4, 5000)


def manifest_dict(text: str = MANIFEST) -> dict:
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "game.toml"
        path.write_text(text)
        return publish_web.load_manifest(path)


def build(**overrides):
    args = dict(
        manifest=manifest_dict(),
        shell=SHELL,
        loader="/* loader */",
        modules=dict(MODULES),
        wasm=WASM,
        worklet=b"registerProcessor('x', class {});",
        rom=ROM,
        ext_rom=EXT,
        rom_label="AROS",
        disks=[("game.adf", DISK)],
        notices=[("about", "About", b"notice text\n")],
        version="9.9.9",
        ref="v9.9.9-1-gabc",
    )
    args.update(overrides)
    return publish_web.build_page(**args)


class Sizes(unittest.TestCase):
    def test_suffixes(self):
        self.assertEqual(publish_web.parse_size("65536"), 65536)
        self.assertEqual(publish_web.parse_size("1500K"), 1500 * 1024)
        self.assertEqual(publish_web.parse_size("48m"), 48 << 20)
        self.assertEqual(publish_web.parse_size(" 2G "), 2 << 30)
        self.assertEqual(publish_web.parse_size("16MiB"), 16 << 20)

    def test_rejects_nonsense(self):
        for text in ("", "lots", "-5M", "1.5M", "0", "12X"):
            with self.subTest(text=text), self.assertRaises(KitError):
                publish_web.parse_size(text)


class Manifest(unittest.TestCase):
    def test_accepts_the_example_manifest(self):
        example = TOOL.parents[1] / "crates/copperline-player/game.example.toml"
        manifest = publish_web.load_manifest(example)
        self.assertEqual(manifest["game"]["id"], "copperline-example")

    def reject(self, text: str, expect: str):
        with self.assertRaises(KitError) as caught:
            manifest_dict(text)
        self.assertIn(expect, str(caught.exception))

    def test_unreadable_and_unparsable(self):
        with self.assertRaises(KitError):
            publish_web.load_manifest(Path("/nonexistent/game.toml"))
        self.reject("[game\n", "cannot read manifest")

    def test_missing_sections_and_unknown_keys(self):
        self.reject('[game]\ntitle = "x"\nid = "x"\n', "missing its [payload]")
        self.reject(MANIFEST + '\n[web]\nx = 1\n', "unknown [web] section")
        self.reject(MANIFEST.replace('id = "test-game"', 'id = "test-game"\nextra = 1'),
                    "unknown key 'extra'")

    def test_game_identity(self):
        self.reject(MANIFEST.replace('"Test <Game> & Co"', '"  "'), "title must not be empty")
        for bad in ("", "has space", "../up", "-lead", "a/b"):
            with self.subTest(id=bad):
                self.reject(MANIFEST.replace('"test-game"', json.dumps(bad)),
                            "id must be a plain directory name")

    def test_model(self):
        self.reject(MANIFEST.replace('"A500"', '"A9000"'), "is not a Copperline machine")
        manifest = manifest_dict(MANIFEST.replace('"A500"', '"a500+"'))
        self.assertEqual(publish_web.normalize_model(manifest["machine"]["model"]), "A500PLUS")

    def test_payload_rules(self):
        self.reject(MANIFEST.replace('adf = "game.adf"', ""), "exactly one of cd, adf, or run")
        self.reject(MANIFEST.replace('adf = "game.adf"', 'adf = "game.adf"\ncd = "g.iso"'),
                    "exactly one of cd, adf, or run")
        self.reject(MANIFEST.replace('"game.adf"', '"disks/game.adf"'), "bare file name")
        self.reject(MANIFEST.replace('"game.adf"', '".."'), "bare file name")
        self.reject(MANIFEST.replace('adf = "game.adf"', 'adf = "game.adf"\nsha256 = "abc"'),
                    "64 hex characters")
        self.reject(
            MANIFEST.replace(
                'adf = "game.adf"',
                'run = { files = "amiga", executable = "Game" }\nsha256 = "' + "a" * 64 + '"',
            ),
            "not run directories",
        )

    def test_display_and_features(self):
        self.reject(MANIFEST + '\n[display]\nshader = "plasma"\n', "shader must be one of")
        self.reject(MANIFEST + '\n[display]\nbezel = "wood"\n', "bezel must be one of")
        self.reject(MANIFEST + '\n[display]\nfullscreen = "yes"\n', "fullscreen must be")
        self.reject(MANIFEST + '\n[features]\nsave_states = 1\n', "save_states must be")
        self.reject(MANIFEST + '\n[branding]\nicon = 7\n', "icon must be a file path")


class WebSupport(unittest.TestCase):
    def test_floppy_title_packs_clean(self):
        self.assertEqual(publish_web.check_web_support(manifest_dict()), [])

    def test_cd_and_run_payloads_are_refused(self):
        cd = manifest_dict(MANIFEST.replace('adf = "game.adf"', 'cd = "game.iso"'))
        with self.assertRaises(KitError) as caught:
            publish_web.check_web_support(cd)
        self.assertIn("floppy (adf) payloads only", str(caught.exception))
        run = manifest_dict(
            MANIFEST.replace('adf = "game.adf"', 'run = { files = "amiga", executable = "G" }')
        )
        with self.assertRaises(KitError):
            publish_web.check_web_support(run)

    def test_cd_machines_and_ram_overrides_are_refused(self):
        with self.assertRaises(KitError) as caught:
            publish_web.check_web_support(manifest_dict(MANIFEST.replace('"A500"', '"CD32"')))
        self.assertIn("no CD drive", str(caught.exception))
        with self.assertRaises(KitError) as caught:
            publish_web.check_web_support(manifest_dict(MANIFEST + 'fast = "8M"\n'))
        self.assertIn("[machine] fast is not supported", str(caught.exception))

    def test_unhonoured_settings_warn(self):
        warnings = publish_web.check_web_support(
            manifest_dict(
                MANIFEST.replace('"A500"', '"A3000"')
                + '\n[display]\nshader = "crt"\nbezel = "1084"\n'
            )
        )
        self.assertEqual(len(warnings), 2)
        self.assertIn("A3000", warnings[0])
        self.assertIn("shader and bezel are not drawn", warnings[1])
        quiet = publish_web.check_web_support(
            manifest_dict(MANIFEST + '\n[display]\nshader = "none"\nbezel = "off"\n')
        )
        self.assertEqual(quiet, [])


class ModuleGraph(unittest.TestCase):
    def test_scan_finds_every_relative_import_form(self):
        source = (
            "import a from './a.js';\n"
            'import { b } from "./dir/b.js";\n'
            "import './side-effect.js';\n"
            "export { c } from '../c.js';\n"
            "const d = await import('./d.js');\n"
            "import fs from 'node:fs';\n"
            "const url = new URL('x.wasm', import.meta.url);\n"
        )
        self.assertEqual(
            publish_web.scan_imports(source),
            ["./a.js", "./dir/b.js", "./side-effect.js", "../c.js", "./d.js"],
        )

    def test_order_is_dependencies_first(self):
        order = publish_web.order_modules(MODULES, "./kit-player.js")
        self.assertEqual(order[-1], "./kit-player.js")
        self.assertEqual(set(order), set(MODULES))
        shared = {
            "./e.js": "import './a.js'; import './b.js';",
            "./a.js": "import './c.js';",
            "./b.js": "import './c.js';",
            "./c.js": "",
            "./unused.js": "",
        }
        order = publish_web.order_modules(shared, "./e.js")
        self.assertEqual(order.count("./c.js"), 1)
        self.assertNotIn("./unused.js", order)
        for name, source in shared.items():
            if name in order:
                for dep in publish_web.scan_imports(source):
                    self.assertLess(order.index(dep), order.index(name))

    def test_missing_module_and_cycle(self):
        with self.assertRaises(KitError) as caught:
            publish_web.order_modules({"./e.js": "import './gone.js';"}, "./e.js")
        self.assertIn("./gone.js is missing (imported by ./e.js)", str(caught.exception))
        with self.assertRaises(KitError) as caught:
            publish_web.order_modules(
                {"./a.js": "import './b.js';", "./b.js": "import './a.js';"}, "./a.js"
            )
        self.assertIn("cycle", str(caught.exception))

    def test_collect_reads_page_and_pkg_modules(self):
        with tempfile.TemporaryDirectory() as tmp:
            www = Path(tmp) / "www"
            pkg = Path(tmp) / "pkg"
            www.mkdir()
            pkg.mkdir()
            (www / "kit-player.js").write_text(MODULES["./kit-player.js"])
            (www / "audio-gate.js").write_text(MODULES["./audio-gate.js"])
            (pkg / "copperline_web.js").write_text(MODULES["./pkg/copperline_web.js"])
            self.assertEqual(publish_web.collect_modules("./kit-player.js", www, pkg), MODULES)
            (www / "audio-gate.js").write_text("import '../outside.js';")
            with self.assertRaises(KitError) as caught:
                publish_web.collect_modules("./kit-player.js", www, pkg)
            self.assertIn("leaves the page directory", str(caught.exception))
            (www / "audio-gate.js").unlink()
            with self.assertRaises(KitError) as caught:
                publish_web.collect_modules("./kit-player.js", www, pkg)
            self.assertIn("cannot read module ./audio-gate.js", str(caught.exception))

    def test_the_real_shell_modules_resolve(self):
        # The page modules as they are in the tree, with a stand-in for the
        # generated glue: catches an import the packer could not place.
        with tempfile.TemporaryDirectory() as tmp:
            pkg = Path(tmp)
            (pkg / "copperline_web.js").write_text("export default 0; export const WebEmu = 0;")
            sources = publish_web.collect_modules(
                publish_web.ENTRY_MODULE, publish_web.WWW, pkg
            )
        order = publish_web.order_modules(sources, publish_web.ENTRY_MODULE)
        self.assertEqual(order[-1], publish_web.ENTRY_MODULE)
        self.assertIn("./pkg/copperline_web.js", order)
        self.assertIn("./input-map.js", order)
        # Neither try.js nor its netplay stack belongs in a player page.
        self.assertNotIn("./try.js", order)
        self.assertFalse([name for name in order if "netplay" in name])


class Assets(unittest.TestCase):
    def test_block_round_trip(self):
        data = blob(9, 100_000)
        block = publish_web.encode_asset("disk/0", "binary", data)
        self.assertIn('data-cl-asset="disk/0"', block)
        self.assertIn('data-cl-size="100000"', block)
        self.assertNotIn("data-cl-lazy", block)
        text = block[block.index(">") + 1:block.rindex("</script>")]
        self.assertTrue(all(len(line) <= publish_web.B64_LINE for line in text.split("\n")))
        self.assertEqual(zlib.decompress(base64.b64decode(text)), data)

    def test_empty_and_lazy_blocks(self):
        block = publish_web.encode_asset("notice/x", "text", b"", lazy=True)
        self.assertIn(" data-cl-lazy>", block)
        _, assets, _ = publish_web.read_page(
            '<script type="application/json" id="cl-config">{}</script>' + block
        )
        self.assertEqual(assets["notice/x"]["data"], b"")
        self.assertTrue(assets["notice/x"]["lazy"])

    def test_asset_names_are_plain(self):
        for name in ('a"b', "a b", "<x>", ""):
            with self.subTest(name=name), self.assertRaises(KitError):
                publish_web.encode_asset(name, "binary", b"x")

    def test_favicon(self):
        self.assertEqual(publish_web.favicon_url(None), "data:,")
        png = publish_web.PNG_MAGIC + b"rest"
        url = publish_web.favicon_url(png)
        self.assertTrue(url.startswith("data:image/png;base64,"))
        self.assertEqual(base64.b64decode(url.split(",", 1)[1]), png)
        with self.assertRaises(KitError):
            publish_web.favicon_url(b"GIF89a")

    def test_json_cannot_close_its_script_element(self):
        text = publish_web.json_for_script({"t": "</script><b>&amp;"})
        self.assertNotIn("<", text)
        self.assertNotIn(">", text)
        self.assertNotIn("&", text)
        self.assertEqual(json.loads(text), {"t": "</script><b>&amp;"})


class Page(unittest.TestCase):
    def test_every_payload_round_trips(self):
        page, packed = build()
        config, assets, references = publish_web.read_page(page)
        self.assertEqual(assets["rom/main"]["data"], ROM)
        self.assertEqual(assets["rom/ext"]["data"], EXT)
        self.assertEqual(assets["disk/0"]["data"], DISK)
        self.assertEqual(assets[publish_web.WASM_ASSET]["data"], WASM)
        self.assertEqual(assets["audio-worklet.js"]["kind"], "worklet")
        for name, source in MODULES.items():
            self.assertEqual(assets[name]["kind"], "module")
            self.assertEqual(assets[name]["data"].decode(), source)
        self.assertEqual(assets["notice/about"]["data"], b"notice text\n")
        self.assertTrue(assets["notice/about"]["lazy"])
        self.assertFalse(assets["disk/0"]["lazy"])
        self.assertEqual(set(assets), set(packed))
        for name, data in packed.items():
            self.assertEqual(assets[name]["size"], len(data))
        publish_web.check_self_contained(page, packed)
        self.assertEqual(references, [("link", "href", "data:,")])
        self.assertEqual(config["format"], publish_web.FORMAT)
        self.assertEqual(config["modules"]["entry"], "./kit-player.js")
        self.assertEqual(config["modules"]["order"][-1], "./kit-player.js")
        self.assertEqual(config["disks"], [
            {"asset": "disk/0", "name": "game.adf", "sha256": hashlib.sha256(DISK).hexdigest()},
        ])
        self.assertEqual(config["rom"], {"main": "rom/main", "ext": "rom/ext", "label": "AROS"})
        self.assertEqual(config["copperline"]["version"], "9.9.9")
        self.assertEqual(config["copperline"]["ref"], "v9.9.9-1-gabc")

    def test_config_carries_the_manifest(self):
        manifest = manifest_dict(
            MANIFEST + '\n[display]\nfullscreen = true\n\n[features]\nsave_states = true\n'
        )
        page, _ = build(manifest=manifest, drives=2, video="NTSC", joystick="off")
        config, _, _ = publish_web.read_page(page)
        self.assertEqual(config["game"], {"title": "Test <Game> & Co", "id": "test-game",
                                          "version": "2.1"})
        self.assertEqual(config["machine"], {"model": "A500", "video": "NTSC", "drives": 2})
        self.assertTrue(config["display"]["fullscreen"])
        self.assertTrue(config["features"]["save_states"])
        self.assertEqual(config["input"], {"joystick": "off"})

    def test_defaults(self):
        page, _ = build(manifest=manifest_dict(MANIFEST.replace('version = "2.1"\n', "")))
        config, _, _ = publish_web.read_page(page)
        self.assertEqual(config["game"]["version"], "1.0")
        self.assertEqual(config["machine"], {"model": "A500", "video": None, "drives": 1})
        self.assertFalse(config["display"]["fullscreen"])
        self.assertFalse(config["features"]["save_states"])
        self.assertEqual(config["input"], {"joystick": "keys"})

    def test_title_is_escaped_in_markup(self):
        page, _ = build()
        self.assertIn("<title>Test &lt;Game&gt; &amp; Co</title>", page)
        self.assertNotIn("<Game>", page)

    def test_a_slot_name_in_a_title_is_not_expanded(self):
        manifest = manifest_dict(MANIFEST.replace("Test <Game> & Co", "100%%LOADER%% fun"))
        page, packed = build(manifest=manifest)
        self.assertIn("<title>100%%LOADER%% fun</title>", page)
        self.assertEqual(page.count("/* loader */"), 1)
        publish_web.check_self_contained(page, packed)

    def test_several_disks_and_no_ext_rom(self):
        second = blob(7, 901120)
        page, packed = build(disks=[("game.adf", DISK), ("data.adf", second)], ext_rom=None)
        config, assets, _ = publish_web.read_page(page)
        self.assertEqual([d["name"] for d in config["disks"]], ["game.adf", "data.adf"])
        self.assertEqual(assets["disk/1"]["data"], second)
        self.assertNotIn("rom/ext", assets)
        self.assertIsNone(config["rom"]["ext"])
        publish_web.check_self_contained(page, packed)

    def test_build_id_follows_the_contents(self):
        ids = set()
        for overrides in (
            {},
            {"disks": [("game.adf", blob(8, 901120))]},
            {"rom": blob(5, 0x80000)},
            {"wasm": WASM + b"x"},
        ):
            config, _, _ = publish_web.read_page(build(**overrides)[0])
            ids.add(config["build"])
        self.assertEqual(len(ids), 4)
        again, _, _ = publish_web.read_page(build()[0])
        self.assertIn(again["build"], ids)

    def test_icon_becomes_the_favicon(self):
        png = publish_web.PNG_MAGIC + blob(6, 300)
        page, packed = build(icon=png)
        _, _, references = publish_web.read_page(page)
        self.assertEqual(len(references), 1)
        self.assertEqual(base64.b64decode(references[0][2].split(",", 1)[1]), png)
        publish_web.check_self_contained(page, packed)

    def test_bad_inputs(self):
        cases = [
            ({"disks": []}, "at least one disk"),
            ({"disks": [("game.adf", b"")]}, "is empty"),
            ({"drives": 0}, "--drives must be between 1 and 4"),
            ({"drives": 5}, "--drives must be between 1 and 4"),
            ({"joystick": "paddle"}, "--joystick must be one of"),
            ({"video": "SECAM"}, "--video must be PAL or NTSC"),
            ({"rom": b"short"}, "expected a 256K, 512K or 1M image"),
            ({"ext_rom": blob(1, 1000)}, "expected a 256K, 512K or 1M image"),
            ({"loader": "x = '</SCRIPT>';"}, "closing script tag"),
            ({"shell": SHELL.replace("%%ASSETS%%", "")}, "no %%ASSETS%% slot"),
            ({"modules": {"./kit-player.js": "import './lost.js';"}}, "./lost.js is missing"),
            ({"icon": b"not a png"}, "must be a PNG"),
        ]
        for overrides, expect in cases:
            with self.subTest(expect=expect), self.assertRaises(KitError) as caught:
                build(**overrides)
            self.assertIn(expect, str(caught.exception))

    def test_256k_rom_is_accepted(self):
        build(rom=blob(1, 0x40000), ext_rom=None)

    def test_self_check_catches_damage_and_outside_references(self):
        page, packed = build()
        with self.assertRaises(KitError) as caught:
            publish_web.check_self_contained(page, dict(packed, **{"disk/0": DISK[:-1] + b"\0"}))
        self.assertIn("disk/0 did not survive", str(caught.exception))
        with self.assertRaises(KitError) as caught:
            publish_web.check_self_contained(page, dict(packed, extra=b"x"))
        self.assertIn("extra is missing", str(caught.exception))
        for injected in (
            '<img src="https://example.com/a.png">',
            '<link rel="stylesheet" href="style.css">',
            '<script src="/x.js"></script>',
        ):
            with self.subTest(injected=injected), self.assertRaises(KitError) as caught:
                publish_web.check_self_contained(page.replace("<h1>", injected + "<h1>"), packed)
            self.assertIn("the page references", str(caught.exception))
        styled = page.replace("body { margin: 0; }", "body { background: url(bg.png); }")
        with self.assertRaises(KitError) as caught:
            publish_web.check_self_contained(styled, packed)
        self.assertIn("stylesheet references", str(caught.exception))

    def test_the_real_shell_and_loader_pack(self):
        page, packed = build(
            shell=(publish_web.WWW / "kit-shell.html").read_text(),
            loader=(publish_web.WWW / "kit-loader.js").read_text(),
        )
        publish_web.check_self_contained(page, packed)
        self.assertNotIn("%%", page.split("<body>", 1)[1].split("<script", 1)[0])


class BundleBuild(unittest.TestCase):
    def test_pin_is_read_from_the_web_crate(self):
        self.assertRegex(publish_web.pinned_bindgen_version(), r"^\d+\.\d+\.\d+$")

    def test_missing_or_mismatched_bindgen_stops_before_cargo(self):
        want = publish_web.pinned_bindgen_version()
        with patch.object(publish_web.shutil, "which", return_value=None), \
                patch.object(publish_web.subprocess, "run") as run:
            with self.assertRaises(KitError) as caught:
                publish_web.build_pkg(Path("/nonexistent/pkg"), "release")
            run.assert_not_called()
        self.assertIn(f"cargo install wasm-bindgen-cli --version {want}", str(caught.exception))

        calls = []

        def fake_run(argv, **kwargs):
            calls.append(argv)
            return SimpleNamespace(stdout="wasm-bindgen 0.0.1\n")

        with patch.object(publish_web.shutil, "which", return_value="/bin/wasm-bindgen"), \
                patch.object(publish_web.subprocess, "run", side_effect=fake_run):
            with self.assertRaises(KitError) as caught:
                publish_web.build_pkg(Path("/nonexistent/pkg"), "release")
        self.assertIn(f"wasm-bindgen 0.0.1 is installed but the web crate pins {want}",
                      str(caught.exception))
        self.assertEqual(calls, [["/bin/wasm-bindgen", "--version"]])

    def test_build_runs_cargo_bindgen_and_wasm_opt(self):
        want = publish_web.pinned_bindgen_version()
        calls = []

        def fake_run(argv, **kwargs):
            calls.append((argv, kwargs))
            return SimpleNamespace(stdout=f"wasm-bindgen {want}\n")

        tools = {"wasm-bindgen": "/bin/wasm-bindgen", "wasm-opt": "/bin/wasm-opt"}
        with patch.object(publish_web.shutil, "which", side_effect=tools.get), \
                patch.object(publish_web.subprocess, "run", side_effect=fake_run), \
                patch.dict(publish_web.os.environ, {"RUSTFLAGS": "-C debuginfo=0"}):
            publish_web.build_pkg(Path("/out/pkg"), "ci")
        cargo, cargo_kwargs = calls[1]
        self.assertEqual(cargo[:4], ["cargo", "build", "-p", "copperline-web"])
        self.assertIn("--locked", cargo)
        self.assertEqual(cargo[cargo.index("--profile") + 1], "ci")
        self.assertEqual(cargo[cargo.index("--target") + 1], "wasm32-unknown-unknown")
        self.assertEqual(cargo_kwargs["env"]["RUSTFLAGS"],
                         "-C debuginfo=0 -C target-feature=+simd128")
        bindgen = calls[2][0]
        self.assertEqual(bindgen[:5],
                         ["/bin/wasm-bindgen", "--target", "web", "--out-dir", "/out/pkg"])
        self.assertTrue(bindgen[5].endswith("target/wasm32-unknown-unknown/ci/copperline_web.wasm"))
        self.assertEqual(calls[3][0], ["/bin/wasm-opt", "-O3", "/out/pkg/copperline_web_bg.wasm",
                                       "-o", "/out/pkg/copperline_web_bg.wasm"])

        # The release profile is spelled the way cargo wants it, and a
        # missing wasm-opt is a warning, not a failure.
        calls.clear()
        stderr = io.StringIO()
        with patch.object(publish_web.shutil, "which",
                          side_effect={"wasm-bindgen": "/bin/wasm-bindgen"}.get), \
                patch.object(publish_web.subprocess, "run", side_effect=fake_run), \
                contextlib.redirect_stderr(stderr):
            publish_web.build_pkg(Path("/out/pkg"), "release")
        self.assertIn("--release", calls[1][0])
        self.assertNotIn("--profile", calls[1][0])
        self.assertEqual(len(calls), 3)
        self.assertIn("wasm-opt (binaryen) is not installed", stderr.getvalue())

    def test_a_failed_build_is_reported(self):
        want = publish_web.pinned_bindgen_version()

        def fake_run(argv, **kwargs):
            if argv[0] == "cargo":
                raise publish_web.subprocess.CalledProcessError(101, argv)
            return SimpleNamespace(stdout=f"wasm-bindgen {want}\n")

        with patch.object(publish_web.shutil, "which", return_value="/bin/wasm-bindgen"), \
                patch.object(publish_web.subprocess, "run", side_effect=fake_run):
            with self.assertRaises(KitError) as caught:
                publish_web.build_pkg(Path("/out/pkg"), "release")
        self.assertIn("building the wasm bundle failed", str(caught.exception))


class CommandLine(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        (self.dir / "game.toml").write_text(MANIFEST)
        (self.dir / "game.adf").write_bytes(DISK)
        self.pkg = self.dir / "pkg"
        self.pkg.mkdir()
        (self.pkg / "copperline_web.js").write_text(
            "export default async function init() {}\nexport class WebEmu {}\n"
        )
        (self.pkg / "copperline_web_bg.wasm").write_bytes(WASM)
        self.out = self.dir / "out" / "page.html"

    def run_tool(self, *extra, manifest="game.toml"):
        argv = ["--manifest", str(self.dir / manifest), "--skip-build", "--pkg", str(self.pkg),
                "--out", str(self.out), *extra]
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = publish_web.main(argv)
        return status, stdout.getvalue(), stderr.getvalue()

    def test_writes_one_self_contained_file(self):
        status, stdout, stderr = self.run_tool()
        self.assertEqual((status, stderr), (0, ""))
        self.assertIn(str(self.out), stdout)
        self.assertEqual([p.name for p in self.out.parent.iterdir()], ["page.html"])
        config, assets, references = publish_web.read_page(self.out.read_text())
        self.assertEqual(assets["disk/0"]["data"], DISK)
        self.assertEqual(assets[publish_web.WASM_ASSET]["data"], WASM)
        aros = publish_web.REPO / "assets/aros"
        self.assertEqual(assets["rom/main"]["data"], (aros / "aros-amiga-m68k-rom.bin").read_bytes())
        self.assertEqual(assets["rom/ext"]["data"], (aros / "aros-amiga-m68k-ext.bin").read_bytes())
        self.assertIn("AROS", config["rom"]["label"])
        self.assertEqual(
            assets["notice/copperline-license"]["data"], (publish_web.REPO / "LICENSE").read_bytes()
        )
        self.assertEqual(assets["notice/aros-license"]["data"], (aros / "LICENSE").read_bytes())
        self.assertEqual(references, [("link", "href", "data:,")])
        self.assertEqual(config["copperline"]["version"], publish_web.copperline_version())

    def test_options_reach_the_page(self):
        (self.dir / "two.adf").write_bytes(blob(11, 901120))
        status, _, _ = self.run_tool(
            "--disk", str(self.dir / "two.adf"), "--drives", "2", "--video", "NTSC",
            "--joystick", "cd32",
        )
        self.assertEqual(status, 0)
        config, assets, _ = publish_web.read_page(self.out.read_text())
        self.assertEqual(config["machine"], {"model": "A500", "video": "NTSC", "drives": 2})
        self.assertEqual(config["input"], {"joystick": "cd32"})
        self.assertEqual([d["name"] for d in config["disks"]], ["game.adf", "two.adf"])
        self.assertEqual(len(assets["disk/1"]["data"]), 901120)

    def test_size_limit(self):
        status, _, stderr = self.run_tool("--max-size", "100K")
        self.assertEqual(status, 1)
        self.assertIn("over the --max-size limit", stderr)
        self.assertFalse(self.out.exists())
        status, _, stderr = self.run_tool("--max-size", "huge")
        self.assertEqual(status, 1)
        self.assertIn("cannot read 'huge' as a size", stderr)

    def test_sha256_pin(self):
        pinned = MANIFEST.replace(
            'adf = "game.adf"', f'adf = "game.adf"\nsha256 = "{hashlib.sha256(DISK).hexdigest()}"'
        )
        (self.dir / "pinned.toml").write_text(pinned)
        self.assertEqual(self.run_tool(manifest="pinned.toml")[0], 0)
        (self.dir / "game.adf").write_bytes(DISK[:-1] + b"\x55")
        status, _, stderr = self.run_tool(manifest="pinned.toml")
        self.assertEqual(status, 1)
        self.assertIn("does not match the manifest's sha256 pin", stderr)

    def test_payload_option_and_errors(self):
        elsewhere = self.dir / "elsewhere"
        elsewhere.mkdir()
        (elsewhere / "game.adf").write_bytes(blob(12, 901120))
        self.assertEqual(self.run_tool("--payload", str(elsewhere / "game.adf"))[0], 0)
        _, assets, _ = publish_web.read_page(self.out.read_text())
        self.assertEqual(assets["disk/0"]["data"], blob(12, 901120))

        (elsewhere / "other.adf").write_bytes(DISK)
        status, _, stderr = self.run_tool("--payload", str(elsewhere / "other.adf"))
        self.assertEqual(status, 1)
        self.assertIn("does not match the manifest's 'game.adf'", stderr)

        (self.dir / "game.adf").unlink()
        status, _, stderr = self.run_tool()
        self.assertEqual(status, 1)
        self.assertIn("cannot read payload", stderr)

    def test_duplicate_disk_names(self):
        status, _, stderr = self.run_tool("--disk", str(self.dir / "game.adf"))
        self.assertEqual(status, 1)
        self.assertIn("share a file name", stderr)

    def test_missing_bundle(self):
        (self.pkg / "copperline_web_bg.wasm").unlink()
        status, _, stderr = self.run_tool()
        self.assertEqual(status, 1)
        self.assertIn("no wasm bundle in", stderr)

    def test_own_rom_and_icon(self):
        (self.dir / "kick.rom").write_bytes(blob(13, 0x40000))
        (self.dir / "icon.png").write_bytes(publish_web.PNG_MAGIC + blob(14, 64))
        (self.dir / "branded.toml").write_text(MANIFEST + '\n[branding]\nicon = "icon.png"\n')
        status, _, stderr = self.run_tool("--rom", str(self.dir / "kick.rom"),
                                          manifest="branded.toml")
        self.assertEqual(status, 0)
        self.assertIn("make sure you may redistribute it", stderr)
        config, assets, references = publish_web.read_page(self.out.read_text())
        self.assertEqual(assets["rom/main"]["data"], blob(13, 0x40000))
        self.assertNotIn("rom/ext", assets)
        self.assertEqual(config["rom"]["label"], "kick.rom")
        self.assertTrue(references[0][2].startswith("data:image/png;base64,"))

        (self.dir / "branded.toml").write_text(MANIFEST + '\n[branding]\nicon = "gone.png"\n')
        status, _, stderr = self.run_tool(manifest="branded.toml")
        self.assertEqual(status, 1)
        self.assertIn("cannot read [branding] icon", stderr)

    def test_unsupported_manifests_fail_and_warnings_print(self):
        (self.dir / "cd.toml").write_text(MANIFEST.replace('adf = "game.adf"', 'cd = "g.iso"'))
        status, _, stderr = self.run_tool(manifest="cd.toml")
        self.assertEqual(status, 1)
        self.assertIn("floppy (adf) payloads only", stderr)
        (self.dir / "crt.toml").write_text(MANIFEST + '\n[display]\nshader = "crt"\n')
        status, _, stderr = self.run_tool(manifest="crt.toml")
        self.assertEqual(status, 0)
        self.assertIn("warning: [display] shader and bezel are not drawn", stderr)

    def test_argument_errors(self):
        for argv in (
            [],
            ["--manifest", "x", "--ext-rom", "y"],
            ["--manifest", "x", "--video", "SECAM"],
            ["--manifest", "x", "--joystick", "paddle"],
            ["--manifest", "x", "--drives", "two"],
        ):
            with self.subTest(argv=argv), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as caught:
                    publish_web.parse_args(argv)
                self.assertEqual(caught.exception.code, 2)
        args = publish_web.parse_args(["--manifest", "game.toml"])
        self.assertEqual((args.drives, args.joystick, args.video, args.profile, args.max_size),
                         (1, "keys", None, "release", "64M"))
        self.assertFalse(args.skip_build)
        self.assertEqual(args.pkg, publish_web.DEFAULT_PKG)


if __name__ == "__main__":
    unittest.main()
