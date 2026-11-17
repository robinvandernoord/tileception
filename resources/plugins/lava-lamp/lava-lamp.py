#!/usr/bin/env python3
"""A lava lamp on Tileception's dashboard.

An example of a dashboard plugin with a surface of its own: it speaks MCP on
stdin and stdout like any plugin, and draws its lamp over the Wayland
connection Tileception hands it in WAYLAND_SOCKET. Click the lamp to change
its colours; the card's Shake button calls the plugin's `shake` tool.

Needs pywayland (`pip install pywayland`) and the protocol file
tileception-dashboard-v1.xml, found next to this repository's resources or
in /usr/share/tileception/protocols.
"""

from __future__ import annotations

import json
import math
import mmap
import os
import random
import selectors
import sys
import tempfile
import time
from pathlib import Path

PROTOCOL = "tileception-dashboard-v1.xml"
DASHBOARD = "tileception://dashboard"
# Cells the lamp is worked out in, across; each is drawn as a square block.
CELLS = 48
FRAME_SECONDS = 1 / 24

PALETTES = [
    ((255, 94, 58), (255, 196, 64)),
    ((64, 156, 255), (120, 255, 214)),
    ((214, 92, 255), (255, 128, 180)),
]


def log(message: str) -> None:
    print(f"lava-lamp: {message}", file=sys.stderr, flush=True)


def protocol_file() -> Path:
    here = Path(__file__).resolve()
    for candidate in [
        os.environ.get("TILECEPTION_PROTOCOLS", ""),
        str(here.parent.parent.parent / "protocols"),
        "/usr/share/tileception/protocols",
    ]:
        if candidate and (Path(candidate) / PROTOCOL).is_file():
            return Path(candidate) / PROTOCOL
    raise SystemExit(f"lava-lamp: cannot find {PROTOCOL}")


def load_bindings():
    """Generates pywayland's bindings for the dashboard protocol once."""
    from pywayland.scanner import Protocol

    package = Path(tempfile.gettempdir()) / f"tileception-dashboard-{os.getuid()}" / "dashboard_bindings"
    package.mkdir(parents=True, exist_ok=True)
    (package / "__init__.py").write_text("")
    # The generated module imports wl_surface from a sibling `wayland`.
    (package / "wayland.py").write_text("from pywayland.protocol.wayland import *  # noqa: F401,F403\n")
    protocol = Protocol.parse_file(str(protocol_file()))
    imports = {interface.name: protocol.name for interface in protocol.interface}
    imports["wl_surface"] = "wayland"
    protocol.output(str(package), imports)
    sys.path.insert(0, str(package.parent))
    from dashboard_bindings.tileception_dashboard_v1 import TileceptionDashboardV1

    return TileceptionDashboardV1


