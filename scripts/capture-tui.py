#!/usr/bin/env python3
"""Capture the real demo TUI through a PTY and rasterize its terminal cells.

Documentation-only dependencies: python3-pyte, python3-pil, fonts-dejavu-core.
No Bluetooth, Redis, personal configuration, or existing workout data is used.
"""
import argparse
import codecs
import fcntl
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time

import pyte
from PIL import Image, ImageDraw, ImageFont

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=Path('target/debug/sunnyfit-ble-cli'))
parser.add_argument('--output', type=Path, default=Path('assets/screenshots/workout-demo.png'))
parser.add_argument('--duration', type=float, default=36)
args = parser.parse_args()
if args.duration <= 0:
    parser.error('--duration must be positive')
columns, rows = 124, 30
screen = pyte.Screen(columns, rows)
stream = pyte.Stream(screen)
decoder = codecs.getincrementaldecoder('utf-8')('replace')
with tempfile.TemporaryDirectory(prefix='sunnyfit-screenshot-') as temp:
    env = dict(os.environ, TERM='xterm-256color', COLORTERM='truecolor', XDG_CONFIG_HOME=temp)
    env.pop('SUNNY_REDIS_URL', None)
    env.pop('NO_COLOR', None)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', rows, columns, 0, 0))
    proc = subprocess.Popen([
        str(args.binary.resolve()), '--demo', '--workout', '--met', '3',
        '--goal-minutes', '1', '--redis-enabled=false', '--output-dir', temp + '/captures',
    ], stdin=slave, stderr=slave, stdout=slave, env=env)
    os.close(slave)
    try:
        end = time.monotonic() + args.duration
        while time.monotonic() < end:
            if proc.poll() is not None:
                raise RuntimeError('TUI exited before capture completed')
            if select.select([master], [], [], 0.1)[0]:
                stream.feed(decoder.decode(os.read(master, 65536)))
        # Freeze the live screen before quit restores the alternate terminal.
        cells = [[screen.buffer[y][x] for x in range(columns)] for y in range(rows)]
        text = '\n'.join(''.join(c.data for c in row) for row in cells)
        assert 'DEMO' in text and 'CADENCE' in text and 'ENERGY' in text
        os.write(master, b'q')
        proc.wait(timeout=10)
        if proc.returncode:
            raise RuntimeError('TUI did not shut down cleanly')
    finally:
        if proc.poll() is None:
            proc.terminate()
            proc.wait(timeout=10)
        os.close(master)

regular = ImageFont.truetype('/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf', 18)
bold = ImageFont.truetype('/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf', 18)
cell_width, cell_height, padding = 11, 22, 24
background, foreground = '#0d1117', '#e6edf3'
colors = {'black': '#000000', 'red': '#cd3131', 'green': '#0dbc79', 'brown': '#e5e510',
          'blue': '#2472c8', 'magenta': '#bc3fbc', 'cyan': '#11a8cd', 'white': '#e5e5e5',
          'brightblack': '#666666', 'brightwhite': '#ffffff'}
def color(value, default):
    if value == 'default':
        return default
    if len(value) == 6 and all(ch in '0123456789abcdefABCDEF' for ch in value):
        return '#' + value
    return colors.get(value, default)
image = Image.new('RGB', (columns * cell_width + padding * 2, rows * cell_height + padding * 2), background)
draw = ImageDraw.Draw(image)
ascent = regular.getmetrics()[0]
for y, row in enumerate(cells):
    for x, cell in enumerate(row):
        fg, bg = color(cell.fg, foreground), color(cell.bg, background)
        if cell.reverse:
            fg, bg = bg, fg
        px, py = padding + x * cell_width, padding + y * cell_height
        if bg != background:
            draw.rectangle((px, py, px + cell_width - 1, py + cell_height - 1), fill=bg)
        if cell.data in '▁▂▃▄▅▆▇█':
            height = round(cell_height * ('▁▂▃▄▅▆▇█'.index(cell.data) + 1) / 8)
            draw.rectangle((px, py + cell_height - height, px + cell_width - 1, py + cell_height - 1), fill=fg)
        elif cell.data.strip():
            draw.text((px, py + ascent), cell.data, font=bold if cell.bold else regular, fill=fg, anchor='ls')
        if cell.underscore:
            draw.line((px, py + cell_height - 2, px + cell_width - 1, py + cell_height - 2), fill=fg)
args.output.parent.mkdir(parents=True, exist_ok=True)
image.save(args.output, optimize=True)
print('Saved screenshot of the live demo terminal:', args.output)
