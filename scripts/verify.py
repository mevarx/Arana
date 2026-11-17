#!/usr/bin/env python3
"""Run real local gates and preserve their output (no fabricated pass results)."""
from __future__ import annotations
import argparse
import datetime as dt
import json
import re
from pathlib import Path
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]

def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--release', action='store_true', help='also build release binaries')
    parser.add_argument('--out', type=Path, default=ROOT / 'artifacts' / 'verification')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    cargo = shutil.which('cargo')
    if not cargo:
        print('cargo was not found on PATH', file=sys.stderr)
        return 2
    commands = [
        ('python-tests', [sys.executable, '-m', 'unittest', 'discover', '-s', 'tests', '-p', 'test_*.py']),
        ('python-acceptance-tests', [sys.executable, '-m', 'unittest', 'discover', '-s', 'scripts/tests', '-p', 'test_*.py']),
        ('fmt', [cargo, 'fmt', '--all', '--', '--check']),
        ('clippy', [cargo, 'clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings']),
        ('tests', [cargo, 'test', '--workspace', '--locked']),
    ]
    if args.release:
        commands.append(('release', [cargo, 'build', '--workspace', '--release', '--locked']))
    report = {'started_at': dt.datetime.now(dt.timezone.utc).isoformat(), 'gates': [],
              'scope': 'Local automated gates. Ignored live tests and external acceptance are NOT implied to pass.'}
    for name, cmd in commands:
        started = time.monotonic()
        p = subprocess.Popen(cmd, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        timed_out = False
        try:
            output, _ = p.communicate(timeout=600)
        except subprocess.TimeoutExpired:
            timed_out = True
            if sys.platform == 'win32':
                subprocess.run(['taskkill', '/PID', str(p.pid), '/T', '/F'], capture_output=True)
            p.kill()
            output, _ = p.communicate()
        log = args.out / f'{name}.log'
        log.write_bytes(output)
        row = {'name': name, 'command': cmd, 'exit_code': p.returncode,
               'timed_out': timed_out, 'seconds': round(time.monotonic()-started, 3),
               'log': str(log.resolve()), 'passed': p.returncode == 0 and not timed_out}
        if name == 'tests':
            totals = re.findall(rb'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored', output)
            row['test_counts'] = {key: sum(int(t[index]) for t in totals) for index, key in enumerate(('passed', 'failed', 'ignored'))}
        report['gates'].append(row)
        print(json.dumps(row, ensure_ascii=False), flush=True)
    report['passed'] = all(g['passed'] for g in report['gates'])
    (args.out / 'verification.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
    return 0 if report['passed'] else 1

if __name__ == '__main__':
    raise SystemExit(main())