class Lamp:
    """Blobs rising and sinking, drawn as metaballs."""

    def __init__(self) -> None:
        self.palette = 0
        self.blobs = [
            [random.uniform(0.2, 0.8), random.uniform(0.1, 0.9), random.uniform(-0.15, 0.15), random.uniform(0.08, 0.14)]
            for _ in range(6)
        ]
        self.last = time.monotonic()

    def shake(self) -> None:
        for blob in self.blobs:
            blob[2] = random.uniform(-0.6, 0.6)

    def step(self) -> None:
        now = time.monotonic()
        elapsed, self.last = min(now - self.last, 0.1), now
        for blob in self.blobs:
            # Warm at the bottom, cool at the top: blobs turn around.
            blob[2] += (0.5 - blob[1]) * 0.4 * elapsed
            blob[2] *= 0.995
            blob[1] = min(max(blob[1] + blob[2] * elapsed, 0.05), 0.95)
            blob[0] += math.sin(now * 0.7 + blob[3] * 40) * 0.02 * elapsed

    def draw(self, pixels: memoryview, width: int, height: int) -> None:
        """Fills ARGB8888 pixels (little endian: B, G, R, A)."""
        cells_x = CELLS
        cell = max(width // cells_x, 1)
        cells_y = max(height // cell, 1)
        low, high = PALETTES[self.palette]
        background = bytes((40, 22, 30, 255))
        stride = width * 4
        aspect = height / max(width, 1)
        for row in range(cells_y):
            y = (row + 0.5) / cells_y
            line = bytearray()
            for column in range(cells_x):
                x = (column + 0.5) / cells_x
                field = 0.0
                for bx, by, _, radius in self.blobs:
                    dx, dy = x - bx, (y - by) * aspect
                    field += radius * radius / (dx * dx + dy * dy + 1e-4)
                if field > 1.0:
                    share = min((field - 1.0) / 2.0, 1.0)
                    colour = [int(low[i] + (high[i] - low[i]) * share) for i in range(3)]
                    texel = bytes((colour[2], colour[1], colour[0], 255))
                else:
                    texel = background
                line += texel * cell
            line += background * (width - len(line) // 4)
            line = bytes(line[:stride])
            for repeat in range(cell):
                y_pixel = row * cell + repeat
                if y_pixel < height:
                    pixels[y_pixel * stride:(y_pixel + 1) * stride] = line
        for y_pixel in range(cells_y * cell, height):
            pixels[y_pixel * stride:(y_pixel + 1) * stride] = background * width


class Surface:
    """The lamp's dashboard item: a wl_surface drawn on frame callbacks."""

    def __init__(self, lamp: Lamp) -> None:
        from pywayland.client import Display
        from pywayland.protocol.wayland import WlCompositor, WlSeat, WlShm

        dashboard_interface = load_bindings()
        self.lamp = lamp
        self.display = Display()
        self.display.connect()
        globals_found: dict[str, tuple[int, int]] = {}
        registry = self.display.get_registry()
        registry.dispatcher["global"] = lambda _, name, interface, version: globals_found.__setitem__(
            interface, (name, version)
        )
        self.display.roundtrip()
        log(f"offered: {', '.join(sorted(globals_found))}")
        def bind(interface: str, cls, wanted: int = 1):
            name, version = globals_found[interface]
            return registry.bind(name, cls, min(version, wanted))

        # Version 4 for damage_buffer and set_buffer_scale.
        self.compositor = bind("wl_compositor", WlCompositor, 4)
        self.shm = bind("wl_shm", WlShm)
        dashboard = bind("tileception_dashboard_v1", dashboard_interface)
        if "wl_seat" in globals_found:
            # Kept: pywayland drops the events of proxies nothing refers to.
            self.seat = bind("wl_seat", WlSeat)
            self.pointer = self.seat.get_pointer()
            self.pointer.dispatcher["button"] = self.clicked
        self.surface = self.compositor.create_surface()
        self.item = dashboard.get_item(self.surface, "lamp")
        self.item.dispatcher["configure"] = self.configured
        self.size = (0, 0, 1)
        self.buffer = None
        self.waiting = False
        self.display.flush()

    def clicked(self, _pointer, _serial, _time, _button, state) -> None:
        if state == 1:
            self.lamp.palette = (self.lamp.palette + 1) % len(PALETTES)

    def configured(self, _item, width: int, height: int, scale: int) -> None:
        self.size = (width, height, max(scale, 1))
        if width > 0 and height > 0:
            self.make_buffer()
            if not self.waiting:
                self.draw()

    def make_buffer(self) -> None:
        from pywayland.protocol.wayland import WlShm

        width, height, scale = self.size
        self.pixel_size = (width * scale, height * scale)
        stride = self.pixel_size[0] * 4
        length = stride * self.pixel_size[1]
        descriptor = os.memfd_create("lava-lamp")
        os.ftruncate(descriptor, length)
        self.memory = mmap.mmap(descriptor, length)
        pool = self.shm.create_pool(descriptor, length)
        os.close(descriptor)
        self.buffer = pool.create_buffer(0, self.pixel_size[0], self.pixel_size[1], stride, WlShm.format.argb8888.value)
        pool.destroy()

    def draw(self, *_ignored) -> None:
        """Draws a frame; the next comes with the frame callback, which
        Tileception sends only while the lamp is on screen."""
        if self.buffer is None:
            self.waiting = False
            return
        started = time.monotonic()
        self.lamp.step()
        self.lamp.draw(memoryview(self.memory), *self.pixel_size)
        self.surface.attach(self.buffer, 0, 0)
        self.surface.set_buffer_scale(self.size[2])
        self.surface.damage_buffer(0, 0, *self.pixel_size)
        callback = self.surface.frame()
        callback.dispatcher["done"] = self.after_frame
        self.waiting = True
        self.surface.commit()
        self.display.flush()
        self.spent = time.monotonic() - started

    def after_frame(self, *_ignored) -> None:
        # Not faster than the lamp needs.
        time.sleep(max(FRAME_SECONDS - self.spent, 0))
        self.draw()


class Plugin:
    """The MCP side: the dashboard resource and the shake tool."""

    def __init__(self, lamp: Lamp) -> None:
        self.lamp = lamp
        self.pending = b""

    def items(self) -> dict:
        return {
            "items": [
                {"type": "surface", "id": "lamp", "title": "Lava lamp", "size": "large"},
                {
                    "type": "card",
                    "id": "controls",
                    "title": "Lava lamp",
                    "detail": "Click the lamp for other colours",
                    "icon": "glyph:flame",
                    "tint": "warm",
                    "enabled": True,
                    "buttons": [
                        {
                            "title": "Shake",
                            "primary": True,
                            "enabled": True,
                            "action": {"type": "tool", "name": "shake"},
                        }
                    ],
                },
            ]
        }

    def answer(self, message: dict) -> dict | None:
        method, params = message.get("method"), message.get("params") or {}
        if method == "initialize":
            return {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}, "resources": {"subscribe": True}},
                "serverInfo": {"name": "lava-lamp", "version": "1.0"},
            }
        if method == "tools/list":
            return {"tools": [{"name": "shake", "description": "Stir the lamp", "inputSchema": {"type": "object"}}]}
        if method == "tools/call" and params.get("name") == "shake":
            self.lamp.shake()
            return {"content": [{"type": "text", "text": "Shaken"}], "structuredContent": {"text": "Shaken"}}
        if method == "resources/list":
            return {"resources": [{"uri": DASHBOARD, "name": "dashboard", "mimeType": "application/json"}]}
        if method == "resources/read" and params.get("uri") == DASHBOARD:
            text = json.dumps(self.items())
            return {"contents": [{"uri": DASHBOARD, "mimeType": "application/json", "text": text}]}
        if method in ("resources/subscribe", "ping"):
            return {}
        return None

    def readable(self) -> bool:
        chunk = os.read(0, 65536)
        if not chunk:
            return False
        self.pending += chunk
        while b"\n" in self.pending:
            line, self.pending = self.pending.split(b"\n", 1)
            if not line.strip():
                continue
            message = json.loads(line)
            if "id" not in message:
                continue
            result = self.answer(message)
            reply = {"jsonrpc": "2.0", "id": message["id"]}
            if result is None:
                reply["error"] = {"code": -32601, "message": f"unsupported: {message.get('method')}"}
            else:
                reply["result"] = result
            sys.stdout.write(json.dumps(reply) + "\n")
            sys.stdout.flush()
        return True


def main() -> None:
    lamp = Lamp()
    plugin = Plugin(lamp)
    surface = None
    if "WAYLAND_SOCKET" in os.environ:
        try:
            surface = Surface(lamp)
        except Exception as error:  # The card still works without the lamp.
            log(f"no lamp: {error}")
    else:
        log("no WAYLAND_SOCKET: set surface = true in [dashboard]")
    selector = selectors.DefaultSelector()
    selector.register(0, selectors.EVENT_READ, "mcp")
    if surface is not None:
        selector.register(surface.display.get_fd(), selectors.EVENT_READ, "wayland")
    while True:
        for key, _ in selector.select():
            if key.data == "mcp" and not plugin.readable():
                return
            if key.data == "wayland" and surface is not None:
                if surface.display.dispatch(block=True) < 0:
                    log("dashboard connection closed")
                    selector.unregister(key.fd)
                    surface = None
                else:
                    surface.display.flush()


if __name__ == "__main__":
    main()
